//! First-run screens: set up an identity, then add a community.

use super::add_community::AddCommunity;
use crate::community::AppState;
use buzz_ui::{ActiveTheme, Button, ButtonVariant, TextField, TypeStyled};
use gpui_kit::component::input::InputState;
use gpui_kit::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Identity,
    Community,
}

pub struct Onboarding {
    step: Step,
    field: Entity<InputState>,
    error: Option<SharedString>,
    busy: bool,
    /// The community step is the "Add a community" flow, invites included.
    community: Option<Entity<AddCommunity>>,
}

impl Onboarding {
    pub fn new(step: Step, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = match step {
            Step::Identity => "nsec1…",
            Step::Community => "wss://relay.example.com",
        };
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        // Why the saved identity could not be read, e.g. a locked keyring.
        let error = match (&cx.global::<AppState>().identity, step) {
            (crate::community::IdentityState::Error(error), Step::Identity) => {
                Some(error.clone().into())
            }
            _ => None,
        };
        let community =
            (step == Step::Community).then(|| cx.new(|cx| AddCommunity::new(true, window, cx)));
        Self {
            step,
            field,
            error,
            busy: false,
            community,
        }
    }

    fn submit(&mut self, create: bool, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.field.read(cx).value().trim().to_string();
        let result = match self.step {
            Step::Identity => {
                let saved = if create {
                    buzz_identity::create()
                } else {
                    buzz_identity::import(&value)
                };
                saved
                    .map(|identity| AppState::set_identity(identity, cx))
                    .map_err(|error| error.to_string())
            }
            Step::Community => AppState::add_community(&value, cx),
        };
        match result {
            Ok(()) => {
                self.error = None;
                self.field
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
            Err(error) => self.error = Some(error.into()),
        }
        self.busy = false;
        cx.notify();
    }
}

impl Render for Onboarding {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        if let Some(community) = &self.community {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(community.clone())
                .into_any_element();
        }
        let (title, detail) = match self.step {
            Step::Identity => (
                "Set up your Buzz identity",
                "Import the nsec of your existing Buzz account, or create a new identity. \
                 Your key stays in this computer's keyring.",
            ),
            Step::Community => (
                "Add a community",
                "Enter the community's relay origin, for example wss://relay.example.com.",
            ),
        };
        let mut actions = div().flex().gap(px(8.)).child(
            Button::new(
                "submit",
                if self.step == Step::Identity {
                    "Import"
                } else {
                    "Add"
                },
            )
            .variant(ButtonVariant::Prominent)
            .on_click(cx.listener(|this, _, window, cx| this.submit(false, window, cx))),
        );
        if self.step == Step::Identity {
            actions =
                actions
                    .child(Button::new("create", "Create new identity").on_click(
                        cx.listener(|this, _, window, cx| this.submit(true, window, cx)),
                    ));
        }
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .w(px(420.))
                    .child(
                        div()
                            .text_heading()
                            .text_color(c.text_standard)
                            .child(title),
                    )
                    .child(div().text_body_sm().text_color(c.text_subtle).child(detail))
                    .child(TextField::new(&self.field))
                    .children(
                        self.error.clone().map(|error| {
                            div().text_caption().text_color(c.text_danger).child(error)
                        }),
                    )
                    .child(actions),
            )
            .into_any_element()
    }
}
