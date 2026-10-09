//! The channel page: header, timeline and composer (ui.md §4.1–4.8).

use super::message::{self, Actions, Item};
use crate::community::{ChannelModel, Community};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::{ChannelSummary, TimelineStatus};
use buzz_ui::{ActiveTheme, IconButton, IconButtonSize, IconName, PanelHeader, TypeStyled};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::*;
use std::sync::Arc;

/// Older history loads when the reader is this many rows from the top.
const LOAD_OLDER_WITHIN: usize = 5;

pub struct ChannelPage {
    community: Entity<Community>,
    channel: ChannelSummary,
    model: Entity<ChannelModel>,
    items: Vec<Item>,
    list: ListState,
    composer: Entity<TextareaState>,
    sending: bool,
    error: Option<SharedString>,
    /// Asks the workspace to open a thread panel.
    on_open_thread: Arc<dyn Fn(Arc<MessageRow>, &mut Window, &mut App)>,
    _subscriptions: Vec<Subscription>,
}

impl ChannelPage {
    pub fn new(
        community: Entity<Community>,
        channel: ChannelSummary,
        on_open_thread: Arc<dyn Fn(Arc<MessageRow>, &mut Window, &mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = community.update(cx, |community, cx| community.open_channel(&channel.id, cx));
        let list = ListState::new(0, ListAlignment::Bottom, px(1000.));
        list.set_follow_mode(FollowMode::Tail);
        let placeholder = match channel.channel_type {
            Some(buzz_relay_client::ChannelType::Dm) => format!("Message {}", channel.name),
            _ => format!("Message #{}", channel.name),
        };
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder)
                .submit_on_enter(true)
                .auto_grow(1, 9)
        });
        let subscriptions = vec![
            cx.observe(&model, |this, _, cx| this.sync_items(cx)),
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
        let weak = cx.entity().downgrade();
        let model_for_scroll = model.clone();
        list.set_scroll_handler(move |event, _, cx| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let near_top = event.visible_range.start < LOAD_OLDER_WITHIN;
            let ids = this.read(cx).items[event.visible_range.clone()]
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
            sending: false,
            error: None,
            on_open_thread,
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
                self.list.splice(0..0, prepended);
                let end = prepended + old.len();
                self.list.splice(end..end, new.len() - end);
                // Head flags of the old first row and the row before the
                // appended tail may have changed.
                if !old.is_empty() {
                    self.list.splice(prepended..prepended + 1, 1);
                    self.list.splice(end - 1..end, 1);
                }
            }
            _ => self.list.reset(new.len()),
        }
        self.items = next;
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let content = self.composer.read(cx).value().trim().to_string();
        if content.is_empty() || self.sending {
            return;
        }
        self.sending = true;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        self.list.set_follow_mode(FollowMode::Tail);
        cx.spawn_in(window, async move |this, cx| {
            let result = session
                .send(&channel, &content, Default::default(), None)
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(()) => {
                        this.error = None;
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

    fn actions(&self, cx: &mut Context<Self>) -> Actions {
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        let viewer = session.viewer().to_string();
        let react_session = session.clone();
        let react_channel = channel.clone();
        let open = self.on_open_thread.clone();
        Actions {
            on_open_thread: Some(Arc::new(move |row, window, cx| {
                open(row.clone(), window, cx)
            })),
            on_react: Some(Arc::new(move |row, content, _, cx| {
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
            on_retry: Some(Arc::new(move |row, _, cx| {
                let (session, id) = (session.clone(), row.id);
                cx.foreground_executor()
                    .spawn(async move {
                        let _ = session.retry_send(id).await;
                    })
                    .detach();
            })),
        }
    }
}

impl Render for ChannelPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let snapshot = self.model.read(cx).snapshot.clone();
        let community = self.community.read(cx);
        let profiles = community.snapshot.profiles.clone();
        let viewer = community.session.viewer().to_string();
        let typing = community.snapshot.typing.get(&self.channel.id).cloned();
        let read_only = self.channel.read_only;

        let header = PanelHeader::new()
            .joined(true)
            .label(
                Some(message::channel_icon(&self.channel)),
                self.channel.name.clone(),
            )
            .action(IconButton::new("members", IconName::Users).size(IconButtonSize::Sm))
            .action(IconButton::new("more", IconName::DotsThree).size(IconButtonSize::Sm));

        let actions = Arc::new(self.actions(cx));
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
            let profiles = profiles.clone();
            let viewer = viewer.clone();
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    list(self.list.clone(), move |index, _, cx| {
                        match items.get(index) {
                            Some(item) => div()
                                .px(px(24.))
                                .child(message::render(item, &viewer, &profiles, &actions, cx))
                                .into_any_element(),
                            None => div().into_any_element(),
                        }
                    })
                    .size_full(),
                )
                .child(div().h(px(12.)))
                .into_any_element()
        };

        let edge = (snapshot.loading_older || snapshot.history_limited).then(|| {
            div()
                .flex()
                .justify_center()
                .h(px(32.))
                .items_center()
                .text_caption()
                .text_color(c.text_subtle)
                .child(if snapshot.history_limited {
                    "History window limit reached"
                } else {
                    "Loading older…"
                })
        });

        let composer = if read_only {
            div()
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
                .into_any_element()
        } else {
            let send_enabled = !self.composer.read(cx).value().trim().is_empty();
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
                .text_body()
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
                                        .size(IconButtonSize::Sm),
                                )
                                .child(
                                    IconButton::new("emoji", IconName::Smiley)
                                        .size(IconButtonSize::Sm),
                                )
                                .child(
                                    IconButton::new("format", IconName::TextAa)
                                        .size(IconButtonSize::Sm),
                                ),
                        )
                        .child(
                            IconButton::new("send", IconName::ArrowUp)
                                .size(IconButtonSize::Sm)
                                .disabled(!send_enabled)
                                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                        ),
                )
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .text_body_sm()
            .text_color(c.text_standard)
            .child(header)
            .children(edge)
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
                    .children(typing.and_then(|who| super::typing::overlay(&who, &profiles, cx)))
                    .child(composer),
            )
    }
}
