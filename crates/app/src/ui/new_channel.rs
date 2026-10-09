//! "Create a channel" (`CreateChannelDialog.tsx`): name, description and
//! privacy. The relay creates the channel with an id chosen here; it opens
//! once the viewer is confirmed as its owner.

use super::modal::{Dismissed, OpenChannel, error};
use crate::community::Community;
use buzz_relay_client::lifecycle::{MAX_ABOUT_CHARS, MAX_NAME_CHARS, NewChannel, canonical_name};
use buzz_ui::{ActiveTheme, Button, Dialog, Surface, Switch, TextArea, TextField, TypeStyled};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::*;

pub struct NewChannelDialog {
    community: Entity<Community>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    private: bool,
    busy: bool,
    error: Option<SharedString>,
    on_open: OpenChannel,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for NewChannelDialog {}

impl NewChannelDialog {
    pub fn new(
        community: Entity<Community>,
        on_open: OpenChannel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("release-notes"));
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("What this channel is for")
                .auto_grow(3, 8)
        });
        name.update(cx, |state, cx| state.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &name,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.create(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
            cx.subscribe(&description, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        Self {
            community,
            name,
            description,
            private: false,
            busy: false,
            error: None,
            on_open,
            _subscriptions: subscriptions,
        }
    }

    fn draft(&self, cx: &App) -> NewChannel {
        NewChannel {
            name: self.name.read(cx).value().to_string(),
            description: self.description.read(cx).value().to_string(),
            private: self.private,
        }
    }

    fn invalid(&self, cx: &App) -> bool {
        let draft = self.draft(cx);
        let name = canonical_name(&draft.name);
        name.is_empty()
            || name.chars().count() > MAX_NAME_CHARS
            || draft.description.trim().chars().count() > MAX_ABOUT_CHARS
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.invalid(cx) {
            return;
        }
        let draft = self.draft(cx);
        let session = self.community.read(cx).session.clone();
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = session.create_channel(draft).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(id) => {
                        let channel = this
                            .community
                            .read(cx)
                            .snapshot
                            .channels
                            .iter()
                            .find(|c| c.id == id)
                            .cloned();
                        if let Some(channel) = channel {
                            (this.on_open)(&channel, None, window, cx);
                        }
                        cx.emit(Dismissed);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for NewChannelDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let name_len = self.name.read(cx).value().chars().count();
        let about_len = self.description.read(cx).value().chars().count();
        let counted = |label: &'static str, count: usize, limit: usize| {
            div()
                .flex()
                .justify_between()
                .child(div().text_label_sm().child(label))
                .child(
                    div()
                        .text_body_sm()
                        .text_color(if count > limit {
                            c.text_danger
                        } else {
                            c.text_subtle
                        })
                        .child(format!("{count}/{limit}")),
                )
        };
        let weak = cx.entity().downgrade();
        Dialog::new("create-channel", "Create a channel")
            .compact_header_gap()
            .on_close(cx.listener(|this, _, _, cx| {
                if !this.busy {
                    cx.emit(Dismissed)
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .child(counted("Name", name_len, MAX_NAME_CHARS))
                            .child(TextField::new(&self.name).surface(Surface::Floating)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .child(counted("Description", about_len, MAX_ABOUT_CHARS))
                            .child(TextArea::new(&self.description).surface(Surface::Floating)),
                    )
                    .children(self.error.clone().map(|message| error(message, cx))),
            )
            .footer_leading(
                Switch::new("create-private", self.private)
                    .label("Private")
                    .disabled(self.busy)
                    .on_change(move |checked, _, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.private = checked;
                            cx.notify();
                        });
                    }),
            )
            .footer(
                Button::new("create-channel-submit", "Create channel")
                    .prominent()
                    .loading(self.busy)
                    .disabled(self.invalid(cx))
                    .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
            )
    }
}
