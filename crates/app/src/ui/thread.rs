//! The thread panel in the Messages board's right dock (ui.md §4.11).

use super::markdown::Directory;
use super::message::{self, Actions, Interaction, Item, Layout, Rows};
use super::time;
use crate::community::Community;
use crate::mirror::mirror;
use buzz_relay_client::fold::{MessageRow, ReplyTree};
use buzz_relay_client::session::ThreadView;
use buzz_relay_client::threads::{Direction, ReadKind};
use buzz_relay_client::{Profiles, TargetStatus, ThreadSnapshot, ThreadStatus};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, IconButton, IconButtonSize, IconName,
    PanelHeader, Tab, TabVariant, Tabs, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nostr::prelude::EventId;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

/// Older replies load when the reader is this many rows from the top.
const LOAD_OLDER_WITHIN: usize = 3;
/// Child lists indent by one control width.
const INDENT: f32 = 40.;
/// Indentation stops growing at this depth; ancestry still nests.
const MAX_INDENT: usize = 6;
/// The connector spine runs under the centre of a 32px avatar.
const SPINE: f32 = 16.;

/// Asks the workspace to close the panel.
pub type OnClose = Arc<dyn Fn(&mut Window, &mut App)>;

pub struct ThreadPanel {
    community: Entity<Community>,
    channel: String,
    root_id: EventId,
    view: ThreadView,
    snapshot: Arc<ThreadSnapshot>,
    entries: Vec<Entry>,
    list: ListState,
    /// Nested branches the reader opened; others stay collapsed.
    expanded: HashSet<EventId>,
    /// The reply the composer answers, when not the root.
    reply_parent: Option<EventId>,
    composer: Entity<TextareaState>,
    placeholder: SharedString,
    sending: bool,
    error: Option<SharedString>,
    on_close: OnClose,
    /// Exact mode: the message to reveal once its thread has loaded.
    revealing: Option<EventId>,
    /// The revealed message, tinted for a moment.
    highlight: Option<EventId>,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ThreadPanel {
    pub fn new(
        community: Entity<Community>,
        channel: String,
        root_id: EventId,
        on_close: OnClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::open(community, channel, root_id, false, on_close, window, cx)
    }

    /// Reveals `message`, any message, in its thread (relay.md §5c.5).
    pub fn exact(
        community: Entity<Community>,
        channel: String,
        message: EventId,
        on_close: OnClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::open(community, channel, message, true, on_close, window, cx)
    }

    fn open(
        community: Entity<Community>,
        channel: String,
        root_id: EventId,
        exact: bool,
        on_close: OnClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = community
            .read(cx)
            .session
            .open_thread(&channel, root_id, exact);
        let snapshot = view.snapshot.borrow().clone();
        let watch = mirror(view.snapshot.clone(), cx, |this: &mut Self, value| {
            this.snapshot = value;
            this.sync_entries();
        });
        let list = ListState::new(0, ListAlignment::Top, px(1000.));
        list.set_follow_mode(FollowMode::Tail);
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .auto_grow(1, 9)
        });
        let mut subscriptions = vec![
            cx.observe(&community, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        this.send(window, cx);
                    }
                },
            ),
        ];
        if exact {
            subscriptions.push(cx.observe_self(|this, cx| this.reveal_target(cx)));
        }
        let weak = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let this = this.read(cx);
            let snapshot = &this.snapshot;
            if event.visible_range.start < LOAD_OLDER_WITHIN
                && event.is_scrolled
                && snapshot.direction == Some(Direction::Older)
                && snapshot.status == ThreadStatus::Ready
                && snapshot.can_load_more
            {
                this.view.load_more();
            }
        });
        let mut panel = Self {
            community,
            channel,
            root_id,
            view,
            snapshot,
            entries: Vec::new(),
            list,
            expanded: HashSet::new(),
            reply_parent: None,
            composer,
            placeholder: SharedString::default(),
            sending: false,
            error: None,
            on_close,
            revealing: exact.then_some(root_id),
            highlight: None,
            _watch: watch,
            _subscriptions: subscriptions,
        };
        panel.sync_entries();
        panel
    }

    pub fn root_id(&self) -> EventId {
        self.root_id
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let composer = this.read(cx).composer.clone();
        composer.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Rebuilds the flattened rows, splicing only what changed so the
    /// reading position holds while older replies load above.
    fn sync_entries(&mut self) {
        let next = entries(&self.snapshot, &self.expanded);
        let prefix = self
            .entries
            .iter()
            .zip(&next)
            .take_while(|(a, b)| a.same(b))
            .count();
        let suffix = self.entries[prefix..]
            .iter()
            .rev()
            .zip(next[prefix..].iter().rev())
            .take_while(|(a, b)| a.same(b))
            .count();
        let old_end = self.entries.len() - suffix;
        self.list
            .splice(prefix..old_end, next.len() - suffix - prefix);
        self.entries = next;
    }

    /// Once the thread around the selected message has loaded, opens the
    /// branches above it, scrolls to it and tints it.
    fn reveal_target(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.revealing else {
            return;
        };
        let snapshot = &self.snapshot;
        if snapshot.target_status == Some(TargetStatus::Loading)
            || matches!(snapshot.status, ThreadStatus::Idle | ThreadStatus::Loading)
        {
            return;
        }
        self.revealing = None;
        if snapshot.target_status != Some(TargetStatus::Ready) {
            return;
        }
        let ancestors = snapshot.tree.ancestors(&id);
        self.expanded.extend(ancestors);
        self.sync_entries();
        let Some(index) = self.entries.iter().position(|e| e.row_id() == Some(id)) else {
            return;
        };
        self.list.scroll_to(ListOffset {
            item_ix: index,
            offset_in_item: px(0.),
        });
        self.highlight = Some(id);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(super::navigation::HIGHLIGHT)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.highlight = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn expand(&mut self, id: EventId, cx: &mut Context<Self>) {
        self.list.pause_following_tail();
        self.expanded.insert(id);
        self.sync_entries();
        cx.notify();
    }

    fn reply_to(&mut self, id: Option<EventId>, window: &mut Window, cx: &mut Context<Self>) {
        self.reply_parent = id;
        self.composer
            .update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        (self.on_close)(window, cx);
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.composer.read(cx).value().trim().to_string();
        let Some(root) = self.snapshot.root.as_ref().map(|root| root.id) else {
            return;
        };
        if content.is_empty() || self.sending || !self.can_send() {
            return;
        }
        self.sending = true;
        let parent = self.reply_parent;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session
                .send(
                    &channel,
                    &content,
                    Default::default(),
                    Vec::new(),
                    Some((root, parent.unwrap_or(root))),
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(()) => {
                        this.error = None;
                        // Keep the new reply in sight: open its branch, or
                        // follow the end for a reply to the root.
                        match parent {
                            Some(parent) => {
                                this.expanded.insert(parent);
                                this.expanded.extend(this.snapshot.tree.ancestors(&parent));
                                if this.reply_parent == Some(parent) {
                                    this.reply_parent = None;
                                }
                                this.sync_entries();
                            }
                            None => this.list.set_follow_mode(FollowMode::Tail),
                        }
                        this.composer
                            .update(cx, |state, cx| state.set_value("", window, cx));
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The reply target, if it is still among the loaded replies.
    fn selected_parent(&self) -> Option<&Arc<MessageRow>> {
        let id = self.reply_parent?;
        self.snapshot.replies.iter().find(|row| row.id == id)
    }

    fn can_send(&self) -> bool {
        self.reply_parent.is_none() || self.selected_parent().is_some()
    }

    fn actions(&self, cx: &mut Context<Self>) -> Actions {
        let session = self.community.read(cx).session.clone();
        let viewer = session.viewer().to_string();
        let react_session = session.clone();
        let channel = self.channel.clone();
        Actions {
            on_open_thread: None,
            on_react: Some(Rc::new(move |row, content, _, cx| {
                // Clicking your own reaction removes it; anyone else's adds yours.
                let own = row
                    .reactions
                    .iter()
                    .find(|group| group.content == content)
                    .and_then(|group| group.events.iter().find(|r| r.author.to_hex() == viewer))
                    .map(|reaction| reaction.id);
                let (session, channel, id, content) = (
                    react_session.clone(),
                    channel.clone(),
                    row.id,
                    content.to_string(),
                );
                cx.foreground_executor()
                    .spawn(async move {
                        let result = match own {
                            Some(reaction) => session.remove(&channel, reaction).await,
                            None => session.react(&channel, id, &content).await,
                        };
                        if let Err(error) = result {
                            tracing::warn!(%error, "reaction failed");
                        }
                    })
                    .detach();
            })),
            on_retry: Some(Rc::new(move |row, _, cx| {
                let (session, id) = (session.clone(), row.id);
                cx.foreground_executor()
                    .spawn(async move {
                        let _ = session.retry_send(id).await;
                    })
                    .detach();
            })),
            ..Default::default()
        }
    }

    fn retry(&self) {
        let snapshot = &self.snapshot;
        if snapshot.direction == Some(Direction::Older)
            && snapshot.read_kind == Some(ReadKind::Older)
            && snapshot.root.is_some()
        {
            self.view.load_more();
        } else {
            self.view.refresh();
        }
    }

    fn composer_view(&self, read_only: bool, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        if read_only {
            return div()
                .mx(px(16.))
                .mb(px(16.))
                .p(px(12.))
                .rounded(px(20.))
                .border_1()
                .border_color(c.border_standard)
                .bg(c.surface_panel)
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("Join this channel to send messages.")
                .into_any_element();
        }
        let profiles = &self.community.read(cx).snapshot.profiles;
        let context = self.reply_parent.map(|_| match self.selected_parent() {
            Some(parent) => {
                let name = message::display_name(profiles, &parent.author.to_hex());
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(12.))
                    .bg(c.surface_inset)
                    .text_caption()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_color(c.text_subtle)
                                    .child(format!("Replying to {name}")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(c.text_standard)
                                    .child(parent.content.clone()),
                            ),
                    )
                    .child(
                        IconButton::new("cancel-reply", IconName::X)
                            .size(IconButtonSize::Sm)
                            .tooltip("Cancel reply target")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.reply_to(None, window, cx)),
                            ),
                    )
                    .into_any_element()
            }
            None => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .text_caption()
                .text_color(c.text_subtle)
                .child("Reply target is no longer available.")
                .child(
                    Button::new("cancel-missing-reply", "Cancel reply target")
                        .size(ButtonSize::Sm)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.reply_to(None, window, cx)),
                        ),
                )
                .into_any_element(),
        });
        let send_enabled = !self.composer.read(cx).value().trim().is_empty() && self.can_send();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(16.))
            .mx(px(16.))
            .mb(px(16.))
            .pt(px(12.))
            .px(px(6.))
            .pb(px(6.))
            .rounded(px(20.))
            .border_1()
            .border_color(c.border_standard)
            .bg(c.surface_panel)
            .shadow_xs()
            .text_body()
            .child(
                div()
                    .pl(px(4.))
                    .pt(px(4.))
                    .child(Textarea::new(&self.composer).appearance(false)),
            )
            .children(context)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .gap(px(4.))
                            .child(
                                IconButton::new("thread-mention", IconName::At)
                                    .size(IconButtonSize::Sm),
                            )
                            .child(
                                IconButton::new("thread-emoji", IconName::Smiley)
                                    .size(IconButtonSize::Sm),
                            )
                            .child(
                                IconButton::new("thread-format", IconName::TextAa)
                                    .size(IconButtonSize::Sm),
                            ),
                    )
                    .child(
                        IconButton::new("thread-send", IconName::ArrowUp)
                            .size(IconButtonSize::Sm)
                            .disabled(!send_enabled)
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    ),
            )
            .into_any_element()
    }

    /// Follows the root author's name into the placeholder.
    fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = &self.snapshot.root else {
            return;
        };
        let profiles = &self.community.read(cx).snapshot.profiles;
        let name = message::display_name(profiles, &root.author.to_hex());
        let placeholder: SharedString = format!("Reply in thread to {name}").into();
        if placeholder != self.placeholder {
            self.placeholder = placeholder.clone();
            self.composer.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx)
            });
        }
    }
}

impl Render for ThreadPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_placeholder(window, cx);
        let actions = self.actions(cx);
        let c = *cx.colors();
        let snapshot = self.snapshot.clone();
        let community = self.community.read(cx);
        let profiles = community.snapshot.profiles.clone();
        let viewer = community.session.viewer().to_string();
        let read_only = community
            .snapshot
            .channels
            .iter()
            .find(|channel| channel.id == self.channel)
            .is_some_and(|channel| channel.read_only);

        let weak = cx.entity().downgrade();
        let header = PanelHeader::new().joined(true).leading(
            Tabs::new(TabVariant::Navigation).tab(
                Tab::new("thread-tab", "Thread")
                    .icon(IconName::ChatCircle)
                    .selected(true)
                    .on_close(move |_, window, cx| {
                        let _ = weak.update(cx, |this, cx| this.close(window, cx));
                    }),
            ),
        );

        let note = |text: SharedString| {
            div()
                .px(px(24.))
                .py(px(8.))
                .text_caption()
                .text_color(c.text_subtle)
                .child(text)
        };

        let body = if self.entries.is_empty() {
            div().flex_1().into_any_element()
        } else {
            let rows = Rc::new(Rows {
                viewer: viewer.clone(),
                directory: Directory {
                    profiles: profiles.clone(),
                    channels: community.snapshot.channels.clone(),
                    media: community.session.media().clone(),
                },
                actions,
                interaction: Interaction::default(),
            });
            let entries = self.entries.clone();
            let weak = cx.entity().downgrade();
            let highlight = self.highlight;
            list(self.list.clone(), move |index, _, cx| {
                let Some(entry) = entries.get(index) else {
                    return div().into_any_element();
                };
                let row = render_entry(entry, &rows, &weak, cx);
                div()
                    .px(px(24.))
                    .child(match highlight.is_some() && entry.row_id() == highlight {
                        true => super::navigation::highlighted(row, cx),
                        false => row,
                    })
                    .into_any_element()
            })
            .flex_1()
            .w_full()
            .into_any_element()
        };

        let loading_older = snapshot.status == ThreadStatus::Loading
            && snapshot.read_kind == Some(ReadKind::Older)
            && snapshot.root.is_some();
        let mut status: Vec<AnyElement> = Vec::new();
        if snapshot.status == ThreadStatus::Loading && self.entries.is_empty() {
            status.push(note("Loading thread…".into()).into_any_element());
        }
        if let Some(error) = &snapshot.error {
            status.push(note(error.clone().into()).into_any_element());
        } else if snapshot.limited {
            status.push(note("Thread history limit reached.".into()).into_any_element());
        }
        if snapshot.error.is_some() {
            status.push(
                div()
                    .px(px(16.))
                    .pb(px(12.))
                    .child(
                        Button::new("retry-thread", "Retry thread")
                            .size(ButtonSize::Sm)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.retry();
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }

        div()
            .id("thread-panel")
            .flex()
            .flex_col()
            .size_full()
            .text_body_sm()
            .text_color(c.text_standard)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(window, cx);
                }
            }))
            .child(header)
            .when(loading_older, |this| {
                this.child(
                    div()
                        .flex()
                        .justify_center()
                        .h(px(32.))
                        .items_center()
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child("Loading earlier replies…"),
                )
            })
            .child(body)
            .children(status)
            .child(div().h(px(12.)).flex_none())
            .children(self.error.clone().map(|error| {
                div()
                    .mx(px(16.))
                    .mb(px(8.))
                    .text_caption()
                    .text_color(c.text_warning)
                    .child(error)
            }))
            .when(snapshot.root.is_some(), |this| {
                this.child(self.composer_view(read_only, cx))
            })
    }
}

/// Development only: `BUZZ_DEV_OPEN_THREAD=<event id>` opens that thread
/// beside the first channel selected, so headless runs can screenshot it.
pub fn dev_open_thread() -> Option<EventId> {
    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if TAKEN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    EventId::from_hex(&std::env::var("BUZZ_DEV_OPEN_THREAD").ok()?).ok()
}

/// The board's columns with the thread docked on the right (ui.md §4.1):
/// `minmax(320px, 1.1fr) minmax(316px, 1fr)`.
pub fn board(channel: AnyElement, thread: Option<Entity<ThreadPanel>>, cx: &App) -> AnyElement {
    let Some(thread) = thread else {
        return channel;
    };
    let c = cx.colors();
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(
            div()
                .flex()
                .flex_grow(1.1)
                .flex_basis(px(0.))
                .min_w(px(320.))
                .h_full()
                .child(channel),
        )
        .child(
            div()
                .flex()
                .flex_grow(1.)
                .flex_basis(px(0.))
                .min_w(px(316.))
                .h_full()
                .border_l_1()
                .border_color(c.border_standard)
                .child(thread),
        )
        .into_any_element()
}

/// One row of the flattened reply tree.
#[derive(Clone)]
enum Entry {
    Root(Item),
    /// The root is missing or unreadable.
    Missing,
    /// A top-level reply whose parent is not loaded.
    Orphan(EventId),
    Reply(Reply),
    Summary(Summary),
}

impl Entry {
    fn row_id(&self) -> Option<EventId> {
        match self {
            Entry::Root(item) => Some(item.row.id),
            Entry::Reply(reply) => Some(reply.item.row.id),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Reply {
    item: Item,
    depth: usize,
    /// For each level `1..=depth`: the branch at that level continues
    /// below this row.
    rails: Vec<bool>,
    /// Children are shown, so a spine runs down from the avatar.
    open: bool,
}

/// A collapsed branch: "N replies" under its parent.
#[derive(Clone)]
struct Summary {
    parent: Arc<MessageRow>,
    depth: usize,
    rails: Vec<bool>,
    count: usize,
    participants: Vec<String>,
}

impl Entry {
    /// Renders the same, so the list keeps its measurement.
    fn same(&self, other: &Self) -> bool {
        let same_item =
            |a: &Item, b: &Item| Arc::ptr_eq(&a.row, &b.row) && a.head == b.head && a.day == b.day;
        match (self, other) {
            (Self::Root(a), Self::Root(b)) => same_item(a, b),
            (Self::Missing, Self::Missing) => true,
            (Self::Orphan(a), Self::Orphan(b)) => a == b,
            (Self::Reply(a), Self::Reply(b)) => {
                same_item(&a.item, &b.item)
                    && a.depth == b.depth
                    && a.rails == b.rails
                    && a.open == b.open
            }
            (Self::Summary(a), Self::Summary(b)) => {
                Arc::ptr_eq(&a.parent, &b.parent)
                    && a.depth == b.depth
                    && a.rails == b.rails
                    && a.count == b.count
                    && a.participants == b.participants
            }
            _ => false,
        }
    }
}

/// Flattens the reply tree as buzz-app's `ThreadPanel` renders it: direct
/// replies in order, each nested branch collapsed behind a summary until
/// the reader opens it, one level at a time.
fn entries(snapshot: &ThreadSnapshot, expanded: &HashSet<EventId>) -> Vec<Entry> {
    let mut walk = Walk {
        tree: &snapshot.tree,
        expanded,
        root: snapshot.root.as_ref().map(|root| root.id),
        previous: snapshot.root.clone(),
        previous_parent: None,
        last_day: snapshot.root.as_ref().map(|root| root.created_at),
        out: Vec::new(),
    };
    match &snapshot.root {
        Some(root) => walk.out.push(Entry::Root(Item {
            row: root.clone(),
            head: true,
            day: Some(time::day(root.created_at).into()),
        })),
        None if matches!(snapshot.status, ThreadStatus::Ready | ThreadStatus::Error) => {
            walk.out.push(Entry::Missing)
        }
        None => {}
    }
    walk.branch(None, &snapshot.tree.top_level, 0, &mut Vec::new());
    walk.out
}

struct Walk<'a> {
    tree: &'a ReplyTree,
    expanded: &'a HashSet<EventId>,
    root: Option<EventId>,
    /// Groups only continue between siblings; a collapsed branch ends one.
    previous: Option<Arc<MessageRow>>,
    previous_parent: Option<EventId>,
    last_day: Option<u64>,
    out: Vec<Entry>,
}

impl Walk<'_> {
    fn branch(
        &mut self,
        parent: Option<EventId>,
        rows: &[Arc<MessageRow>],
        depth: usize,
        rails: &mut Vec<bool>,
    ) {
        for (index, row) in rows.iter().enumerate() {
            let last = index + 1 == rows.len();
            let children = self.tree.children_of(&row.id);
            let open = !children.is_empty() && self.expanded.contains(&row.id);
            if parent.is_none() && row.reply_parent_id.is_some_and(|p| Some(p) != self.root) {
                self.out.push(Entry::Orphan(row.id));
            }
            let continues = self.previous_parent == parent
                && self
                    .previous
                    .as_ref()
                    .is_some_and(|previous| message::continues(previous, row));
            let day = self
                .last_day
                .is_none_or(|day| !time::same_day(day, row.created_at))
                .then(|| time::day(row.created_at).into());
            if depth > 0 {
                rails.push(!last);
            }
            self.out.push(Entry::Reply(Reply {
                item: Item {
                    row: row.clone(),
                    head: !continues,
                    day,
                },
                depth,
                rails: rails.clone(),
                open,
            }));
            self.previous = (children.is_empty() || open).then(|| row.clone());
            self.previous_parent = parent;
            self.last_day = Some(row.created_at);
            if open {
                self.branch(Some(row.id), children, depth + 1, rails);
            } else if !children.is_empty() {
                let mut descendants = Vec::new();
                self.collect(&row.id, &mut descendants);
                let mut participants: Vec<String> = Vec::new();
                for reply in &descendants {
                    let author = reply.author.to_hex();
                    if !participants.contains(&author) {
                        participants.push(author);
                    }
                }
                self.out.push(Entry::Summary(Summary {
                    parent: row.clone(),
                    depth,
                    rails: rails.clone(),
                    count: descendants.len(),
                    participants,
                }));
            }
            if depth > 0 {
                rails.pop();
            }
        }
    }

    fn collect(&self, id: &EventId, out: &mut Vec<Arc<MessageRow>>) {
        for child in self.tree.children_of(id) {
            out.push(child.clone());
            self.collect(&child.id, out);
        }
    }
}

fn indent(depth: usize) -> f32 {
    INDENT * depth.min(MAX_INDENT) as f32
}

/// A 1px connector segment at the spine of `level`'s parent.
fn rail(level: usize, color: Hsla) -> Div {
    div()
        .absolute()
        .left(px(indent(level - 1) + SPINE))
        .w(px(1.))
        .bg(color)
}

/// Pass-through lines for every branch that continues past this row.
fn rails(rails: &[bool], color: Hsla) -> Vec<Div> {
    rails
        .iter()
        .enumerate()
        .filter(|(index, continues)| **continues && *index < MAX_INDENT)
        .map(|(index, _)| rail(index + 1, color).top_0().bottom_0())
        .collect()
}

fn render_entry(
    entry: &Entry,
    rows: &Rc<Rows>,
    panel: &WeakEntity<ThreadPanel>,
    cx: &mut App,
) -> AnyElement {
    let c = *cx.colors();
    match entry {
        Entry::Root(item) => message::render_in(Layout::Thread, item, rows, cx),
        Entry::Missing => div()
            .py(px(32.))
            .px(px(16.))
            .text_body_sm()
            .text_color(c.text_subtle)
            .child("Original message unavailable.")
            .into_any_element(),
        Entry::Orphan(_) => div()
            .pt(px(8.))
            .text_caption()
            .text_color(c.text_subtle)
            .child("Earlier reply unavailable in loaded history.")
            .into_any_element(),
        Entry::Reply(reply) => render_reply(reply, rows, panel, cx),
        Entry::Summary(summary) => render_summary(summary, &rows.directory.profiles, panel, cx),
    }
}

fn render_reply(
    reply: &Reply,
    rows: &Rc<Rows>,
    panel: &WeakEntity<ThreadPanel>,
    cx: &mut App,
) -> AnyElement {
    let c = *cx.colors();
    let line = c.border_standard;
    let depth = reply.depth;
    let row = &reply.item.row;
    let layout = if depth > 0 {
        Layout::Nested
    } else {
        Layout::Thread
    };
    // The day divider is drawn here so connectors can run through it.
    let mut divider = None;
    if let Some(day) = &reply.item.day {
        let mut lines = rails(&reply.rails[..depth.saturating_sub(1)], line);
        if depth > 0 && depth <= MAX_INDENT {
            lines.push(rail(depth, line).top_0().bottom_0());
        }
        divider = Some(
            div()
                .relative()
                .pl(px(indent(depth)))
                .children(lines)
                .child(message::day_divider(day.clone(), cx)),
        );
    }
    let item = Item {
        day: None,
        ..reply.item.clone()
    };
    let top = if item.head { 14. } else { 2. };
    let avatar = if depth > 0 { 24. } else { 32. };
    let group = SharedString::from(format!("thread-reply-{}", row.id.to_hex()));
    let target = row.id;
    let panel = panel.clone();
    let reply_button = div()
        .absolute()
        .top(px(top - 2.))
        .right_0()
        .invisible()
        .group_hover(group.clone(), |style| style.visible())
        .child(
            IconButton::new(
                SharedString::from(format!("reply-{}", row.id.to_hex())),
                IconName::ChatCircle,
            )
            .size(IconButtonSize::Sm)
            .tooltip("Reply")
            .on_click(move |_, window, cx| {
                let _ = panel.update(cx, |this, cx| this.reply_to(Some(target), window, cx));
            }),
        );
    let mut message_row = div()
        .group(group)
        .relative()
        .pl(px(indent(depth)))
        .children(rails(&reply.rails[..depth.saturating_sub(1)], line));
    if depth > 0 && depth <= MAX_INDENT {
        // The elbow into this row's avatar, and the parent's spine past it.
        message_row = message_row.child(
            rail(depth, line)
                .top_0()
                .h(px(top + avatar / 2.))
                .w(px(INDENT - SPINE - 4.))
                .bg(transparent_black())
                .border_l_1()
                .border_b_1()
                .border_color(line)
                .rounded_bl(px(12.)),
        );
        if reply.rails[depth - 1] {
            message_row = message_row.child(rail(depth, line).top_0().bottom_0());
        }
    }
    if reply.open && depth < MAX_INDENT {
        message_row =
            message_row.child(rail(depth + 1, line).top(px(top + avatar + 4.)).bottom_0());
    }
    let message_row = message_row
        .child(message::render_in(layout, &item, rows, cx))
        .child(reply_button);
    div()
        .flex()
        .flex_col()
        .children(divider)
        .child(message_row)
        .into_any_element()
}

fn render_summary(
    summary: &Summary,
    profiles: &Profiles,
    panel: &WeakEntity<ThreadPanel>,
    cx: &mut App,
) -> AnyElement {
    let c = *cx.colors();
    let mut avatars = div().flex();
    for (index, author) in summary.participants.iter().take(3).enumerate() {
        let mut avatar =
            Avatar::new(message::display_name(profiles, author)).size(AvatarSize::Small);
        if let Some(picture) = profiles.get(author).and_then(|p| p.picture.clone()) {
            avatar = avatar.image(picture);
        }
        avatars = avatars.child(
            div()
                .when(index > 0, |d| d.ml(px(-4.)))
                .rounded_full()
                .p(px(1.5))
                .bg(c.surface_panel)
                .child(avatar),
        );
    }
    let label = if summary.count == 1 {
        "1 reply".to_string()
    } else {
        format!("{} replies", summary.count)
    };
    let target = summary.parent.id;
    let panel = panel.clone();
    div()
        .relative()
        .pl(px(indent(summary.depth) + 40.))
        .children(rails(&summary.rails, c.border_standard))
        .child(
            div()
                .id(SharedString::from(format!(
                    "branch-{}",
                    summary.parent.id.to_hex()
                )))
                .flex()
                .items_center()
                .gap(px(8.))
                .ml(px(-4.))
                .mt(px(4.))
                .p(px(4.))
                .pr(px(6.))
                .min_h(px(32.))
                .rounded(px(10.))
                .cursor_pointer()
                .text_caption()
                .text_color(c.text_standard)
                .hover(|style| style.bg(c.affordance_subtle_hover))
                .child(avatars)
                .child(label)
                .on_click(move |_, _, cx| {
                    let _ = panel.update(cx, |this, cx| this.expand(target, cx));
                }),
        )
        .into_any_element()
}
