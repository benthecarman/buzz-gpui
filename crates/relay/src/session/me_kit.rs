//! The session's recipe catalog and the preferences built on it
//! (`channel-templates/capability.ts`, `relay/me-preferences.ts`,
//! `relay/sidebar-personal-groups.ts`).
//!
//! [`MeKit`] reads the viewer's private recipe records, at most one read at
//! a time, and saves one record at a time through the outbox with the
//! same revision checks as buzz-app: the head must be the one the caller
//! saw, no earlier save of the coordinate may be unresolved, and the new
//! record must be the one the relay selects afterwards.
//!
//! On top of it sit the Me groups and placement (which conversations live
//! in Me rather than Messages) and the Messages "personal groups" that
//! replace the legacy sidebar sections once that record exists.

use super::actions::{Context, Ops};
use super::work_sessions::delivered;
use super::{Message, Session};
use crate::event::Verified;
use crate::me_kit::{self as kit, GroupsId, KitEntry};
use crate::outbox::{Delivery, SendOptions};
use crate::reader::ReadOptions;
use crate::sidebar::{
    AssignmentIntent, GroupSource, NewSection, PersonalGroup, PersonalGroups, SidebarGroups,
    SidebarPreferences,
};
use crate::sidebar_store::{Destination, PreferencesStore, SidebarState};
use crate::socket::Filter;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use nostr::prelude::EventId;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

/// A save older than this cannot be replayed (`confirm`).
const REPLAY_WINDOW_SECS: u64 = 15 * 60;
const SAVE_UNAVAILABLE: &str = "Recipe saving is unavailable or already in progress";
const UNRESOLVED: &str =
    "A save for this recipe is unresolved. Inspect Outbox and refresh before replacing it.";
const TOO_SOON: &str = "Please wait a second before saving this recipe again";
/// `CanvasConflictError`.
pub const CANVAS_CONFLICT: &str = "Canvas changed since you opened it. Your draft is kept; load the current document before replacing it.";

#[derive(Clone, Debug, Default, PartialEq)]
pub enum MeKitStatus {
    #[default]
    Idle,
    Loading,
    Ready,
    Error(String),
    /// The session cannot read or save recipes (offline resume).
    Unavailable,
}

/// The catalog as last read.
#[derive(Clone, Debug, PartialEq)]
pub struct MeKitSnapshot {
    pub status: MeKitStatus,
    pub entries: Arc<[KitEntry]>,
}

impl Default for MeKitSnapshot {
    fn default() -> Self {
        Self {
            status: MeKitStatus::Idle,
            entries: Arc::new([]),
        }
    }
}

impl MeKitSnapshot {
    pub fn error(&self) -> Option<&str> {
        match &self.status {
            MeKitStatus::Error(error) => Some(error),
            _ => None,
        }
    }

    pub fn ready(&self) -> bool {
        self.status == MeKitStatus::Ready
    }

    /// The Me record's placement (`useMePlacement`'s `ids`).
    pub fn me_channels(&self) -> Vec<String> {
        kit::groups(&self.entries, GroupsId::Me)
            .map(|groups| groups.channels)
            .unwrap_or_default()
    }

    /// The live Me groups, if the record exists.
    pub fn me(&self) -> Option<PersonalGroups> {
        kit::groups(&self.entries, GroupsId::Me)
    }

    /// The live Messages personal groups, if the record exists.
    pub fn personal(&self) -> Option<PersonalGroups> {
        kit::groups(&self.entries, GroupsId::Personal)
    }
}

/// The session's recipe catalog. Clones share one catalog.
#[derive(Clone)]
pub struct MeKit {
    inner: Arc<Inner>,
}

struct Inner {
    commands: mpsc::UnboundedSender<Message>,
    runtime: tokio::runtime::Handle,
    community: String,
    snapshot: watch::Sender<Arc<MeKitSnapshot>>,
    refreshing: Mutex<Option<Shared<BoxFuture<'static, ()>>>>,
    /// Bumped by [`MeKit::clear`]; a read started before is dropped.
    epoch: AtomicU64,
    saving: AtomicBool,
    me: Mutex<PreferencesStore>,
    me_state: watch::Sender<Arc<SidebarState>>,
    /// Me mutations run one at a time (`serialize`).
    mutations: tokio::sync::Mutex<()>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn ids_filter(id: &str) -> Filter {
    let mut filter = Filter::new();
    filter.insert("ids".into(), json!([id]));
    filter.insert("limit".into(), json!(1));
    filter.insert("consistency".into(), json!("strong"));
    filter
}

impl MeKit {
    pub(super) fn new(
        commands: mpsc::UnboundedSender<Message>,
        runtime: tokio::runtime::Handle,
        community: String,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                commands,
                runtime,
                community,
                snapshot: watch::Sender::new(Arc::default()),
                refreshing: Mutex::new(None),
                epoch: AtomicU64::new(0),
                saving: AtomicBool::new(false),
                me: Mutex::new(PreferencesStore::default()),
                me_state: watch::Sender::new(Arc::default()),
                mutations: tokio::sync::Mutex::new(()),
            }),
        }
    }

    fn ops(&self) -> Ops {
        Ops {
            commands: self.inner.commands.clone(),
        }
    }

    async fn context(&self) -> Result<Context, String> {
        self.ops().context().await
    }

    /// The community the records are scoped to (its HTTP origin).
    pub fn community(&self) -> &str {
        &self.inner.community
    }

    pub fn snapshot(&self) -> Arc<MeKitSnapshot> {
        self.inner.snapshot.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<Arc<MeKitSnapshot>> {
        self.inner.snapshot.subscribe()
    }

    fn set(&self, status: MeKitStatus, entries: Option<Arc<[KitEntry]>>) {
        self.inner.snapshot.send_modify(|snapshot| {
            *snapshot = Arc::new(MeKitSnapshot {
                status,
                entries: entries.unwrap_or_else(|| snapshot.entries.clone()),
            });
        });
        // Me placement takes conversations out of the Messages sidebar.
        let _ = self
            .inner
            .commands
            .send(Message::With(Box::new(|actor| actor.publish())));
    }

    /// Reads the catalog once, if it never was.
    pub fn ensure(&self) {
        if self.snapshot().status == MeKitStatus::Idle {
            drop(self.refresh());
        }
    }

    /// Reads the catalog again; joins a read already running.
    pub fn refresh(&self) -> Shared<BoxFuture<'static, ()>> {
        let mut slot = self.inner.refreshing.lock().expect("kit lock");
        if let Some(running) = slot.as_ref() {
            return running.clone();
        }
        let epoch = self.inner.epoch.load(Ordering::SeqCst);
        self.set(MeKitStatus::Loading, None);
        let kit = self.clone();
        let work = async move {
            let result = kit.read_catalog().await;
            if kit.inner.epoch.load(Ordering::SeqCst) == epoch {
                *kit.inner.refreshing.lock().expect("kit lock") = None;
                match result {
                    Ok(entries) => kit.set(MeKitStatus::Ready, Some(entries.into())),
                    Err(error) => kit.set(MeKitStatus::Error(error), None),
                }
            }
        }
        .boxed()
        .shared();
        *slot = Some(work.clone());
        self.inner.runtime.spawn(work.clone());
        work
    }

    async fn read_catalog(&self) -> Result<Vec<KitEntry>, String> {
        let ctx = self.context().await?;
        let events = fresh(&ctx, vec![kit::catalog_filter(&ctx.viewer)]).await?;
        if events.len() >= kit::CATALOG_LIMIT {
            return Err(
                "Recipe catalog reached its read limit; no partial catalog will be used".into(),
            );
        }
        kit::heads(&events, &self.inner.community)
            .iter()
            .map(|event| kit::decode(&ctx.identity, event, &self.inner.community))
            .collect()
    }

    /// Forgets the catalog, e.g. on sign-out of the community.
    pub fn clear(&self) {
        self.inner.epoch.fetch_add(1, Ordering::SeqCst);
        *self.inner.refreshing.lock().expect("kit lock") = None;
        self.set(MeKitStatus::Idle, Some(Arc::new([])));
        self.inner.me.lock().expect("me lock").clear();
        self.publish_me();
    }

    async fn read_head(&self, ctx: &Context, record: &Value) -> Result<Option<Verified>, String> {
        let coordinate = kit::coordinate(record, &self.inner.community)?;
        let events = fresh(ctx, vec![kit::head_filter(&ctx.viewer, &coordinate)]).await?;
        Ok(kit::selected_head(&events).cloned())
    }

    /// Saves `value` (a record's `value`) if the relay's head is still
    /// `expected`, then reads the catalog again. Returns the new event id.
    pub async fn save(
        &self,
        value: Value,
        expected: Option<String>,
        deleted: bool,
    ) -> Result<String, String> {
        let kit = self.clone();
        self.inner
            .runtime
            .spawn(async move { kit.save_now(value, expected, deleted).await })
            .await
            .unwrap_or_else(|_| Err("The session has closed".into()))
    }

    async fn save_now(
        &self,
        value: Value,
        expected: Option<String>,
        deleted: bool,
    ) -> Result<String, String> {
        let record = kit::record(&self.inner.community, value, deleted);
        let coordinate = kit::coordinate(&record, &self.inner.community)?;
        let ctx = self.context().await.map_err(|_| SAVE_UNAVAILABLE)?;
        if self.inner.saving.swap(true, Ordering::SeqCst) {
            return Err(SAVE_UNAVAILABLE.into());
        }
        let result = async {
            ctx.outbox.ready().await.map_err(|error| error.to_string())?;
            let head = self.read_head(&ctx, &record).await?;
            if head.as_ref().map(Verified::id_hex) != expected {
                return Err("This saved recipe changed. Refresh the catalog and review your draft before replacing it.".to_owned());
            }
            let unresolved = ctx.outbox.visible().iter().any(|op| {
                op.kind() == 30078
                    && op.has_tag("d", &coordinate)
                    && !matches!(op.delivery, Delivery::Accepted | Delivery::Seen)
            });
            if unresolved {
                return Err(UNRESOLVED.into());
            }
            // Replaceable records use seconds; never an equal-time
            // replacement.
            if head.as_ref().is_some_and(|head| head.created_at_secs() >= now()) {
                return Err(TOO_SOON.into());
            }
            let (content, tags) = kit::prepare(&ctx.identity, &record, &self.inner.community)?;
            let id = ctx
                .outbox
                .send(
                    crate::messages::Draft {
                        kind: 30078,
                        content,
                        tags,
                    },
                    SendOptions::default(),
                )
                .map_err(|error| error.to_string())?;
            self.confirm(&ctx, id).await?;
            if self.read_head(&ctx, &record).await?.map(|e| e.id_hex()) != Some(id.to_hex()) {
                return Err("Another recipe save is selected. Your draft is kept; refresh and review before saving again.".into());
            }
            Ok(id.to_hex())
        }
        .await;
        self.inner.saving.store(false, Ordering::SeqCst);
        let id = result?;
        self.refresh().await;
        Ok(id)
    }

    /// `confirm`: the exact event, from the relay, after the outbox
    /// delivered it.
    pub(super) async fn confirm(&self, ctx: &Context, id: EventId) -> Result<(), String> {
        let hex = id.to_hex();
        let found = |events: &[Verified]| events.iter().any(|event| event.id_hex() == hex);
        if found(&fresh(ctx, vec![ids_filter(&hex)]).await?) {
            return Ok(());
        }
        let saved = ctx.outbox.get(&id);
        if saved
            .as_ref()
            .is_none_or(|op| now().saturating_sub(op.created_at()) >= REPLAY_WINDOW_SECS)
        {
            return Err("This operation could not be confirmed and is too old to replay. Keep your draft and inspect the current result before making a new save.".into());
        }
        delivered(ctx, id, false, true).await?;
        if !found(&fresh(ctx, vec![ids_filter(&hex)]).await?) {
            return Err("Save is awaiting exact relay confirmation; your draft is kept".into());
        }
        Ok(())
    }

    // ----- Canvas (`capability.ts` `canvas`) -----

    /// Whether the viewer can still write in `channel` (`canWrite`).
    async fn can_write(&self, channel: &str) -> Result<bool, String> {
        let id = channel.to_owned();
        self.ops()
            .with(move |actor| {
                actor
                    .discovery
                    .as_ref()
                    .and_then(|discovery| discovery.get(&id))
                    .is_some_and(|summary| {
                        !summary.read_only && !summary.archived && !summary.cached
                    })
            })
            .await
    }

    /// The channel's current Canvas (kind 40100), read strongly.
    pub async fn canvas_read(&self, channel: &str) -> Result<Option<Verified>, String> {
        let kit = self.clone();
        let channel = channel.to_owned();
        self.inner
            .runtime
            .spawn(async move {
                if !kit.can_write(&channel).await? {
                    return Err("Canvas is unavailable after channel access changed".into());
                }
                let ctx = kit.context().await?;
                kit.canvas_head(&ctx, &channel).await
            })
            .await
            .unwrap_or_else(|_| Err("The session has closed".into()))
    }

    async fn canvas_head(&self, ctx: &Context, channel: &str) -> Result<Option<Verified>, String> {
        let mut filter = Filter::new();
        filter.insert("kinds".into(), json!([crate::kinds::CANVAS]));
        filter.insert("#h".into(), json!([channel]));
        filter.insert("limit".into(), json!(1));
        filter.insert("consistency".into(), json!("strong"));
        let events = fresh(ctx, vec![filter]).await?;
        Ok(kit::selected_head(&events).cloned())
    }

    /// Saves Canvas if its head is still `expected` (`canvas.save`), and
    /// returns the new head.
    pub async fn canvas_save(
        &self,
        channel: &str,
        content: &str,
        expected: Option<String>,
    ) -> Result<Verified, String> {
        let kit = self.clone();
        let (channel, content) = (channel.to_owned(), content.to_owned());
        self.inner
            .runtime
            .spawn(async move { kit.canvas_save_now(&channel, &content, expected).await })
            .await
            .unwrap_or_else(|_| Err("The session has closed".into()))
    }

    async fn canvas_save_now(
        &self,
        channel: &str,
        content: &str,
        expected: Option<String>,
    ) -> Result<Verified, String> {
        if !self.can_write(channel).await? {
            return Err("Canvas writing is unavailable in this channel".into());
        }
        if content.len() > kit::CANVAS_BYTES {
            return Err("Keep Canvas below 24 KiB".into());
        }
        let ctx = self.context().await?;
        ctx.outbox
            .ready()
            .await
            .map_err(|error| error.to_string())?;
        let head = self.canvas_head(&ctx, channel).await?;
        if head.as_ref().map(Verified::id_hex) != expected {
            return Err(CANVAS_CONFLICT.into());
        }
        let unresolved = ctx.outbox.visible().iter().any(|op| {
            op.kind() == crate::kinds::CANVAS
                && op.has_tag("h", channel)
                && !matches!(op.delivery, Delivery::Accepted | Delivery::Seen)
        });
        if unresolved {
            return Err("This channel has an unresolved Canvas save. Inspect Outbox before making another save.".into());
        }
        if head
            .as_ref()
            .is_some_and(|head| head.created_at_secs() >= now())
        {
            return Err("Please wait a second before saving Canvas again".into());
        }
        if !self.can_write(channel).await? {
            return Err("Channel access changed".into());
        }
        let id = ctx
            .outbox
            .send(
                crate::messages::Draft {
                    kind: crate::kinds::CANVAS,
                    content: content.to_owned(),
                    tags: vec![
                        vec!["h".into(), channel.to_owned()],
                        vec![
                            "expected-revision".into(),
                            expected.unwrap_or_else(|| "none".into()),
                        ],
                    ],
                },
                SendOptions::default(),
            )
            .map_err(|error| error.to_string())?;
        self.confirm(&ctx, id).await?;
        match self.canvas_head(&ctx, channel).await? {
            Some(head) if head.id_hex() == id.to_hex() => Ok(head),
            _ => Err("Your save was received, but a different Canvas is selected. Your draft is kept; load the current document.".into()),
        }
    }

    // ----- Me groups and placement (`me-preferences.ts`) -----

    pub fn me_state(&self) -> Arc<SidebarState> {
        self.inner.me_state.borrow().clone()
    }

    pub fn watch_me(&self) -> watch::Receiver<Arc<SidebarState>> {
        self.inner.me_state.subscribe()
    }

    fn publish_me(&self) {
        let state = Arc::new(self.inner.me.lock().expect("me lock").state(Vec::new()));
        self.inner.me_state.send_if_modified(|current| {
            let changed = **current != *state;
            if changed {
                *current = state;
            }
            changed
        });
    }

    /// The Me record now: its value (empty without one) and entry.
    async fn current(&self, fresh: bool) -> Result<(PersonalGroups, Option<KitEntry>), String> {
        if fresh || !self.snapshot().ready() {
            self.refresh().await;
        }
        let snapshot = self.snapshot();
        if !snapshot.ready() {
            return Err(snapshot
                .error()
                .unwrap_or("Me groups are unavailable")
                .to_owned());
        }
        let entry = kit::groups_entry(&snapshot.entries, GroupsId::Me).cloned();
        let value = entry
            .as_ref()
            .and_then(KitEntry::groups)
            .unwrap_or_default();
        Ok((value, entry))
    }

    /// Saves Me groups and checks the relay now has exactly them.
    async fn save_me(
        &self,
        value: PersonalGroups,
        entry: Option<&KitEntry>,
    ) -> Result<SidebarGroups, String> {
        kit::admit(&value, entry, &self.inner.community)?;
        self.save(
            kit::groups_value(GroupsId::Me, &value),
            entry.map(|entry| entry.event_id.clone()),
            false,
        )
        .await?;
        if self.snapshot().me().as_ref() != Some(&value) {
            return Err("Me group changes could not be confirmed".into());
        }
        Ok(value.project())
    }

    fn me_preferences(groups: &PersonalGroups) -> SidebarPreferences {
        let projected = groups.project();
        SidebarPreferences {
            sections: projected.sections,
            assignments: projected.assignments,
            group_source: GroupSource::Personal,
            ..Default::default()
        }
    }

    /// Reads the Me groups once, if never read.
    pub fn me_ensure(&self) {
        let idle = {
            let store = self.inner.me.lock().expect("me lock");
            store.confirmed().is_none() && !store.reading()
        };
        if idle {
            self.me_refresh();
        }
    }

    /// Reads the Me groups again (Retry sections).
    pub fn me_refresh(&self) {
        let Some(ticket) = self.inner.me.lock().expect("me lock").begin_read() else {
            return;
        };
        self.publish_me();
        let kit = self.clone();
        self.inner.runtime.spawn(async move {
            let result = kit
                .current(false)
                .await
                .map(|(value, _)| Self::me_preferences(&value));
            kit.inner
                .me
                .lock()
                .expect("me lock")
                .finish_read(ticket, result);
            kit.publish_me();
        });
    }

    /// Moves a conversation to a Me group, or back to Conversations
    /// (`None`). Shown at once; a failure stays until retried or
    /// dismissed.
    pub async fn me_assign(&self, channel: &str, section: Option<String>) -> Result<(), String> {
        self.me_move(channel, Destination::Section(section)).await
    }

    /// Creates a Me group and moves the conversation into it.
    pub async fn me_create_and_assign(
        &self,
        channel: &str,
        section: NewSection,
    ) -> Result<(), String> {
        self.me_move(channel, Destination::Create(section)).await
    }

    pub async fn me_retry_move(&self, channel: &str) -> Result<(), String> {
        let started = self.inner.me.lock().expect("me lock").retry_move(channel);
        self.run_move(started).await
    }

    pub fn me_dismiss_move(&self, channel: &str) {
        self.inner.me.lock().expect("me lock").dismiss_move(channel);
        self.publish_me();
    }

    async fn me_move(&self, channel: &str, destination: Destination) -> Result<(), String> {
        let started = self
            .inner
            .me
            .lock()
            .expect("me lock")
            .begin_move(channel, destination);
        self.run_move(started).await
    }

    async fn run_move(
        &self,
        started: Result<
            (
                crate::sidebar_store::Ticket,
                crate::sidebar_store::MoveIntent,
            ),
            String,
        >,
    ) -> Result<(), String> {
        self.publish_me();
        let (ticket, intent) = started?;
        let kit = self.clone();
        let result = self
            .inner
            .runtime
            .spawn(async move {
                let _serial = kit.inner.mutations.lock().await;
                let assignment = match &intent.destination {
                    Destination::Section(section) => AssignmentIntent {
                        channel_id: intent.channel_id.clone(),
                        section_id: section.clone(),
                        create_section: None,
                    },
                    Destination::Create(new) => AssignmentIntent {
                        channel_id: intent.channel_id.clone(),
                        section_id: None,
                        create_section: Some(new.clone()),
                    },
                    Destination::Starred => return Err("Me groups have no stars".into()),
                };
                let (value, entry) = kit.current(true).await?;
                let next = assign(
                    &value,
                    &assignment,
                    "The new Me group changed; refresh before retrying",
                    "The Me group is unavailable",
                )?;
                match next {
                    None => Ok(value.project()),
                    Some(next) => kit.save_me(next, entry.as_ref()).await,
                }
            })
            .await
            .unwrap_or_else(|_| Err("The session has closed".into()));
        let outcome = self
            .inner
            .me
            .lock()
            .expect("me lock")
            .finish_move(ticket, result.map(|groups| (Some(groups), Vec::new())));
        self.publish_me();
        outcome
    }

    /// Deletes a Me group; its conversations go back to Conversations.
    pub async fn me_remove_section(&self, section: &str) -> Result<(), String> {
        let started = self
            .inner
            .me
            .lock()
            .expect("me lock")
            .begin_remove_section(section);
        self.publish_me();
        let (ticket, _) = started?;
        let kit = self.clone();
        let section = section.to_owned();
        let result = self
            .inner
            .runtime
            .spawn(async move {
                let _serial = kit.inner.mutations.lock().await;
                let (value, entry) = kit.current(true).await?;
                if !value.groups.iter().any(|group| group.id == section) {
                    return Ok(value.project());
                }
                kit.save_me(remove_group(&value, &section), entry.as_ref())
                    .await
            })
            .await
            .unwrap_or_else(|_| Err("The session has closed".into()));
        let outcome = self
            .inner
            .me
            .lock()
            .expect("me lock")
            .finish_remove_section(ticket, None, result);
        self.publish_me();
        outcome
    }

    /// Whether the conversation is placed in Me (`placement.has`).
    pub fn placed(&self, channel: &str) -> bool {
        let snapshot = self.snapshot();
        snapshot.ready() && snapshot.me_channels().iter().any(|id| id == channel)
    }

    /// Checks a new conversation would fit in Me (`placement.admit`).
    pub async fn admit(&self, channel: &str, section: Option<&str>) -> Result<(), String> {
        let kit = self.clone();
        let (channel, section) = (channel.to_owned(), section.map(str::to_owned));
        self.inner
            .runtime
            .spawn(async move {
                let _serial = kit.inner.mutations.lock().await;
                let (value, entry) = kit.current(true).await?;
                let next = kit::placed(&value, &channel, true, section.as_deref())?;
                kit::admit(&next, entry.as_ref(), &kit.inner.community)
            })
            .await
            .unwrap_or_else(|_| Err("The session has closed".into()))
    }

    /// Puts a conversation in Me (optionally in a group) or takes it out
    /// (`placement.set`).
    pub async fn set_placement(
        &self,
        channel: &str,
        personal: bool,
        section: Option<&str>,
    ) -> Result<(), String> {
        let kit = self.clone();
        let (channel, section) = (channel.to_owned(), section.map(str::to_owned));
        let result = self
            .inner
            .runtime
            .spawn(async move {
                let _serial = kit.inner.mutations.lock().await;
                let (value, entry) = kit.current(true).await?;
                let section = if personal { section } else { None };
                let next = kit::placed(&value, &channel, personal, section.as_deref())?;
                if next == value {
                    return Ok(());
                }
                kit.save_me(next, entry.as_ref()).await.map(drop)
            })
            .await
            .unwrap_or_else(|_| Err("The session has closed".into()));
        // The groups view follows the saved record.
        self.me_refresh();
        result
    }

    // ----- Messages personal groups (`sidebar-personal-groups.ts`) -----

    /// The legacy preferences with personal groups in place of their
    /// sections when that record exists (`readActiveSidebarGroups`).
    pub(super) async fn active_groups(
        &self,
        legacy: SidebarPreferences,
    ) -> Result<SidebarPreferences, String> {
        if self.snapshot().status == MeKitStatus::Unavailable {
            return Ok(legacy);
        }
        if !self.snapshot().ready() {
            self.refresh().await;
        }
        let snapshot = self.snapshot();
        if !snapshot.ready() {
            return Err(snapshot
                .error()
                .unwrap_or("Personal groups are unavailable")
                .to_owned());
        }
        Ok(match snapshot.personal() {
            Some(groups) => legacy.with_personal_groups(&groups),
            None => legacy,
        })
    }

    /// A Messages group move through the personal-groups record. `None`:
    /// no such record, so the legacy writer saves it
    /// (`activeSidebarAssignment`).
    pub(super) async fn personal_assign(
        &self,
        intent: &AssignmentIntent,
        source: GroupSource,
    ) -> Result<Option<SidebarGroups>, String> {
        if self.snapshot().status == MeKitStatus::Unavailable {
            return if source == GroupSource::Personal {
                Err("Personal groups are unavailable".into())
            } else {
                Ok(None)
            };
        }
        self.refresh().await;
        let snapshot = self.snapshot();
        if !snapshot.ready() {
            return Err(snapshot
                .error()
                .unwrap_or("Personal groups could not be refreshed")
                .to_owned());
        }
        let entry = kit::groups_entry(&snapshot.entries, GroupsId::Personal);
        let current = entry.and_then(KitEntry::groups);
        if current.is_some() != (source == GroupSource::Personal) {
            return Err(
                "The active group source changed; refresh your sidebar before moving this channel"
                    .into(),
            );
        }
        let (Some(current), Some(entry)) = (current, entry) else {
            return Ok(None);
        };
        let target = intent
            .create_section
            .as_ref()
            .map(|new| new.id.clone())
            .or_else(|| intent.section_id.clone());
        let Some(next) = assign(
            &current,
            intent,
            "The new section changed; refresh before retrying",
            "The destination group is unavailable",
        )?
        else {
            return Ok(Some(current.project()));
        };
        self.save(
            kit::groups_value(GroupsId::Personal, &next),
            Some(entry.event_id.clone()),
            false,
        )
        .await?;
        let confirmed = self.snapshot().personal();
        match confirmed {
            Some(groups) if groups.assignments.get(&intent.channel_id) == target.as_ref() => {
                Ok(Some(groups.project()))
            }
            _ => Err("Personal group placement could not be confirmed".into()),
        }
    }

    /// Removes a Messages personal group (`activeSidebarSectionRemoval`).
    /// `None`: legacy groups are active.
    pub(super) async fn personal_remove(
        &self,
        section: &str,
        source: GroupSource,
    ) -> Result<Option<SidebarGroups>, String> {
        if self.snapshot().status == MeKitStatus::Unavailable {
            return if source == GroupSource::Personal {
                Err("Personal groups are unavailable".into())
            } else {
                Ok(None)
            };
        }
        self.refresh().await;
        let snapshot = self.snapshot();
        if !snapshot.ready() {
            return Err(snapshot
                .error()
                .unwrap_or("Personal groups could not be refreshed")
                .to_owned());
        }
        let entry = kit::groups_entry(&snapshot.entries, GroupsId::Personal);
        let current = entry.and_then(KitEntry::groups);
        if current.is_some() != (source == GroupSource::Personal) {
            return Err("The active group source changed; refresh your sidebar before removing this section".into());
        }
        if source == GroupSource::Legacy {
            return Ok(None);
        }
        let (Some(current), Some(entry)) = (current, entry) else {
            return Err("Personal group no longer exists".into());
        };
        if !current.groups.iter().any(|group| group.id == section) {
            return Err("Personal group no longer exists".into());
        }
        let next = remove_group(&current, section);
        if let Err(message) = self
            .save(
                kit::groups_value(GroupsId::Personal, &next),
                Some(entry.event_id.clone()),
                false,
            )
            .await
        {
            return Err(removal_error(&message));
        }
        match self.snapshot().personal() {
            Some(groups)
                if !groups.groups.iter().any(|group| group.id == section)
                    && !groups.assignments.values().any(|id| id == section) =>
            {
                Ok(Some(groups.project()))
            }
            _ => Err("Personal section removal could not be confirmed".into()),
        }
    }

    /// Saves the Messages personal groups as edited in the "Personal
    /// groups" dialog (`ChannelGroupsDialog.tsx`).
    pub async fn save_personal_groups(&self, groups: Vec<PersonalGroup>) -> Result<(), String> {
        self.refresh().await;
        let snapshot = self.snapshot();
        if !snapshot.ready() {
            return Err(snapshot
                .error()
                .unwrap_or("Personal groups could not be refreshed")
                .to_owned());
        }
        let entry = kit::groups_entry(&snapshot.entries, GroupsId::Personal).cloned();
        let current = entry
            .as_ref()
            .and_then(KitEntry::groups)
            .unwrap_or_default();
        let next = with_groups(&current, groups);
        self.save(
            kit::groups_value(GroupsId::Personal, &next),
            entry.map(|entry| entry.event_id),
            false,
        )
        .await
        .map(drop)
    }
}

/// The groups after an assignment, or `None` when nothing changes.
fn assign(
    current: &PersonalGroups,
    intent: &AssignmentIntent,
    changed: &str,
    unavailable: &str,
) -> Result<Option<PersonalGroups>, String> {
    let target = intent
        .create_section
        .as_ref()
        .map(|new| new.id.clone())
        .or_else(|| intent.section_id.clone());
    let mut next = current.clone();
    if let Some(new) = &intent.create_section {
        let name = new.name.trim();
        match next.groups.iter().find(|group| group.id == new.id) {
            Some(existing) if existing.name != name => return Err(changed.into()),
            Some(_) => {}
            None => next.groups.push(PersonalGroup {
                id: new.id.clone(),
                name: name.into(),
                default_template_id: String::new(),
            }),
        }
    }
    if let Some(target) = &target
        && !next.groups.iter().any(|group| &group.id == target)
    {
        return Err(unavailable.into());
    }
    match &target {
        Some(target) => next
            .assignments
            .insert(intent.channel_id.clone(), target.clone()),
        None => next.assignments.remove(&intent.channel_id),
    };
    Ok((next != *current).then_some(next))
}

fn remove_group(current: &PersonalGroups, section: &str) -> PersonalGroups {
    let mut next = current.clone();
    next.groups.retain(|group| group.id != section);
    next.assignments.retain(|_, group| group != section);
    next
}

/// Names, order and defaults from the dialog; assignments to removed
/// groups go back to the default sections.
fn with_groups(current: &PersonalGroups, groups: Vec<PersonalGroup>) -> PersonalGroups {
    let mut next = current.clone();
    next.groups = groups;
    let ids: Vec<String> = next.groups.iter().map(|group| group.id.clone()).collect();
    next.assignments.retain(|_, group| ids.contains(group));
    next
}

/// The removal error copy (`activeSidebarSectionRemoval`).
fn removal_error(message: &str) -> String {
    if message == TOO_SOON {
        "This section was just saved. Wait a second, then choose Remove section again.".into()
    } else if message == UNRESOLVED {
        "A section removal or other personal-group save is unresolved. Open Channel settings → Diagnostics → Outbox to inspect or retry the existing operation, then reload the app to check the result before removing this section again.".into()
    } else {
        format!(
            "Section removal could not be confirmed: {message} Open Channel settings → Diagnostics → Outbox to inspect any pending operation before retrying."
        )
    }
}

async fn fresh(ctx: &Context, filters: Vec<Filter>) -> Result<Vec<Verified>, String> {
    ctx.reader
        .read(
            filters,
            ReadOptions {
                fresh: true,
                ..ReadOptions::default()
            },
        )
        .await
        .map(|events| events.to_vec())
        .map_err(|error| error.message)
}

impl Session {
    /// The viewer's recipe catalog, Me groups and placement.
    pub fn me_kit(&self) -> &MeKit {
        &self.me_kit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::collections::BTreeMap;

    const CHANNEL: &str = "11111111-1111-4111-8111-111111111111";

    fn groups(ids: &[&str]) -> PersonalGroups {
        PersonalGroups {
            groups: ids
                .iter()
                .map(|id| PersonalGroup {
                    id: (*id).into(),
                    name: id.to_uppercase(),
                    default_template_id: String::new(),
                })
                .collect(),
            assignments: BTreeMap::new(),
            channels: Vec::new(),
        }
    }

    #[test]
    fn assignment_creates_moves_and_refuses_changed_groups() {
        let current = groups(&["a"]);
        let create = AssignmentIntent {
            channel_id: CHANNEL.into(),
            section_id: None,
            create_section: Some(NewSection {
                id: "b".into(),
                name: " B ".into(),
            }),
        };
        let next = assign(&current, &create, "changed", "gone")
            .unwrap()
            .unwrap();
        assert_eq!(next.groups[1].name, "B");
        assert_eq!(next.assignments[CHANNEL], "b");
        // The same create again changes nothing.
        assert!(assign(&next, &create, "changed", "gone").unwrap().is_none());
        let renamed = AssignmentIntent {
            create_section: Some(NewSection {
                id: "b".into(),
                name: "Other".into(),
            }),
            ..create
        };
        assert_eq!(
            assign(&next, &renamed, "changed", "gone").unwrap_err(),
            "changed"
        );
        let missing = AssignmentIntent {
            channel_id: CHANNEL.into(),
            section_id: Some("zzz".into()),
            create_section: None,
        };
        assert_eq!(
            assign(&next, &missing, "changed", "gone").unwrap_err(),
            "gone"
        );
        let back = AssignmentIntent {
            channel_id: CHANNEL.into(),
            section_id: None,
            create_section: None,
        };
        let moved = assign(&next, &back, "changed", "gone").unwrap().unwrap();
        assert!(moved.assignments.is_empty());
    }

    #[test]
    fn removing_a_group_drops_its_assignments() {
        let mut current = groups(&["a", "b"]);
        current.assignments.insert(CHANNEL.into(), "a".into());
        let next = remove_group(&current, "a");
        assert_eq!(next.groups.len(), 1);
        assert!(next.assignments.is_empty());
        let edited = with_groups(&current, groups(&["b"]).groups);
        assert!(edited.assignments.is_empty());
    }

    #[test]
    fn removal_errors_match_the_reference() {
        assert!(removal_error(TOO_SOON).starts_with("This section was just saved."));
        assert!(removal_error(UNRESOLVED).starts_with("A section removal or other"));
        assert_eq!(
            removal_error("Boom."),
            "Section removal could not be confirmed: Boom. Open Channel settings → Diagnostics → Outbox to inspect any pending operation before retrying."
        );
    }
}
