//! "Create a channel" (`CreateChannelDialog.tsx`): name, description,
//! duration and privacy, with the privacy confirmation and "Discard
//! changes?" as steps of the same dialog. The relay creates the channel
//! with an id chosen here; it opens once the viewer is confirmed as its
//! owner.

use super::form_fields::{self, Bounded, TextKind};
use super::modal::{Dismissed, OpenChannel, error};
use crate::community::Community;
use buzz_relay_client::lifecycle::{
    DEFAULT_TEMPORARY_TTL, DraftErrors, NewChannel, canonical_name, draft_errors,
};
use buzz_ui::{Button, Dialog, Surface, Switch, tokens::space};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::*;
use std::rc::Rc;

pub struct NewChannelDialog {
    community: Entity<Community>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    private: bool,
    temporary: bool,
    /// The temporary duration, kept while Ongoing is chosen.
    temporary_ttl: u64,
    /// The visibility awaiting "Make channel private/public?".
    privacy_choice: Option<bool>,
    skip_warning: bool,
    confirm_discard: bool,
    busy: bool,
    error: Option<SharedString>,
    on_open: OpenChannel,
    /// Holds focus on the privacy and discard steps, so Escape steps back.
    step_focus: FocusHandle,
    _bounds: [Bounded; 2],
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for NewChannelDialog {}

/// What the dialog holds, apart from its inputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Draft {
    name: String,
    description: String,
    private: bool,
    temporary: bool,
}

impl Draft {
    fn errors(&self) -> DraftErrors {
        draft_errors(&canonical_name(&self.name), &self.description, None)
    }

    /// Anything typed or chosen asks before it is thrown away.
    fn dirty(&self) -> bool {
        !self.name.is_empty() || !self.description.is_empty() || self.private || self.temporary
    }
}

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
        let bounds = [
            Bounded::new(&name, TextKind::Name.limit(), window, cx),
            Bounded::new(&description, TextKind::Description.limit(), window, cx),
        ];
        let subscriptions = vec![
            cx.subscribe_in(
                &name,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.create(window, cx),
                    InputEvent::Change => {
                        this.error = None;
                        cx.notify()
                    }
                    _ => {}
                },
            ),
            cx.subscribe(&description, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    cx.notify();
                }
            }),
        ];
        Self {
            community,
            name,
            description,
            private: false,
            temporary: false,
            temporary_ttl: DEFAULT_TEMPORARY_TTL,
            privacy_choice: None,
            skip_warning: false,
            confirm_discard: false,
            busy: false,
            error: None,
            on_open,
            step_focus: cx.focus_handle(),
            _bounds: bounds,
            _subscriptions: subscriptions,
        }
    }

    fn draft(&self, cx: &App) -> Draft {
        Draft {
            name: self.name.read(cx).value().to_string(),
            description: self.description.read(cx).value().to_string(),
            private: self.private,
            temporary: self.temporary,
        }
    }

    /// The form takes edits only on its own step and while idle.
    fn editing(&self) -> bool {
        !self.busy && self.privacy_choice.is_none() && !self.confirm_discard
    }

    /// Focus follows the step: the name field, or the step for Escape.
    fn after_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.privacy_choice.is_some() || self.confirm_discard {
            self.step_focus.focus(window, cx);
        } else {
            self.name.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    /// Close, Escape and the scrim step back through privacy and discard.
    fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.privacy_choice.is_some() {
            self.privacy_choice = None;
        } else if self.confirm_discard {
            self.confirm_discard = false;
        } else if self.draft(cx).dirty() {
            self.confirm_discard = true;
        } else {
            cx.emit(Dismissed);
            return;
        }
        self.after_step(window, cx);
    }

    fn set_private(&mut self, private: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editing() {
            return;
        }
        if form_fields::skip_privacy_confirmation(self.community.read(cx), cx) {
            self.private = private;
            self.error = None;
        } else {
            self.skip_warning = false;
            self.privacy_choice = Some(private);
        }
        self.after_step(window, cx);
    }

    fn confirm_privacy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(private) = self.privacy_choice.take() else {
            return;
        };
        if self.busy {
            return;
        }
        if self.skip_warning {
            form_fields::remember_privacy_confirmation(self.community.read(cx), cx);
        }
        self.private = private;
        self.error = None;
        self.after_step(window, cx);
    }

    fn set_duration(&mut self, ttl: Option<u64>, cx: &mut Context<Self>) {
        if !self.editing() {
            return;
        }
        self.temporary = ttl.is_some();
        if let Some(ttl) = ttl {
            self.temporary_ttl = ttl;
        }
        self.error = None;
        cx.notify();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        if draft.errors().any() || !self.editing() {
            return;
        }
        let channel = NewChannel {
            name: canonical_name(&draft.name),
            description: draft.description.trim().to_owned(),
            private: self.private,
            ttl: self.temporary.then_some(self.temporary_ttl),
        };
        let session = self.community.read(cx).session.clone();
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = session.create_channel(channel).await;
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
                    Err(message) => this.error = Some(message.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dialog(&self, cx: &mut Context<Self>) -> Dialog {
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.request_close(window, cx));
        if self.confirm_discard {
            return Dialog::new("create-channel", "Discard changes?")
                .description("Your channel draft has unsaved changes.")
                .compact_header_gap()
                .on_close(close)
                .footer(
                    Button::new("keep-editing", "Keep editing")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_discard = false;
                            this.after_step(window, cx);
                        })),
                )
                .footer(
                    Button::new("discard-changes", "Discard changes")
                        .destructive()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.busy {
                                cx.emit(Dismissed);
                            }
                        })),
                );
        }
        if let Some(private) = self.privacy_choice {
            let weak = cx.entity().downgrade();
            return Dialog::new("create-channel", form_fields::privacy_title(private))
                .compact_header_gap()
                .compact_footer_gap()
                .on_close(close)
                .child(form_fields::privacy_confirmation(
                    private,
                    self.skip_warning,
                    move |checked, _, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.skip_warning = checked;
                            cx.notify();
                        });
                    },
                    cx,
                ))
                .footer(
                    Button::new("privacy-cancel", "Cancel")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.privacy_choice = None;
                            this.after_step(window, cx);
                        })),
                )
                .footer(
                    Button::new("privacy-continue", "Continue")
                        .prominent()
                        .disabled(self.busy)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_privacy(window, cx)),
                        ),
                );
        }
        let draft = self.draft(cx);
        let errors = draft.errors();
        let weak = cx.entity().downgrade();
        let on_duration: form_fields::OnDuration = Rc::new(move |ttl, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_duration(ttl, cx));
        });
        let weak = cx.entity().downgrade();
        Dialog::new("create-channel", "Create a channel")
            .compact_header_gap()
            .on_close(close)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S4)
                    // A name error waits until there is a name.
                    .child(form_fields::name_field(
                        &self.name,
                        errors.name.filter(|_| !draft.name.is_empty()),
                        false,
                        cx,
                    ))
                    .child(form_fields::description_field(
                        &self.description,
                        errors.description,
                        false,
                        cx,
                    ))
                    .child(form_fields::duration_field(
                        self.temporary,
                        self.temporary_ttl,
                        false,
                        None,
                        on_duration,
                        cx,
                    ))
                    .children(self.error.clone().map(|message| error(message, cx))),
            )
            .footer_leading(
                Switch::new("create-private", self.private)
                    .label("Private")
                    .on_change(move |checked, window, cx| {
                        let _ = weak.update(cx, |this, cx| this.set_private(checked, window, cx));
                    }),
            )
            .footer(
                Button::new("create-channel-submit", "Create channel")
                    .prominent()
                    .loading(self.busy)
                    .disabled(errors.any())
                    .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
            )
    }
}

impl Render for NewChannelDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        // The dialog covers the scrim so a press outside the card steps
        // back (`dismissOnOutsideClick`) instead of closing outright.
        div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.request_close(window, cx)),
            )
            .child(
                div()
                    .track_focus(&self.step_focus)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            cx.stop_propagation();
                            this.request_close(window, cx);
                        }
                    }))
                    .child(self.dialog(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn invalid_drafts_cannot_be_created() {
        let mut draft = Draft::default();
        assert!(draft.errors().name.is_some(), "a name is required");
        draft.name = "  #release-notes ".into();
        assert!(!draft.errors().any(), "normalized before checking");
        draft.description = "Buzz session (x)".into();
        assert!(draft.errors().description.is_some());
        draft.description = "x".repeat(1001);
        assert!(draft.errors().description.is_some());
    }

    #[test]
    fn any_choice_makes_the_draft_dirty() {
        assert!(!Draft::default().dirty());
        for draft in [
            Draft {
                name: " ".into(),
                ..Draft::default()
            },
            Draft {
                description: "x".into(),
                ..Draft::default()
            },
            Draft {
                private: true,
                ..Draft::default()
            },
            Draft {
                temporary: true,
                ..Draft::default()
            },
        ] {
            assert!(draft.dirty());
        }
    }
}
