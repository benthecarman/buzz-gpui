//! The channel page: header, timeline and composer (ui.md §4.1–4.8,
//! §4.12).

use super::channel_menu::{ChannelActions, JoinNotice};
use super::composer::{Composer, ComposerEvent, Submission};
use super::markdown::Directory;
use super::message::{self, Item, Rows};
use super::message_actions::{Hooks, MessageActions};
use crate::community::{ChannelModel, Community};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::{ChannelSummary, EventId, TimelineStatus};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, Icon, IconName, PanelHeader, TypeStyled,
    tokens::shadow,
};
use gpui_kit::component::input::TextareaState;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

/// Older history loads when the reader is this many rows from the top.
const LOAD_OLDER_WITHIN: usize = 5;
/// The jump pill shows this far from the bottom (`JumpToLatestButton`).
const JUMP_DISTANCE: Pixels = px(80.);

/// Whether [`ChannelPage::reveal`] found the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reveal {
    /// The first page has not arrived yet.
    Loading,
    Shown,
    /// Not a top-level message in the loaded window.
    Absent,
}

/// Asks the workspace to open a thread panel.
pub type OpenThread = Arc<dyn Fn(Arc<MessageRow>, &mut Window, &mut App)>;

/// The viewer left or hid this channel; the workspace closes the page.
pub struct ChannelClosed;

pub struct ChannelPage {
    community: Entity<Community>,
    channel: ChannelSummary,
    model: Entity<ChannelModel>,
    items: Vec<Item>,
    /// Row 0 is the top edge (load older); messages follow.
    list: ListState,
    composer: Entity<Composer>,
    input: Entity<TextareaState>,
    /// Row actions: the hover bar, menus, reactions and dialogs.
    message_actions: Entity<MessageActions>,
    error: Option<SharedString>,
    /// Rows that arrived while the reader was scrolled up.
    unseen: usize,
    /// The reader is at least [`JUMP_DISTANCE`] from the bottom.
    away: bool,
    /// A message just revealed by navigation, tinted for a moment.
    highlight: Option<EventId>,
    _subscriptions: Vec<Subscription>,
    /// The header's members button and actions menu.
    actions: Entity<ChannelActions>,
    /// Join for a read-only preview (§4.12).
    join: Entity<JoinNotice>,
}

impl EventEmitter<ChannelClosed> for ChannelPage {}

impl ChannelPage {
    pub fn new(
        community: Entity<Community>,
        channel: ChannelSummary,
        on_open_thread: OpenThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = community.update(cx, |community, cx| community.open_channel(&channel.id, cx));
        let list = ListState::new(1, ListAlignment::Bottom, px(1000.));
        list.set_follow_mode(FollowMode::Tail);
        let placeholder = composer_placeholder(&channel, &community.read(cx).snapshot.profiles);
        let composer = cx.new(|cx| {
            Composer::new(
                community.clone(),
                channel.id.clone(),
                None,
                placeholder,
                window,
                cx,
            )
        });
        let input = composer.read(cx).input();
        let message_actions =
            Self::message_actions(&community, &channel, &composer, &on_open_thread, window, cx);
        let subscriptions = vec![
            cx.observe(&message_actions, |_, _, cx| cx.notify()),
            cx.observe_in(&model, window, |this, _, window, cx| {
                this.sync_items(window, cx)
            }),
            cx.observe_in(&community, window, |this, community, window, cx| {
                // Joining, leaving or editing changes the summary.
                if let Some(current) = community
                    .read(cx)
                    .snapshot
                    .channels
                    .iter()
                    .find(|c| c.id == this.channel.id)
                {
                    this.channel = current.clone();
                }
                // A DM is named after its people, whose profiles may arrive
                // later.
                let placeholder =
                    composer_placeholder(&this.channel, &community.read(cx).snapshot.profiles);
                this.composer.update(cx, |composer, cx| {
                    composer.set_placeholder(placeholder, window, cx)
                });
                cx.notify()
            }),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &ComposerEvent, window, cx| match event {
                    ComposerEvent::Submit(submission) => this.send(submission.clone(), window, cx),
                    ComposerEvent::EditLast => this.edit_last(window, cx),
                    ComposerEvent::CancelReply => {}
                    ComposerEvent::RetryEdit => this
                        .message_actions
                        .update(cx, |actions, cx| actions.retry_edit(cx)),
                },
            ),
        ];
        let weak = cx.entity().downgrade();
        let model_for_scroll = model.clone();
        list.set_scroll_handler(move |event, _, cx| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            // Row 0 is the edge.
            let near_top = event.visible_range.start < LOAD_OLDER_WITHIN + 1;
            let items = &this.read(cx).items;
            let start = event.visible_range.start.saturating_sub(1).min(items.len());
            let end = event.visible_range.end.saturating_sub(1).min(items.len());
            let ids = items[start..end]
                .iter()
                .map(|item| item.row.id)
                .take(128)
                .collect();
            let model = model_for_scroll.read(cx);
            model.view.viewport(ids, event.is_following_tail);
            if near_top && model.snapshot.has_more && !model.snapshot.loading_older {
                model.load_older();
            }
        });
        let page_handle = cx.entity().downgrade();
        let on_removed: super::channel_menu::OnRemoved = std::rc::Rc::new(move |_, cx| {
            let _ = page_handle.update(cx, |_, cx| cx.emit(ChannelClosed));
        });
        let actions =
            cx.new(|_| ChannelActions::new(community.clone(), channel.clone(), on_removed));
        let join = cx.new(|_| JoinNotice::new(community.clone(), channel.clone()));
        let mut page = Self {
            actions,
            join,
            community,
            channel,
            model,
            items: Vec::new(),
            list,
            composer,
            input,
            message_actions,
            error: None,
            unseen: 0,
            away: false,
            highlight: None,
            _subscriptions: subscriptions,
        };
        page.sync_items(window, cx);
        page
    }

    pub fn channel_id(&self) -> &str {
        &self.channel.id
    }

    pub fn channel(&self) -> &ChannelSummary {
        &self.channel
    }

    pub fn composer(&self) -> Entity<TextareaState> {
        self.input.clone()
    }

    /// Scrolls a top-level message into view and tints it briefly, paging in
    /// older history while it may still be there. A reply is not revealed
    /// here; there is no "around message" query (relay.md §5c.5).
    pub fn reveal(&mut self, id: EventId, cx: &mut Context<Self>) -> Reveal {
        let index = self
            .items
            .iter()
            .position(|item| item.row.id == id && item.row.thread_root_id.is_none());
        let Some(index) = index else {
            // A cached window may be missing what the relay now has; older
            // history is paged in until the message turns up or runs out.
            let model = self.model.read(cx);
            let snapshot = &model.snapshot;
            return match snapshot.status {
                TimelineStatus::Loading => Reveal::Loading,
                TimelineStatus::Ready if snapshot.stale || snapshot.loading_older => {
                    Reveal::Loading
                }
                TimelineStatus::Ready if snapshot.has_more && !snapshot.history_limited => {
                    model.load_older();
                    Reveal::Loading
                }
                _ => Reveal::Absent,
            };
        };
        // List row 0 is the edge.
        self.list.scroll_to(ListOffset {
            item_ix: index + 1,
            offset_in_item: px(0.),
        });
        self.highlight = Some(id);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(super::navigation::HIGHLIGHT)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.highlight == Some(id) {
                    this.highlight = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
        Reveal::Shown
    }

    /// Rebuilds the rows, telling the list exactly what was added at either
    /// end so the reading position stays put while history loads above.
    fn sync_items(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.model.read(cx).snapshot.rows.clone();
        let next = message::items(&rows);
        let old: Vec<_> = self.items.iter().map(|item| item.row.id).collect();
        let new: Vec<_> = next.iter().map(|item| item.row.id).collect();
        match old
            .first()
            .and_then(|first| new.iter().position(|id| id == first))
        {
            Some(prepended) if new[prepended..].starts_with(&old) => {
                // List indices are one past item indices: row 0 is the edge.
                self.list.splice(1..1, prepended);
                let end = prepended + old.len();
                let appended = new.len() - end;
                self.list.splice(end + 1..end + 1, appended);
                // Head flags of the old first row and the row before the
                // appended tail may have changed.
                if !old.is_empty() {
                    self.list.splice(prepended + 1..prepended + 2, 1);
                    self.list.splice(end..end + 1, 1);
                }
                if self.away {
                    self.unseen += appended;
                }
            }
            _ => self.list.reset(new.len() + 1),
        }
        // The edge's state may have changed.
        self.list.splice(0..1, 1);
        self.items = next;
        cx.notify();
    }

    fn session(&self, cx: &App) -> Arc<buzz_relay_client::Session> {
        self.community.read(cx).session.clone()
    }

    fn send(&mut self, submission: Submission, window: &mut Window, cx: &mut Context<Self>) {
        if submission.editing.is_some() {
            self.message_actions
                .update(cx, |actions, cx| actions.save_edit(submission, window, cx));
            return;
        }
        let session = self.session(cx);
        let channel = self.channel.id.clone();
        if submission.editing.is_none() {
            self.list.set_follow_mode(FollowMode::Tail);
            self.unseen = 0;
        }
        let mentions = submission.mentions.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = match &submission.editing {
                Some(row) => session.edit(&channel, row.id, &submission.content).await,
                None => {
                    session
                        .send(
                            &channel,
                            &submission.content,
                            mentions,
                            submission.attachments.clone(),
                            None,
                        )
                        .await
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        this.error = None;
                        this.composer
                            .update(cx, |composer, cx| composer.sent(window, cx));
                    }
                    Err(error) => {
                        this.error = Some(error.into());
                        this.composer.update(cx, |composer, cx| composer.failed(cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_edit(&mut self, row: Arc<MessageRow>, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |composer, cx| composer.start_edit(row, window, cx));
    }

    /// Up in an empty composer edits the viewer's newest editable message
    /// (`lastEditableMessage`).
    fn edit_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let row = self
            .message_actions
            .read(cx)
            .last_editable(self.items.iter().map(|item| &item.row), cx);
        if let Some(row) = row {
            self.start_edit(row, window, cx);
        }
    }

    /// The row actions, wired to this page's composer and thread panel.
    fn message_actions(
        community: &Entity<Community>,
        channel: &ChannelSummary,
        composer: &Entity<Composer>,
        on_open_thread: &OpenThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MessageActions> {
        let page = cx.entity().downgrade();
        let (edit_composer, focus_composer) = (composer.downgrade(), composer.downgrade());
        let (reply, open) = (on_open_thread.clone(), on_open_thread.clone());
        let hooks = Hooks {
            reply: Rc::new(move |row, window, cx| reply(row, window, cx)),
            open_thread: Some(Rc::new(move |row, window, cx| open(row, window, cx))),
            edit: Rc::new(move |row, window, cx| {
                let _ =
                    edit_composer.update(cx, |composer, cx| composer.start_edit(row, window, cx));
            }),
            focus_composer: Rc::new(move |window, cx| {
                let _ = focus_composer.update(cx, |composer, cx| composer.focus(window, cx));
            }),
            thread_root: None,
            find: Rc::new(move |id, cx| {
                page.upgrade().and_then(|page| {
                    page.read(cx)
                        .items
                        .iter()
                        .find(|item| item.row.id == id)
                        .map(|item| item.row.clone())
                })
            }),
            thread: None,
            composer: composer.downgrade(),
        };
        let (community, channel) = (community.clone(), channel.id.clone());
        cx.new(|cx| MessageActions::new(community, channel, hooks, window, cx))
    }

    /// The list's first row: load older, its progress, its retry, or the
    /// history window limit (§4.3).
    fn edge(
        snapshot: &buzz_relay_client::TimelineSnapshot,
        model: Entity<ChannelModel>,
        cx: &App,
    ) -> AnyElement {
        let c = cx.colors();
        let frame = || {
            div()
                .h(px(56.))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_caption()
                .text_color(c.text_subtle)
        };
        if snapshot.history_limited {
            return frame()
                .child("History window limit reached")
                .into_any_element();
        }
        if !snapshot.has_more {
            return div().into_any_element();
        }
        let label = if snapshot.loading_older {
            "Loading older…"
        } else if snapshot.error.is_some() {
            "Retry older messages"
        } else {
            "Load older messages"
        };
        frame()
            .child(
                Button::new("load-older", label)
                    .variant(ButtonVariant::Subtle)
                    .size(ButtonSize::Md)
                    .disabled(snapshot.loading_older)
                    .on_click(move |_, _, cx| model.read(cx).load_older()),
            )
            .into_any_element()
    }

    fn composer_shell(&self) -> AnyElement {
        if self.channel.read_only {
            return self.join.clone().into_any_element();
        }
        self.composer.clone().into_any_element()
    }

    fn jump_pill(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let label = match self.unseen {
            0 => "Jump to latest".to_string(),
            1 => "1 new message".to_string(),
            n => format!("{n} new messages"),
        };
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(2.))
            .flex()
            .justify_center()
            .child(
                div()
                    .id("jump-to-latest")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(32.))
                    .px(px(16.))
                    .rounded(px(12.))
                    .bg(c.glass_4)
                    .shadow({
                        let mut shadows = shadow::sm();
                        shadows.extend(shadow::rims(c.rim_lit, c.rim_shade));
                        shadows
                    })
                    .cursor_pointer()
                    .text_label_sm()
                    .text_color(c.text_standard)
                    .hover(|s| s.bg(c.affordance_subtle_hover))
                    .child(Icon::new(IconName::ArrowDown).size(16.))
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.list.set_follow_mode(FollowMode::Tail);
                        this.unseen = 0;
                        this.away = false;
                        cx.notify();
                    })),
            )
    }
}

/// `MessageComposer`'s label: `Message #<name>`, where a DM's name is its
/// people (or "Notes to self"), still with the `#` as in buzz-app.
fn composer_placeholder(
    channel: &ChannelSummary,
    profiles: &buzz_relay_client::Profiles,
) -> SharedString {
    match channel.channel_type {
        Some(buzz_relay_client::ChannelType::Session) => "Message this session".into(),
        _ => format!(
            "Message #{}",
            super::sidebar::channel_label(channel, profiles)
        )
        .into(),
    }
}

impl Render for ChannelPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let snapshot = self.model.read(cx).snapshot.clone();
        let community = self.community.read(cx);
        let directory = Directory::of(community, Some(&self.channel.members), cx);
        let viewer = community.session.viewer().to_string();
        let typing = community.snapshot.typing.get(&self.channel.id).cloned();

        // Distance from the bottom, for the jump pill.
        let max = self.list.max_offset_for_scrollbar().y;
        let offset = -self.list.scroll_px_offset_for_scrollbar().y;
        self.away = !self.list.is_following_tail() && max - offset >= JUMP_DISTANCE;
        if !self.away {
            self.unseen = 0;
        }

        let header = PanelHeader::new()
            .joined(true)
            .label(
                Some(message::channel_icon(&self.channel)),
                super::sidebar::channel_label(&self.channel, &community.snapshot.profiles),
            )
            .action(self.actions.clone());

        let typing_profiles = community.snapshot.profiles.clone();
        let mut rows = Rows::new(viewer, directory);
        let actions = self.message_actions.clone();
        MessageActions::fill(&actions, &mut rows, cx);
        let rows = Rc::new(rows);
        let items = self.items.clone();
        let timeline = if self.items.is_empty() {
            let text = match &snapshot.status {
                TimelineStatus::Loading => "Loading messages…".to_string(),
                TimelineStatus::Error(error) => error.clone(),
                TimelineStatus::Ready => "No messages yet.".to_string(),
            };
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .py(px(32.))
                .px(px(16.))
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(text)
                .into_any_element()
        } else {
            let model = self.model.clone();
            let edge_snapshot = snapshot.clone();
            let highlight = self.highlight;
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    list(self.list.clone(), move |index, _, cx| {
                        if index == 0 {
                            return div()
                                .px(px(24.))
                                .child(Self::edge(&edge_snapshot, model.clone(), cx))
                                .into_any_element();
                        }
                        match items.get(index - 1) {
                            Some(item) => {
                                let row = message::render(item, &rows, cx);
                                div()
                                    .px(px(24.))
                                    .child(match highlight == Some(item.row.id) {
                                        true => super::navigation::highlighted(row, cx),
                                        false => row,
                                    })
                                    .into_any_element()
                            }
                            None => div().into_any_element(),
                        }
                    })
                    .size_full(),
                )
                .child(div().h(px(12.)))
                .when(self.away, |d| d.child(self.jump_pill(cx)))
                .into_any_element()
        };

        let composer = self.composer_shell();

        let overlays = self
            .message_actions
            .update(cx, |actions, cx| actions.overlays(window, cx));
        let status = self
            .message_actions
            .update(cx, |actions, cx| actions.status(cx));

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .text_body_sm()
            .text_color(c.text_standard)
            .child(header)
            .child(timeline)
            .children(self.error.clone().map(|error| {
                div()
                    .mx(px(16.))
                    .mb(px(8.))
                    .text_caption()
                    .text_color(c.text_warning)
                    .child(error)
            }))
            .children(status)
            .child(
                div()
                    .relative()
                    .children(
                        typing.and_then(|who| super::typing::overlay(&who, &typing_profiles, cx)),
                    )
                    .child(composer),
            )
            .children(overlays)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn dms_are_named_after_their_people() {
        use buzz_relay_client::{ChannelType, Profile};
        use std::collections::HashMap;
        let (bob, carol) = ("b".repeat(64), "c".repeat(64));
        let named = |name: &str| {
            Arc::new(Profile {
                name: name.into(),
                ..Default::default()
            })
        };
        let profiles: buzz_relay_client::Profiles = Arc::new(HashMap::from([
            (bob.clone(), named("Bob")),
            (carol.clone(), named("Carol")),
        ]));
        let channel = |kind, participants: Option<Vec<String>>| ChannelSummary {
            name: "DM".into(),
            channel_type: Some(kind),
            participants,
            ..Default::default()
        };
        let group = channel(ChannelType::Dm, Some(vec![bob, carol]));
        assert_eq!(
            composer_placeholder(&group, &profiles),
            "Message #Bob, Carol"
        );
        let notes = channel(ChannelType::Dm, Some(Vec::new()));
        assert_eq!(
            composer_placeholder(&notes, &profiles),
            "Message #Notes to self"
        );
        let stream = ChannelSummary {
            name: "general".into(),
            ..channel(ChannelType::Stream, None)
        };
        assert_eq!(composer_placeholder(&stream, &profiles), "Message #general");
        let session = channel(ChannelType::Session, None);
        assert_eq!(
            composer_placeholder(&session, &profiles),
            "Message this session"
        );
    }
}
