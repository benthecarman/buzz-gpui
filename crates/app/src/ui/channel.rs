//! The channel page: header, timeline and composer (ui.md §4.1–4.8,
//! §4.12).

use super::markdown::Directory;
use super::message::{self, Actions, Command, Hover, Interaction, Item, Rows};
use crate::community::{ChannelModel, Community};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::{ChannelSummary, EventId, TimelineStatus, message_link};
use buzz_ui::{
    ActiveTheme, AlertDialog, Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize,
    IconName, PanelHeader, Scrim, Toast, ToastTone, TypeStyled, tokens::shadow,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Older history loads when the reader is this many rows from the top.
const LOAD_OLDER_WITHIN: usize = 5;
/// The jump pill shows this far from the bottom (`JumpToLatestButton`).
const JUMP_DISTANCE: Pixels = px(80.);
/// `useToastNotification`'s timeout.
const TOAST_DURATION: Duration = Duration::from_secs(4);

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

pub struct ChannelPage {
    community: Entity<Community>,
    channel: ChannelSummary,
    model: Entity<ChannelModel>,
    items: Vec<Item>,
    /// Row 0 is the top edge (load older); messages follow.
    list: ListState,
    composer: Entity<TextareaState>,
    placeholder: SharedString,
    sending: bool,
    error: Option<SharedString>,
    on_open_thread: OpenThread,
    interaction: Interaction,
    /// The composer is editing this message (§4.8 edit mode).
    editing: Option<Arc<MessageRow>>,
    /// The delete confirmation is open for this message.
    deleting: Option<Arc<MessageRow>>,
    toast: Option<(SharedString, usize)>,
    toasts: usize,
    /// Rows that arrived while the reader was scrolled up.
    unseen: usize,
    /// The reader is at least [`JUMP_DISTANCE`] from the bottom.
    away: bool,
    /// A message just revealed by navigation, tinted for a moment.
    highlight: Option<EventId>,
    _subscriptions: Vec<Subscription>,
}

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
        let placeholder: SharedString = match channel.channel_type {
            Some(buzz_relay_client::ChannelType::Dm) => format!("Message {}", channel.name),
            _ => format!("Message #{}", channel.name),
        }
        .into();
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder.clone())
                .submit_on_enter(true)
                .auto_grow(1, 9)
        });
        let subscriptions = vec![
            cx.observe(&model, |this, _, cx| this.sync_items(cx)),
            cx.observe(&community, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
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
        let mut page = Self {
            community,
            channel,
            model,
            items: Vec::new(),
            list,
            composer,
            placeholder,
            sending: false,
            error: None,
            on_open_thread,
            interaction: Interaction::default(),
            editing: None,
            deleting: None,
            toast: None,
            toasts: 0,
            unseen: 0,
            away: false,
            highlight: None,
            _subscriptions: subscriptions,
        };
        page.sync_items(cx);
        page
    }

    pub fn channel_id(&self) -> &str {
        &self.channel.id
    }

    pub fn composer(&self) -> Entity<TextareaState> {
        self.composer.clone()
    }

    /// Scrolls a top-level message into view and tints it briefly. A reply,
    /// or a message outside the loaded window, is not revealed here; there
    /// is no "around message" query (relay.md §5c.5).
    pub fn reveal(&mut self, id: EventId, cx: &mut Context<Self>) -> Reveal {
        let index = self
            .items
            .iter()
            .position(|item| item.row.id == id && item.row.thread_root_id.is_none());
        let Some(index) = index else {
            // A cached window may be missing what the relay now has.
            let snapshot = &self.model.read(cx).snapshot;
            return match snapshot.status {
                TimelineStatus::Loading => Reveal::Loading,
                TimelineStatus::Ready if snapshot.stale => Reveal::Loading,
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
    fn sync_items(&mut self, cx: &mut Context<Self>) {
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
        if let Some(editing) = &self.editing
            && !new.contains(&editing.id)
        {
            self.editing = None;
        }
        self.items = next;
        cx.notify();
    }

    fn session(&self, cx: &App) -> Arc<buzz_relay_client::Session> {
        self.community.read(cx).session.clone()
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.composer.read(cx).value().trim().to_string();
        if content.is_empty() || self.sending {
            return;
        }
        let session = self.session(cx);
        let channel = self.channel.id.clone();
        let editing = self.editing.clone();
        if let Some(row) = &editing
            && content == edit_text(row).trim()
        {
            self.cancel_edit(window, cx);
            return;
        }
        self.sending = true;
        if editing.is_none() {
            self.list.set_follow_mode(FollowMode::Tail);
            self.unseen = 0;
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = match &editing {
                Some(row) => session.edit(&channel, row.id, &content).await,
                None => {
                    session
                        .send(&channel, &content, Default::default(), None)
                        .await
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(()) => {
                        this.error = None;
                        if this.editing.is_some() {
                            this.cancel_edit(window, cx);
                        } else {
                            this.composer
                                .update(cx, |state, cx| state.set_value("", window, cx));
                        }
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_edit(&mut self, row: Arc<MessageRow>, window: &mut Window, cx: &mut Context<Self>) {
        let text = edit_text(&row);
        self.editing = Some(row);
        self.composer.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_placeholder("Edit message", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        let placeholder = self.placeholder.clone();
        self.composer.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.set_placeholder(placeholder, window, cx);
        });
        cx.notify();
    }

    fn show_toast(&mut self, text: &str, cx: &mut Context<Self>) {
        self.toasts += 1;
        let generation = self.toasts;
        self.toast = Some((text.to_string().into(), generation));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|(_, g)| *g == generation) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn command(
        &mut self,
        row: Arc<MessageRow>,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            Command::CopyLink => {
                let link = message_link(&self.channel.id, &row.id);
                cx.write_to_clipboard(ClipboardItem::new_string(link));
                self.show_toast("Link copied", cx);
            }
            Command::CopyMessage => {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_text(&row)));
                self.show_toast("Message copied", cx);
            }
            Command::MarkUnread => {
                let (session, channel) = (self.session(cx), self.channel.id.clone());
                cx.foreground_executor()
                    .spawn(async move {
                        if let Err(error) = session.mark_message_unread(&channel, row.id).await {
                            tracing::warn!(%error, "mark unread failed");
                        }
                    })
                    .detach();
            }
            Command::Edit => self.start_edit(row, window, cx),
            Command::Delete => {
                self.deleting = Some(row);
                cx.notify();
            }
        }
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.deleting.take() else {
            return;
        };
        let (session, channel) = (self.session(cx), self.channel.id.clone());
        cx.spawn(async move |this, cx| {
            if let Err(error) = session.remove(&channel, row.id).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.into());
                    cx.notify();
                });
            }
        })
        .detach();
        cx.notify();
    }

    fn hover(&mut self, id: EventId, target: Hover, hovered: bool, cx: &mut Context<Self>) {
        // A row under the pointer means it left every other row's bar; a bar
        // that vanished while hovered never reports leaving.
        if target == Hover::Row
            && hovered
            && self.interaction.bar_hovered.is_some_and(|bar| bar != id)
        {
            self.interaction.bar_hovered = None;
            cx.notify();
        }
        let slot = match target {
            Hover::Row => &mut self.interaction.hovered,
            Hover::Bar => &mut self.interaction.bar_hovered,
        };
        let next = if hovered {
            Some(id)
        } else if *slot == Some(id) {
            None
        } else {
            *slot
        };
        if *slot != next {
            *slot = next;
            cx.notify();
        }
    }

    fn actions(&self, cx: &mut Context<Self>) -> Actions {
        let session = self.session(cx);
        let channel = self.channel.id.clone();
        let viewer = session.viewer().to_string();
        let react_session = session.clone();
        let react_channel = channel.clone();
        let open = self.on_open_thread.clone();
        let this = cx.entity().downgrade();
        let (hover, popup, command) = (this.clone(), this.clone(), this);
        Actions {
            on_open_thread: Some(Rc::new(move |row, window, cx| {
                open(row.clone(), window, cx)
            })),
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
                    react_channel.clone(),
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
            on_hover: Some(Rc::new(move |id, target, hovered, _, cx| {
                let _ = hover.update(cx, |this, cx| this.hover(id, target, hovered, cx));
            })),
            on_popup: Some(Rc::new(move |open, _, cx| {
                let _ = popup.update(cx, |this, cx| {
                    if this.interaction.open != open {
                        this.interaction.open = open;
                        cx.notify();
                    }
                });
            })),
            on_command: Some(Rc::new(move |row, action, window, cx| {
                let row = row.clone();
                let _ = command.update(cx, |this, cx| this.command(row, action, window, cx));
            })),
        }
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

    fn composer_shell(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        if self.channel.read_only {
            return div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .px(px(16.))
                .py(px(8.))
                .mb(px(8.))
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("Read-only preview · You haven’t joined this conversation.")
                .into_any_element();
        }
        let has_text = !self.composer.read(cx).value().trim().is_empty();
        let editing = self.editing.is_some();
        let header = editing.then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .mt(px(-12.))
                .mx(px(-6.))
                .mb(px(-12.))
                .px(px(12.))
                .py(px(8.))
                .rounded_t(px(19.))
                .bg(c.surface_inset)
                .text_label_sm()
                .text_color(c.text_subtle)
                .child(Icon::new(IconName::PencilSimple).size(18.))
                .child(div().flex_1().child("Editing message"))
                .child(
                    IconButton::new("cancel-edit", IconName::X)
                        .size(IconButtonSize::Sm)
                        .icon_size(18.)
                        .tooltip("Cancel edit")
                        .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))),
                )
        });
        div()
            .flex()
            .flex_col()
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
            .overflow_hidden()
            .text_body()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.editing.is_some() {
                    this.cancel_edit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .children(header)
            .child(
                div()
                    .pl(px(4.))
                    .pt(px(4.))
                    .child(Textarea::new(&self.composer).appearance(false)),
            )
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
                                IconButton::new("mention", IconName::At)
                                    .size(IconButtonSize::Sm)
                                    .tooltip("Mention a member"),
                            )
                            .child(
                                IconButton::new("emoji", IconName::Smiley)
                                    .size(IconButtonSize::Sm)
                                    .tooltip("Insert emoji"),
                            )
                            .child(
                                IconButton::new("format", IconName::TextAa)
                                    .size(IconButtonSize::Sm)
                                    .tooltip("Toggle formatting"),
                            ),
                    )
                    .child(
                        IconButton::new("send", IconName::ArrowUp)
                            .size(IconButtonSize::Sm)
                            .when(has_text, |b| b.variant(ButtonVariant::Prominent))
                            .disabled(!has_text || self.sending)
                            .tooltip(if editing {
                                "Save changes"
                            } else {
                                "Send message"
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    ),
            )
            .into_any_element()
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

    /// A full-window overlay for dialogs and the image viewer.
    fn overlay(window: &Window, child: impl IntoElement) -> impl IntoElement {
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(point(px(0.), px(0.)))
                .child(
                    div()
                        .id("overlay")
                        .w(window.viewport_size().width)
                        .h(window.viewport_size().height)
                        .occlude()
                        .child(child),
                ),
        )
        .with_priority(2)
    }
}

/// What "Edit message" puts in the composer: the body with its attachment
/// Markdown (`messageEditText`).
fn edit_text(row: &MessageRow) -> String {
    row.source_content
        .clone()
        .unwrap_or_else(|| row.content.clone())
}

/// `messageCopyText`: the body, then each attachment's URL.
fn copy_text(row: &MessageRow) -> String {
    std::iter::once(row.content.as_str())
        .chain(row.attachments.iter().map(|a| a.url.as_str()))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl Render for ChannelPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let snapshot = self.model.read(cx).snapshot.clone();
        let community = self.community.read(cx);
        let directory = Directory {
            profiles: community.snapshot.profiles.clone(),
            channels: community.snapshot.channels.clone(),
            media: community.session.media().clone(),
        };
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
                self.channel.name.clone(),
            )
            .action(
                IconButton::new("members", IconName::Users)
                    .size(IconButtonSize::Sm)
                    .tooltip("Channel members"),
            )
            .action(
                IconButton::new("more", IconName::DotsThree)
                    .size(IconButtonSize::Sm)
                    .tooltip("Channel actions"),
            );

        let typing_profiles = community.snapshot.profiles.clone();
        let rows = Rc::new(Rows {
            viewer,
            directory,
            actions: self.actions(cx),
            interaction: self.interaction.clone(),
        });
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

        let composer = self.composer_shell(cx);

        let toast = self.toast.as_ref().map(|(text, generation)| {
            let right = window.viewport_size().width - px(16.) - px(384.);
            deferred(
                anchored()
                    .position_mode(AnchoredPositionMode::Window)
                    .position(point(right, px(128.)))
                    .child(
                        Toast::new(("toast", *generation), text.clone())
                            .tone(ToastTone::Success)
                            .on_dismiss(cx.listener(|this, _, _, cx| {
                                this.toast = None;
                                cx.notify();
                            })),
                    ),
            )
            .with_priority(3)
        });

        let dialog = self.deleting.as_ref().map(|_| {
            Self::overlay(
                window,
                Scrim::new().child(
                    AlertDialog::new("Delete message?")
                        .description(
                            "This requests removal of this message from Buzz’s relay. People may \
                             still have copies. This cannot be undone.",
                        )
                        .action(
                            Button::new("delete-cancel", "Cancel")
                                .variant(ButtonVariant::Subtle)
                                .size(ButtonSize::Sm)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.deleting = None;
                                    cx.notify();
                                })),
                        )
                        .action(
                            Button::new("delete-confirm", "Delete")
                                .variant(ButtonVariant::Destructive)
                                .size(ButtonSize::Sm)
                                .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
                        ),
                ),
            )
        });

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
            .child(
                div()
                    .relative()
                    .children(
                        typing.and_then(|who| super::typing::overlay(&who, &typing_profiles, cx)),
                    )
                    .child(composer),
            )
            .children(toast)
            .children(dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn copies_the_body_then_attachment_urls() {
        use buzz_relay_client::fold::{Attachment, AttachmentKind};
        let mut row = super::super::markdown::tests::row("Look");
        assert_eq!(copy_text(&row), "Look");
        row.attachments = vec![Attachment {
            url: "https://x.test/media/a.png".into(),
            kind: AttachmentKind::Image,
            mime: None,
            size: None,
            name: None,
            duration: None,
            waveform: None,
            voice_note: false,
            dimensions: None,
            blurhash: None,
            preview_url: None,
        }];
        assert_eq!(copy_text(&row), "Look\n\nhttps://x.test/media/a.png");
    }
}
