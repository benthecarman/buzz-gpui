//! "Create a channel" (`CreateChannelDialog.tsx`): name, description,
//! duration and privacy, with the privacy confirmation and "Discard
//! changes?" as steps of the same dialog. The relay creates the channel
//! with an id chosen here; it opens once the viewer is confirmed as its
//! owner.

use super::form_fields::{self, Bounded, TextKind};
use super::modal::{Dismissed, OpenChannel, error};
use super::templates::{self, TemplateEditor};
use crate::community::Community;
use buzz_relay_client::kit::setup::{ChannelCreationInput, CreationVisibility};
use buzz_relay_client::lifecycle::{
    DEFAULT_TEMPORARY_TTL, DraftErrors, NewChannel, canonical_name, draft_errors,
};
use buzz_relay_client::sidebar::GroupSource;
use buzz_relay_client::sidebar_store::PreferencesStatus;
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
    /// The sidebar group a new channel goes into (`initialGroup`).
    group: Option<String>,
    /// The template selector, while Templates & teams is on.
    template: Option<Entity<TemplateEditor>>,
    /// A creation awaiting "Retry channel"; the form shows it frozen.
    pending: Option<ChannelCreationInput>,
    /// Holds focus on the privacy and discard steps, so Escape steps back.
    step_focus: FocusHandle,
    _bounds: [Bounded; 2],
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for NewChannelDialog {}

/// The sidebar group whose "Create channel" opened the dialog, set just
/// before the dialog opens and taken by it.
#[derive(Default)]
pub struct CreateIn(pub Option<String>);

impl Global for CreateIn {}

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
        let template = templates::provider(cx)
            .map(|_| cx.new(|cx| TemplateEditor::new(community.clone(), cx)));
        let mut subscriptions = subscriptions;
        subscriptions.extend(template.as_ref().map(|editor| {
            cx.subscribe(editor, |this, _, _: &templates::Changed, cx| {
                this.error = None;
                cx.notify();
            })
        }));
        let session = community.read(cx).session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Some(pending) = session.pending_creation().await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| this.restore(pending, window, cx));
        })
        .detach();
        let group = cx
            .try_global::<CreateIn>()
            .and_then(|create| create.0.clone());
        cx.set_global(CreateIn(None));
        Self {
            group,
            template,
            pending: None,
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

    /// Shows a creation awaiting confirmation, frozen, for "Retry channel".
    fn restore(
        &mut self,
        pending: ChannelCreationInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name.update(cx, |state, cx| {
            state.set_value(pending.name.clone(), window, cx)
        });
        let description = pending.description.clone().unwrap_or_default();
        self.description
            .update(cx, |state, cx| state.set_value(description, window, cx));
        self.private = pending.visibility == CreationVisibility::Private;
        self.temporary = pending.ttl_seconds.is_some();
        self.temporary_ttl = pending.ttl_seconds.unwrap_or(DEFAULT_TEMPORARY_TTL);
        self.pending = Some(pending);
        self.error = None;
        cx.notify();
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
        !self.busy
            && self.privacy_choice.is_none()
            && !self.confirm_discard
            && self.pending.is_none()
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
        } else if self.pending.is_none()
            && (self.draft(cx).dirty()
                || self.template.as_ref().is_some_and(|t| t.read(cx).dirty()))
        {
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
        let retry = self.pending.is_some() && !self.busy;
        if !retry && (draft.errors().any() || !self.editing()) {
            return;
        }
        // Recovery keeps the original frozen request.
        let input = match &self.pending {
            Some(pending) => pending.clone(),
            None => {
                let setup = match self.template.as_ref().map(|t| t.read(cx).setup()) {
                    Some(Err(problem)) => {
                        self.error = Some(problem.into());
                        cx.notify();
                        return;
                    }
                    Some(Ok(setup)) => setup,
                    None => None,
                };
                // A channel created from a sidebar group goes into it.
                let setup = match &self.group {
                    Some(group) => {
                        let sidebar = self.community.read(cx).snapshot.sidebar.clone();
                        let ready = sidebar.status == PreferencesStatus::Ready;
                        let data = sidebar.data.as_ref();
                        if !ready
                            || !data.is_some_and(|d| d.sections.iter().any(|s| s.id == *group))
                        {
                            self.error = Some(
                                if ready {
                                    "This group is no longer available. Close this dialog and \
                                     create from another sidebar group or Channels."
                                } else {
                                    "Wait for saved sidebar groups to load, then try again."
                                }
                                .into(),
                            );
                            cx.notify();
                            return;
                        }
                        let personal =
                            data.is_some_and(|d| d.group_source == GroupSource::Personal);
                        let mut setup = setup.unwrap_or_default();
                        setup.group_id = group.clone();
                        setup.group_source =
                            Some(if personal { "personal" } else { "legacy" }.into());
                        Some(setup)
                    }
                    None => setup,
                };
                let channel = NewChannel {
                    name: canonical_name(&draft.name),
                    description: draft.description.trim().to_owned(),
                    private: self.private,
                    ttl: self.temporary.then_some(self.temporary_ttl),
                };
                ChannelCreationInput::from_new(&channel, setup)
            }
        };
        let session = self.community.read(cx).session.clone();
        self.busy = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = session.create_channel_with(input).await;
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
                    Err(message) => {
                        this.error = Some(message.into());
                        let session = this.community.read(cx).session.clone();
                        cx.spawn_in(window, async move |this, cx| {
                            let pending = session.pending_creation().await;
                            let _ = this.update_in(cx, |this, window, cx| {
                                // An unconfirmed Create becomes "Retry channel".
                                let error = this.error.take();
                                match pending {
                                    Some(pending) => this.restore(pending, window, cx),
                                    None => this.pending = None,
                                }
                                this.error = error;
                                cx.notify();
                            });
                        })
                        .detach();
                    }
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
        let pending = self.pending.is_some();
        let summary = self
            .pending
            .as_ref()
            .and_then(|p| p.setup.as_ref())
            .map(|setup| templates::setup_summary(setup.agents.len(), cx));
        let problem = self
            .template
            .as_ref()
            .filter(|_| !pending)
            .and_then(|t| t.read(cx).draft().and_then(|d| d.problem.clone()));
        let title = match &self.group {
            Some(group) => {
                let name = self
                    .community
                    .read(cx)
                    .snapshot
                    .sidebar
                    .data
                    .as_ref()
                    .and_then(|data| data.sections.iter().find(|s| s.id == *group))
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "saved group".into());
                format!("Create a channel in {name}")
            }
            None => "Create a channel".into(),
        };
        Dialog::new("create-channel", title)
            .compact_header_gap()
            .on_close(close)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S4)
                    .children(pending.then(|| templates::pending_notice(cx)))
                    // A name error waits until there is a name.
                    .child(form_fields::name_field(
                        &self.name,
                        errors.name.filter(|_| !draft.name.is_empty() && !pending),
                        pending,
                        cx,
                    ))
                    .child(form_fields::description_field(
                        &self.description,
                        errors.description.filter(|_| !pending),
                        pending,
                        cx,
                    ))
                    .children(self.template.clone().filter(|_| !pending))
                    .child(form_fields::duration_field(
                        self.temporary,
                        self.temporary_ttl,
                        pending,
                        None,
                        on_duration,
                        cx,
                    ))
                    .children(problem.map(|message| error(message, cx)))
                    .children(summary)
                    .children(self.error.clone().map(|message| error(message, cx))),
            )
            .footer_leading(
                Switch::new("create-private", self.private)
                    .label("Private")
                    .disabled(pending)
                    .on_change(move |checked, window, cx| {
                        let _ = weak.update(cx, |this, cx| this.set_private(checked, window, cx));
                    }),
            )
            .footer(
                Button::new(
                    "create-channel-submit",
                    if pending {
                        "Retry channel"
                    } else {
                        "Create channel"
                    },
                )
                .prominent()
                .loading(self.busy)
                .disabled(errors.any() && !pending)
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
