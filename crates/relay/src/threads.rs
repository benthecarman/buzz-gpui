//! Thread views (`threads.ts`, `thread-window.ts`, `thread-follows.ts`;
//! spec §5c).
//!
//! Everything here is pure and synchronous: [`ThreadState`] never performs
//! I/O. It hands out the next read as a [`ThreadRead`] and takes the result
//! back through [`ThreadState::complete`], so the session actor stays the
//! only owner of the reader, the access epoch and the outbox.
//!
//! # Driving a thread view from the session
//!
//! One [`ThreadState`] per open thread panel, keyed by the session (the
//! reference caps owned views at 64 across threads and filtered views).
//!
//! 1. **Open.** Build a [`ThreadConfig`] with the relay's NIP-11 `self` as
//!    both `relay_author` and `signing_authority`, the viewer's key, and
//!    `authority_host = thread_authority(origin.websocket()).ok()`. Pass the
//!    session's shared [`MessageClock`] and, as `seed`, the message if the
//!    session already holds it (a retained window or another thread's
//!    [`ThreadState::event`]), but only when the channel is readable. Then
//!    call [`ThreadState::receive`] with every recently observed event and
//!    [`ThreadState::set_local`] with the outbox's signed operations (the
//!    same list `local_changed` gives timelines).
//! 2. **Reads.** [`ThreadState::refresh`] and [`ThreadState::load_more`]
//!    return the first read of a run, or `None` when nothing should start.
//!    Run `read.filters` through `Reader::read` with `read.options`
//!    (foreground, not fresh), in a task that reports back to the actor.
//!    Before reporting, if the channel is no longer readable, report a
//!    `ReadError` (`Unavailable`) instead. Back on the actor, call
//!    [`ThreadState::complete`] with the read's `token` and the result.
//!    Thread windows are verified inside `complete`, before anything is
//!    admitted, so the session must **not** pass a strict page's events to
//!    other stores itself.
//! 3. **Completion.** [`Completion::admit`] lists verified events the
//!    session should now run through its `accept()` pipeline (profiles,
//!    outbox echo, timelines and every thread's `receive`). It is empty for
//!    a page whose bounds failed, and an exact target's overlays are held
//!    back until the target folded completely. [`Completion::denied`] means
//!    the relay refused the read (401/403): deny the channel in discovery
//!    and purge as for any revocation; this view already revoked itself.
//!    [`Completion::next`] is the next read of the same run; start it like
//!    the first.
//! 4. **Live and local.** Call [`ThreadState::receive`] for every verified
//!    batch `accept()` sees, and [`ThreadState::set_local`] whenever the
//!    outbox changes. Neither starts a read.
//! 5. **Lifecycle.** On revocation of the channel call
//!    [`ThreadState::revoke`] (and [`ThreadState::regrant`] if access
//!    returns while the view is open); on a cache clear,
//!    [`ThreadState::clear`]; when the live route for the channel is
//!    re-established, [`ThreadState::refresh`] to repair the retained range.
//!    Each of these fences reads in flight: a stale token's result is
//!    ignored by `complete`, so the session needs no extra epoch check.
//!    Dropping the state disposes the view.
//! 6. **Output.** [`ThreadState::take_snapshot`] returns the new
//!    [`ThreadSnapshot`] once after anything visible changed; send it on
//!    the view's watch channel. Unchanged rows keep their `Arc`.
//!
//! The reply tree is built here ([`ThreadSnapshot::tree`], from
//! [`reply_tree`]) and rebuilt only when the replies change, so the UI can
//! render nesting without walking ancestry itself.
//!
//! Thread follows ([`ThreadFollows`]) are a separate model: the session
//! loads it from `Store::partition_pref(THREAD_FOLLOWS_PREF)`, calls
//! [`ThreadFollows::observe`] with accepted batches and saves
//! [`ThreadFollows::to_json`] after [`ThreadFollows::follow`].

mod follows;
mod window;

#[cfg(test)]
mod tests;

pub use follows::{
    InvalidThread, THREAD_FOLLOW_LIMIT, THREAD_FOLLOWS_PREF, ThreadFollows, follow_key,
};
pub use window::{
    CONTENT_KINDS, DEPTH_LIMIT, EVIDENCE_LIMIT, INITIAL_WINDOW_SIZE, MAX_CURSOR_TIME, PAGE_SIZE,
    STRICT_WINDOW_KINDS, TARGET_OVERLAY_KINDS, TOMBSTONE_KINDS, ThreadBounds, ThreadCursor,
    ThreadWindowError, WINDOW_FIELDS, is_canonical_channel, legacy_filter, message_filter,
    overlays_filter, thread_authority, thread_binding, thread_bounds, tombstones_filter,
    verify_thread_windows, window_filter,
};

use crate::event::{Verified, kinds};
use crate::fold::{
    Delivery, FoldOptions, LocalDelivery, MessageClock, MessageProjection, MessageRow, ReplyTree,
    reply_tree, thread_reference,
};
use crate::reader::{Events, ReadError, ReadErrorKind, ReadOptions};
use crate::socket::Filter;
use nostr::prelude::{EventId, PublicKey};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use window::is_hex64;

/// Pages one view may hold: 10 + 9 × 50 = 460 strict replies at most.
pub const MAX_PAGES: usize = 10;
/// Retained evidence cap. Over it the view clears rather than evicting a
/// deletion and resurrecting what it deleted.
pub const MAX_EVENTS: usize = 2000;
pub const MAX_BYTES: usize = 4 << 20;

/// Events that only describe others (`threads.ts:18`).
const AUX: [u16; 6] = [
    kinds::DELETION,
    kinds::REACTION,
    kinds::MODERATOR_DELETE,
    kinds::EDIT,
    kinds::THREAD_SUMMARY,
    kinds::CHANNEL_WINDOW,
];
/// Private product feedback never enters a view (`session.ts`).
const PRODUCT_FEEDBACK: u16 = 42000;

const MEMORY_LIMIT: &str =
    "Thread view exceeded its memory limit. Refresh to read from the beginning.";
const ROOT_UNAVAILABLE: &str = "The original thread message is unavailable.";
const SELECTED_UNAVAILABLE: &str = "The selected message is unavailable.";
const NOT_ADVANCING: &str = "Thread pagination did not advance. Refresh to try again.";
const EVIDENCE_EXCEEDED: &str = "Selected message exceeded its evidence limit";
const INTERRUPTED: &str = "Thread read interrupted. Refresh to continue.";
const NO_ACCESS: &str = "This channel is no longer available.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    Idle,
    Loading,
    Ready,
    Error,
}

/// How the loaded range grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Strict signed windows, newest first.
    Older,
    /// The legacy bounded walk, oldest first.
    Forward,
}

/// The read in flight or that last failed, apart from the direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadKind {
    Older,
    Refresh,
}

/// The exact-navigation target's state (§5c.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetStatus {
    Loading,
    Ready,
    /// Missing, deleted or not readable.
    Unavailable,
    Error,
}

/// What a thread panel renders.
#[derive(Clone, Debug)]
pub struct ThreadSnapshot {
    pub status: ThreadStatus,
    /// `None` until loaded, and when the original message is unavailable.
    pub root: Option<Arc<MessageRow>>,
    /// Every other row in rendered order.
    pub replies: Arc<[Arc<MessageRow>]>,
    /// The replies nested by signed ancestry under the root.
    pub tree: Arc<ReplyTree>,
    pub error: Option<String>,
    /// Continuation is possible; not a claim about the thread's size.
    pub can_load_more: bool,
    /// More exists but the view holds its maximum.
    pub limited: bool,
    pub direction: Option<Direction>,
    pub read_kind: Option<ReadKind>,
    /// Exact mode only: the selected message, folded on its own evidence.
    pub target: Option<Arc<MessageRow>>,
    /// Exact mode only.
    pub target_status: Option<TargetStatus>,
}

impl ThreadSnapshot {
    /// Before anything loaded.
    pub fn idle(exact: bool) -> Self {
        Self {
            status: ThreadStatus::Idle,
            root: None,
            replies: Arc::new([]),
            tree: Arc::default(),
            error: None,
            can_load_more: false,
            limited: false,
            direction: None,
            read_kind: None,
            target: None,
            target_status: exact.then_some(TargetStatus::Loading),
        }
    }

    fn same(&self, other: &Self) -> bool {
        let same_row = |a: &Option<Arc<MessageRow>>, b: &Option<Arc<MessageRow>>| match (a, b) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        self.status == other.status
            && same_row(&self.root, &other.root)
            && Arc::ptr_eq(&self.replies, &other.replies)
            && Arc::ptr_eq(&self.tree, &other.tree)
            && self.error == other.error
            && self.can_load_more == other.can_load_more
            && self.limited == other.limited
            && self.direction == other.direction
            && self.read_kind == other.read_kind
            && same_row(&self.target, &other.target)
            && self.target_status == other.target_status
    }
}

#[derive(Clone, Debug)]
pub struct ThreadConfig {
    pub channel: String,
    /// The message the view was opened on: the root, or a reply in exact
    /// mode.
    pub message_id: EventId,
    /// Fold the selected message on its own evidence first (deep links
    /// and reveal of a reply, §5c.5).
    pub exact: bool,
    /// NIP-11 `self`: the only signer of 39005 and 39007.
    pub relay_author: PublicKey,
    /// The key whose messages are attributed through `actor`/`p` tags.
    pub signing_authority: Option<PublicKey>,
    pub viewer: PublicKey,
    /// [`thread_authority`] of the relay origin. `None` makes every strict
    /// page invalid instead of falling back.
    pub authority_host: Option<String>,
}

/// Which read of a run this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadStep {
    /// Exact mode: the selected message by id.
    Target,
    /// Exact mode: overlays of the selected message.
    Overlays,
    /// Exact mode: deletions of those overlays.
    Tombstones,
    /// The opened message by id, to find its root.
    Selected,
    /// The root by id, once per strict run.
    Root,
    /// A strict `thread_window` page.
    Window,
    /// A legacy page, batched with the root lookup.
    Legacy,
}

/// A read for the session to run.
#[derive(Clone, Debug)]
pub struct ThreadRead {
    pub token: u64,
    pub step: ReadStep,
    pub filters: Vec<Filter>,
    pub options: ReadOptions,
}

/// What the session does after [`ThreadState::complete`].
#[derive(Debug, Default)]
pub struct Completion {
    /// Verified events to run through the session's `accept()` pipeline.
    pub admit: Vec<Verified>,
    /// The next read of this run.
    pub next: Option<ThreadRead>,
    /// The relay refused the read: deny the channel.
    pub denied: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Strict, until the first page shows whether the relay signs windows.
    Probe,
    /// Strict, proven.
    Older,
    Legacy,
}

/// Fields a publish carries over unless changed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fields {
    status: ThreadStatus,
    error: Option<String>,
    can_load_more: bool,
    limited: bool,
    direction: Option<Direction>,
    read_kind: Option<ReadKind>,
}

struct Pending {
    token: u64,
    step: ReadStep,
    filters: Vec<Filter>,
}

/// One `run(replace)` of the reference, suspended between reads.
struct Run {
    target_pages: usize,
    next_cursor: Option<ThreadCursor>,
    next_pages: usize,
    fetched: HashMap<EventId, Verified>,
    /// The verified root of this run, prepended to every strict page.
    root: Vec<Verified>,
    page: usize,
    selected: Option<Verified>,
    pending: Option<Pending>,
}

type EventMap = HashMap<EventId, Verified>;

/// One thread view (`createThreadView`). See the module docs for how the
/// session drives it.
pub struct ThreadState {
    config: ThreadConfig,
    viewer_hex: String,
    relay_hex: String,
    projection: MessageProjection,
    mode: Mode,
    root_id: Option<EventId>,
    root_unavailable: bool,
    target_status: Option<TargetStatus>,
    /// Retained fetched and live evidence. Bounded reads never retract it.
    remote: EventMap,
    /// Exact overlays read but not yet safe to show.
    staged: EventMap,
    local: Vec<(Verified, LocalDelivery)>,
    cursor: Option<ThreadCursor>,
    pages: usize,
    run: Option<Run>,
    /// A refresh was asked for while a run was in flight.
    again: bool,
    accessible: bool,
    next_token: u64,
    fields: Fields,
    tree_root: Option<EventId>,
    current: Arc<ThreadSnapshot>,
    changed: bool,
}

impl ThreadState {
    pub fn new(config: ThreadConfig, clock: Arc<MessageClock>, seed: Option<&Verified>) -> Self {
        let projection = MessageProjection::new(
            config.channel.clone(),
            config.relay_author,
            FoldOptions {
                include_replies: true,
                signing_authority: config.signing_authority,
                ..Default::default()
            },
        )
        .with_clock(clock);
        let mode = if is_canonical_channel(&config.channel) {
            Mode::Probe
        } else {
            Mode::Legacy
        };
        let exact = config.exact;
        let mut state = Self {
            viewer_hex: config.viewer.to_hex(),
            relay_hex: config.relay_author.to_hex(),
            config,
            projection,
            mode,
            root_id: None,
            root_unavailable: false,
            target_status: exact.then_some(TargetStatus::Loading),
            remote: HashMap::new(),
            staged: HashMap::new(),
            local: Vec::new(),
            cursor: None,
            pages: 0,
            run: None,
            again: false,
            accessible: true,
            next_token: 1,
            fields: Fields {
                status: ThreadStatus::Idle,
                error: None,
                can_load_more: false,
                limited: false,
                direction: None,
                read_kind: None,
            },
            tree_root: None,
            current: Arc::new(ThreadSnapshot::idle(exact)),
            changed: false,
        };
        if let Some(seed) = seed
            && is_content(seed)
            && state.in_channel(seed)
        {
            let root = thread_reference(seed).map_or(seed.id, |reference| reference.root_id);
            state.root_id = Some(root);
            if seed.id == root {
                state.remote.insert(seed.id, seed.clone());
            }
            state.publish();
        }
        state
    }

    pub fn channel(&self) -> &str {
        &self.config.channel
    }

    pub fn message_id(&self) -> &EventId {
        &self.config.message_id
    }

    pub fn exact(&self) -> bool {
        self.config.exact
    }

    /// The thread root, once known.
    pub fn root_id(&self) -> Option<&EventId> {
        self.root_id.as_ref()
    }

    /// A read of this view is waiting for its result.
    pub fn loading(&self) -> bool {
        self.run.is_some()
    }

    pub fn snapshot(&self) -> Arc<ThreadSnapshot> {
        self.current.clone()
    }

    /// The snapshot, once after each visible change.
    pub fn take_snapshot(&mut self) -> Option<Arc<ThreadSnapshot>> {
        std::mem::take(&mut self.changed).then(|| self.current.clone())
    }

    /// A retained or staged event, so the session can check a live overlay
    /// or seed another view without a read.
    pub fn event(&self, id: &EventId) -> Option<&Verified> {
        self.remote.get(id).or_else(|| self.staged.get(id))
    }

    /// Reloads the whole retained range from the newest page.
    pub fn refresh(&mut self) -> Option<ThreadRead> {
        self.start(true)
    }

    /// Loads the next page, if the last run left one to load.
    pub fn load_more(&mut self) -> Option<ThreadRead> {
        self.start(false)
    }

    /// Feeds back the result of a read this view handed out. Results of
    /// fenced reads are ignored.
    pub fn complete(&mut self, token: u64, result: Result<Events, ReadError>) -> Completion {
        let mut out = Completion::default();
        let Some(run) = self.run.as_mut() else {
            return out;
        };
        if run
            .pending
            .as_ref()
            .is_none_or(|pending| pending.token != token)
        {
            return out;
        }
        let Pending { step, filters, .. } = run.pending.take().expect("pending read");
        let events = match result {
            Err(error) if error.kind == ReadErrorKind::Denied => {
                out.denied = true;
                self.revoke();
                return out;
            }
            Err(error) => {
                out.next = self.fail(error.message);
                return out;
            }
            Ok(events) => events,
        };
        // The exact reader refuses a capped raw response: a fold over a
        // truncated overlay set is not safe (`session.ts:1822-1829`).
        if self.config.exact
            && events.len() >= EVIDENCE_LIMIT
            && !filters
                .iter()
                .any(|filter| filter.contains_key("depth_limit"))
        {
            out.next = self.fail(EVIDENCE_EXCEEDED.into());
            return out;
        }
        let events: Vec<Verified> = events
            .iter()
            .filter(|event| event.kind_u16() != PRODUCT_FEEDBACK)
            .cloned()
            .collect();
        out.next = match step {
            ReadStep::Target => self.on_target(events, &mut out.admit),
            ReadStep::Overlays => self.on_overlays(events, &mut out.admit),
            ReadStep::Tombstones => self.on_tombstones(events, &mut out.admit),
            ReadStep::Selected => {
                out.admit.clone_from(&events);
                self.on_selected(events)
            }
            ReadStep::Root => {
                out.admit.clone_from(&events);
                self.on_root(events)
            }
            ReadStep::Window => self.on_window(&filters, events, &mut out.admit),
            ReadStep::Legacy => {
                out.admit.clone_from(&events);
                self.on_page(events, None)
            }
        };
        out
    }

    /// Takes verified live or read evidence that belongs to this thread.
    /// Returns whether this call changed the snapshot.
    pub fn receive(&mut self, events: &[Verified]) -> bool {
        if !self.accessible || (self.root_id.is_none() && !self.config.exact) {
            return false;
        }
        let incoming: Vec<Verified> = self
            .related(events)
            .into_iter()
            .filter(|event| !self.remote.contains_key(&event.id))
            .collect();
        if incoming.is_empty() {
            return false;
        }
        let before = self.current.clone();
        let all = union(&self.remote, incoming);
        if self.retain(all, true) {
            self.publish();
        }
        !Arc::ptr_eq(&before, &self.current)
    }

    /// Replaces the outbox operations this view may show. Failed replies
    /// stay visible for retry; failed edits and reactions do not apply.
    pub fn set_local(&mut self, operations: &[(Verified, LocalDelivery)]) {
        self.local = operations.to_vec();
        self.publish();
    }

    /// Stops the run in flight and keeps what was loaded.
    pub fn interrupt(&mut self) {
        self.purge(false);
    }

    /// Stops the run in flight and forgets everything (cache clear).
    pub fn clear(&mut self) {
        self.purge(true);
    }

    /// The channel became unreadable: stop, forget, and refuse new runs
    /// until [`regrant`](Self::regrant).
    pub fn revoke(&mut self) {
        self.accessible = false;
        self.purge(false);
    }

    /// The channel is readable again. Nothing is reloaded until a refresh.
    pub fn regrant(&mut self) {
        self.accessible = true;
    }

    // ----- runs -----

    fn start(&mut self, replace: bool) -> Option<ThreadRead> {
        if self.run.is_some() {
            if replace {
                self.again = true;
            }
            return None;
        }
        if !self.accessible {
            self.purge(false);
            return None;
        }
        if !replace && (!self.fields.can_load_more || self.fields.limited) {
            return None;
        }
        self.run = Some(Run {
            target_pages: if replace { self.pages.max(1) } else { 1 },
            next_cursor: if replace { None } else { self.cursor.clone() },
            next_pages: if replace { 0 } else { self.pages },
            fetched: HashMap::new(),
            root: Vec::new(),
            page: 0,
            selected: None,
            pending: None,
        });
        // A repair keeps an already-shown target; only a new or missing one
        // waits for its fold.
        if self.config.exact && replace && self.target_status != Some(TargetStatus::Ready) {
            self.target_status = Some(TargetStatus::Loading);
        }
        self.fields.status = ThreadStatus::Loading;
        self.fields.error = None;
        self.fields.read_kind = Some(if replace {
            ReadKind::Refresh
        } else {
            ReadKind::Older
        });
        self.publish_status();
        let selected = message_filter(&self.config.channel, &self.config.message_id);
        if self.config.exact && replace {
            self.issue(ReadStep::Target, vec![selected])
        } else if self.root_id.is_none() {
            self.issue(ReadStep::Selected, vec![selected])
        } else {
            self.begin_root()
        }
    }

    fn issue(&mut self, step: ReadStep, filters: Vec<Filter>) -> Option<ThreadRead> {
        let run = self.run.as_mut()?;
        let token = self.next_token;
        self.next_token += 1;
        run.pending = Some(Pending {
            token,
            step,
            filters: filters.clone(),
        });
        Some(ThreadRead {
            token,
            step,
            filters,
            options: ReadOptions::default(),
        })
    }

    fn begin_root(&mut self) -> Option<ThreadRead> {
        let root = self.root_id?;
        if self.mode == Mode::Legacy {
            return self.begin_page();
        }
        // Strict reads cannot batch an id lookup with a window. Revalidate
        // the root once per run, not once per page of a repair.
        let filter = message_filter(&self.config.channel, &root);
        self.issue(ReadStep::Root, vec![filter])
    }

    fn begin_page(&mut self) -> Option<ThreadRead> {
        let root = self.root_id?;
        let run = self.run.as_ref()?;
        let channel = &self.config.channel;
        let cursor = run.next_cursor.as_ref();
        match self.mode {
            Mode::Legacy => {
                let filters = vec![
                    message_filter(channel, &root),
                    legacy_filter(channel, &root, cursor),
                ];
                self.issue(ReadStep::Legacy, filters)
            }
            Mode::Probe | Mode::Older => {
                let limit = if run.next_pages == 0 {
                    INITIAL_WINDOW_SIZE
                } else {
                    PAGE_SIZE
                };
                let filter = window_filter(channel, &root, limit, cursor);
                self.issue(ReadStep::Window, vec![filter])
            }
        }
    }

    fn on_target(
        &mut self,
        events: Vec<Verified>,
        admit: &mut Vec<Verified>,
    ) -> Option<ThreadRead> {
        // The immutable selected content is admitted at once, like any read;
        // only its separately fetched overlays wait for the whole fold.
        admit.extend(events.iter().cloned());
        let Some(event) = self.selected_in(&events) else {
            self.target_status = Some(TargetStatus::Unavailable);
            self.fields.status = ThreadStatus::Ready;
            self.fields.can_load_more = false;
            self.publish();
            return self.end_run();
        };
        self.root_id = Some(thread_reference(&event).map_or(event.id, |r| r.root_id));
        let all = union(&self.remote, [event.clone()]);
        if !self.retain(all, true) {
            return None;
        }
        self.run.as_mut()?.selected = Some(event);
        let filter = overlays_filter(&self.config.message_id);
        self.issue(ReadStep::Overlays, vec![filter])
    }

    fn on_overlays(
        &mut self,
        events: Vec<Verified>,
        admit: &mut Vec<Verified>,
    ) -> Option<ThreadRead> {
        self.staged = self
            .related(&events)
            .into_iter()
            .map(|event| (event.id, event))
            .collect();
        let all = union(&self.remote, self.staged.values().cloned());
        if !self.retain(all.clone(), false) {
            return None;
        }
        let mut overlays: Vec<EventId> = all
            .values()
            .filter(|event| {
                AUX.contains(&event.kind_u16())
                    && targets(event).any(|target| target == self.config.message_id)
            })
            .map(|event| event.id)
            .collect();
        if overlays.is_empty() {
            return self.finish_target(admit);
        }
        overlays.sort();
        self.issue(ReadStep::Tombstones, vec![tombstones_filter(&overlays)])
    }

    fn on_tombstones(
        &mut self,
        events: Vec<Verified>,
        admit: &mut Vec<Verified>,
    ) -> Option<ThreadRead> {
        for event in self.related(&events) {
            self.staged.insert(event.id, event);
        }
        let all = union(&self.remote, self.staged.values().cloned());
        if !self.retain(all, false) {
            return None;
        }
        self.finish_target(admit)
    }

    /// Admits the target with its complete overlay closure at once.
    fn finish_target(&mut self, admit: &mut Vec<Verified>) -> Option<ThreadRead> {
        let selected = self.run.as_ref()?.selected.clone()?;
        let mut accepted = vec![selected];
        accepted.extend(self.staged.values().cloned());
        admit.extend(accepted.iter().cloned());
        let related = self.related(&accepted);
        let all = union(&self.remote, related);
        if !self.retain(all, true) {
            return None;
        }
        self.staged.clear();
        self.target_status = Some(TargetStatus::Ready);
        self.publish();
        if self.current.target_status != Some(TargetStatus::Ready) {
            // Folded away: deleted by evidence already known.
            self.fields.status = ThreadStatus::Ready;
            self.fields.can_load_more = false;
            self.publish();
            return self.end_run();
        }
        self.begin_root()
    }

    fn on_selected(&mut self, events: Vec<Verified>) -> Option<ThreadRead> {
        let Some(event) = self.selected_in(&events) else {
            return self.fail(SELECTED_UNAVAILABLE.into());
        };
        self.root_id = Some(thread_reference(&event).map_or(event.id, |r| r.root_id));
        self.begin_root()
    }

    fn on_root(&mut self, events: Vec<Verified>) -> Option<ThreadRead> {
        if !self.has_root(&events) {
            return self.root_missing();
        }
        let related = self.related(&events);
        let all = union(&self.remote, related);
        if !self.retain(all, true) {
            return None;
        }
        self.run.as_mut()?.root = events;
        self.begin_page()
    }

    fn on_window(
        &mut self,
        filters: &[Filter],
        events: Vec<Verified>,
        admit: &mut Vec<Verified>,
    ) -> Option<ThreadRead> {
        let verified = verify_thread_windows(
            filters,
            &events,
            self.config.authority_host.as_deref(),
            &self.viewer_hex,
            &self.relay_hex,
        );
        match verified {
            Err(ThreadWindowError::Missing { legacy_rows: true }) if self.mode == Mode::Probe => {
                // A 200 without bounds is an old relay, not support. Its rows
                // never entered the session; restart without a reverse cursor.
                self.mode = Mode::Legacy;
                let run = self.run.as_mut()?;
                run.next_cursor = None;
                run.next_pages = 0;
                self.begin_page()
            }
            Err(error) => self.fail(error.to_string()),
            Ok(Some(bounds)) if bounds.len() == 1 => {
                self.mode = Mode::Older;
                let page: Vec<Verified> = events
                    .into_iter()
                    .filter(|event| event.kind_u16() != kinds::THREAD_WINDOW)
                    .collect();
                admit.extend(page.iter().cloned());
                let mut response = self.run.as_ref()?.root.clone();
                response.extend(page);
                let bounds = bounds.into_iter().next();
                self.on_page(response, bounds)
            }
            Ok(_) => self.fail(ThreadWindowError::Invalid.to_string()),
        }
    }

    fn on_page(
        &mut self,
        events: Vec<Verified>,
        bounds: Option<ThreadBounds>,
    ) -> Option<ThreadRead> {
        if !self.has_root(&events) {
            // Hide unavailable history without dropping known tombstones.
            return self.root_missing();
        }
        let root = self.root_id?;
        // Count traversal rows before presentation filtering: the relay may
        // return other content kinds, and aux does not use the page limit.
        let mut replies: Vec<&Verified> = events
            .iter()
            .filter(|event| {
                !AUX.contains(&event.kind_u16())
                    && event.id != root
                    && self.in_channel(event)
                    && thread_reference(event).is_some_and(|r| r.root_id == root)
            })
            .collect();
        replies.sort_by(|a, b| {
            a.created_at_secs()
                .cmp(&b.created_at_secs())
                .then_with(|| a.id.cmp(&b.id))
        });
        let related = self.related(&events);
        let run = self.run.as_mut()?;
        if bounds.is_none()
            && let Some(cursor) = &run.next_cursor
            && replies.iter().any(|event| cursor.cmp_event(event).is_ge())
        {
            return self.fail(NOT_ADVANCING.into());
        }
        run.next_cursor = match &bounds {
            Some(bounds) => bounds.cursor.clone(),
            None => replies
                .last()
                .map(|event| ThreadCursor::of(event))
                .or_else(|| run.next_cursor.take()),
        };
        run.next_pages += 1;
        for event in related {
            run.fetched.insert(event.id, event);
        }
        if over_budget(run.fetched.values()) {
            let fetched = std::mem::take(&mut run.fetched);
            self.retain(fetched, true);
            return None;
        }
        // Only signed bounds prove exhaustion. A short legacy page may be
        // filtered after its limit, so only an empty one ends it.
        let more = bounds.map_or(!replies.is_empty(), |bounds| bounds.has_more);
        run.page += 1;
        if more && run.page < run.target_pages {
            return self.begin_page();
        }
        self.finish(more)
    }

    fn finish(&mut self, more: bool) -> Option<ThreadRead> {
        let run = self.run.as_mut()?;
        let fetched = std::mem::take(&mut run.fetched);
        let next_cursor = run.next_cursor.take();
        let next_pages = run.next_pages;
        // A bounded response cannot retract observed evidence: keep known
        // edits, tombstones and live rows a later page omits.
        let all = union(&self.remote, fetched.into_values());
        if !self.retain(all, true) {
            return None;
        }
        self.root_unavailable = false;
        self.cursor = next_cursor;
        self.pages = next_pages;
        self.fields = Fields {
            status: ThreadStatus::Ready,
            error: None,
            read_kind: None,
            direction: Some(if self.mode == Mode::Older {
                Direction::Older
            } else {
                Direction::Forward
            }),
            can_load_more: more && self.pages < MAX_PAGES,
            limited: more && self.pages >= MAX_PAGES,
        };
        self.publish();
        self.end_run()
    }

    fn root_missing(&mut self) -> Option<ThreadRead> {
        self.root_unavailable = true;
        self.cursor = None;
        self.pages = 0;
        self.fields.can_load_more = false;
        self.publish();
        self.fail(ROOT_UNAVAILABLE.into())
    }

    fn fail(&mut self, message: String) -> Option<ThreadRead> {
        if self.target_status == Some(TargetStatus::Loading) {
            self.target_status = Some(TargetStatus::Error);
        }
        self.fields.status = ThreadStatus::Error;
        self.fields.error = Some(message);
        self.publish_status();
        self.end_run()
    }

    fn end_run(&mut self) -> Option<ThreadRead> {
        self.run = None;
        self.staged.clear();
        if std::mem::take(&mut self.again) {
            return self.start(true);
        }
        None
    }

    fn purge(&mut self, clear: bool) {
        self.run = None;
        self.again = false;
        self.staged.clear();
        if self.config.exact {
            self.target_status = Some(TargetStatus::Loading);
        }
        if clear || !self.accessible {
            self.remote.clear();
            self.root_id = None;
            self.cursor = None;
            self.pages = 0;
        }
        self.fields.status = ThreadStatus::Idle;
        self.fields.can_load_more = false;
        self.fields.limited = false;
        self.fields.read_kind = None;
        self.fields.error = Some(
            if self.accessible {
                INTERRUPTED
            } else {
                NO_ACCESS
            }
            .into(),
        );
        self.publish();
    }

    /// Commits `events` as the retained evidence (when `commit`), or clears
    /// the view when they are over budget. Nothing is silently evicted.
    fn retain(&mut self, events: EventMap, commit: bool) -> bool {
        if over_budget(events.values()) {
            self.run = None;
            self.again = false;
            self.remote.clear();
            self.staged.clear();
            self.root_id = None;
            self.cursor = None;
            self.pages = 0;
            if self.config.exact {
                self.target_status = Some(TargetStatus::Error);
            }
            self.fields.status = ThreadStatus::Error;
            self.fields.limited = true;
            self.fields.can_load_more = false;
            self.fields.error = Some(MEMORY_LIMIT.into());
            self.publish();
            return false;
        }
        if commit {
            self.remote = events;
        }
        true
    }

    // ----- selection -----

    fn in_channel(&self, event: &Verified) -> bool {
        event.has_tag("h", &self.config.channel)
    }

    fn selected_in(&self, events: &[Verified]) -> Option<Verified> {
        events
            .iter()
            .find(|event| {
                event.id == self.config.message_id && is_content(event) && self.in_channel(event)
            })
            .cloned()
    }

    fn has_root(&self, events: &[Verified]) -> bool {
        events.iter().any(|event| {
            Some(event.id) == self.root_id
                && is_content(event)
                && self.in_channel(event)
                && thread_reference(event).is_none()
        })
    }

    /// The thread's rows in `events`, plus two hops of overlays naming a
    /// known id, so a deletion of a reaction comes along (`threads.ts:119-150`).
    fn related(&self, events: &[Verified]) -> Vec<Verified> {
        let exact = self.config.exact;
        if self.root_id.is_none() && !exact {
            return Vec::new();
        }
        let mut taken: HashSet<EventId> = HashSet::new();
        let mut result = Vec::new();
        for event in events {
            let kind = event.kind_u16();
            if AUX.contains(&kind) || kind == kinds::THREAD_WINDOW || !self.in_channel(event) {
                continue;
            }
            let belongs = Some(event.id) == self.root_id
                || (exact && event.id == self.config.message_id)
                || (self.root_id.is_some()
                    && thread_reference(event).map(|r| r.root_id) == self.root_id);
            if belongs && taken.insert(event.id) {
                result.push(event.clone());
            }
        }
        let known = |id: &EventId, taken: &HashSet<EventId>| {
            taken.contains(id)
                || self.remote.contains_key(id)
                || self.staged.contains_key(id)
                || (exact && *id == self.config.message_id)
        };
        for _ in 0..2 {
            for event in events {
                if !AUX.contains(&event.kind_u16()) || taken.contains(&event.id) {
                    continue;
                }
                if targets(event).any(|target| known(&target, &taken)) {
                    taken.insert(event.id);
                    result.push(event.clone());
                }
            }
        }
        result
    }

    // ----- output -----

    /// Refolds and republishes (`threads.ts:173-240`).
    fn publish(&mut self) {
        let exact = self.config.exact;
        let operations: &[(Verified, LocalDelivery)] =
            if self.accessible { &self.local } else { &[] };
        let candidates: Vec<Verified> = operations
            .iter()
            .filter(|(event, delivery)| delivery.status != Delivery::Failed || is_content(event))
            .map(|(event, _)| event.clone())
            .collect();
        let local = self.related(&candidates);
        let mut inputs: HashMap<EventId, &Verified> =
            self.remote.iter().map(|(id, event)| (*id, event)).collect();
        for event in &local {
            inputs.insert(event.id, event);
        }
        let deliveries: Vec<(EventId, LocalDelivery)> = operations
            .iter()
            .map(|(event, delivery)| (event.id, delivery.clone()))
            .collect();
        let (rows, _) = self
            .projection
            .reconcile_local(inputs.into_values(), deliveries);
        let rows: &[Arc<MessageRow>] = if self.root_unavailable && !exact {
            &[]
        } else {
            &rows
        };
        let message_id = self.config.message_id;
        let target = if self.target_status == Some(TargetStatus::Ready) {
            rows.iter().find(|row| row.id == message_id).cloned()
        } else {
            None
        };
        if self.target_status == Some(TargetStatus::Ready) && target.is_none() {
            self.target_status = Some(TargetStatus::Unavailable);
        }
        let target_ready = self.target_status == Some(TargetStatus::Ready);
        let readable = |row: &&Arc<MessageRow>| !exact || row.id != message_id || target_ready;
        let replies: Vec<Arc<MessageRow>> = rows
            .iter()
            .filter(readable)
            .filter(|row| {
                Some(row.id) != self.root_id && (!self.root_unavailable || row.id == message_id)
            })
            .cloned()
            .collect();
        let root = if self.root_unavailable {
            None
        } else {
            rows.iter()
                .filter(readable)
                .find(|row| Some(row.id) == self.root_id)
                .cloned()
        };
        let current = &self.current;
        let replies: Arc<[Arc<MessageRow>]> = if current.replies.len() == replies.len()
            && current
                .replies
                .iter()
                .zip(&replies)
                .all(|(a, b)| Arc::ptr_eq(a, b))
        {
            current.replies.clone()
        } else {
            replies.into()
        };
        let tree = if Arc::ptr_eq(&replies, &current.replies) && self.tree_root == self.root_id {
            current.tree.clone()
        } else {
            Arc::new(reply_tree(&replies, self.root_id.as_ref()))
        };
        self.tree_root = self.root_id;
        let next = ThreadSnapshot {
            root,
            replies,
            tree,
            target: if exact { target } else { None },
            ..self.with_fields()
        };
        self.set_snapshot(next);
    }

    /// Republishes status fields over the last fold.
    fn publish_status(&mut self) {
        let next = ThreadSnapshot {
            root: self.current.root.clone(),
            replies: self.current.replies.clone(),
            tree: self.current.tree.clone(),
            target: if self.target_status == Some(TargetStatus::Ready) {
                self.current.target.clone()
            } else {
                None
            },
            ..self.with_fields()
        };
        self.set_snapshot(next);
    }

    fn with_fields(&self) -> ThreadSnapshot {
        let fields = self.fields.clone();
        ThreadSnapshot {
            status: fields.status,
            root: None,
            replies: Arc::new([]),
            tree: Arc::default(),
            error: fields.error,
            can_load_more: fields.can_load_more,
            limited: fields.limited,
            direction: fields.direction,
            read_kind: fields.read_kind,
            target: None,
            target_status: if self.config.exact {
                self.target_status
            } else {
                None
            },
        }
    }

    fn set_snapshot(&mut self, next: ThreadSnapshot) {
        if !next.same(&self.current) {
            self.current = Arc::new(next);
            self.changed = true;
        }
    }
}

fn is_content(event: &Verified) -> bool {
    CONTENT_KINDS.contains(&event.kind_u16())
}

/// Ids named by `e` tags, compared as lowercase hex like the reference.
fn targets(event: &Verified) -> impl Iterator<Item = EventId> + '_ {
    event.tags_named("e").filter_map(|tag| {
        tag.get(1)
            .filter(|value| is_hex64(value))
            .and_then(|value| EventId::from_hex(value).ok())
    })
}

fn union(base: &EventMap, extra: impl IntoIterator<Item = Verified>) -> EventMap {
    let mut all = base.clone();
    for event in extra {
        all.insert(event.id, event);
    }
    all
}

/// Over [`MAX_EVENTS`] or over [`MAX_BYTES`] as one JSON array.
fn over_budget<'a>(events: impl ExactSizeIterator<Item = &'a Verified>) -> bool {
    let count = events.len();
    if count > MAX_EVENTS {
        return true;
    }
    let bytes: usize = events.map(Verified::byte_size).sum::<usize>() + count.saturating_sub(1) + 2;
    bytes > MAX_BYTES
}
