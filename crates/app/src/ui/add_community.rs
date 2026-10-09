//! "Add a community" (relay.md §1.6–1.7, `CommunityDialog.tsx`): a relay
//! address opens a community the identity already belongs to; an invite
//! link (`buzz://join?relay=…&code=…` or `https://host/invite/<code>`)
//! shows the community's join policy when it has one, then claims the
//! invite.

use super::modal::{Dismissed, error};
use crate::community::{AppState, IdentityState};
use crate::runtime::Runtime;
use buzz_relay_client::community::{self, CommunityInput, JoinInfo, JoinPolicy};
use buzz_relay_client::{RelayHttp, RelayOrigin};
use buzz_ui::{ActiveTheme, Button, Checkbox, Dialog, Surface, TextField, TypeStyled};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

enum Step {
    Address,
    /// The invite's community has a join policy to accept first.
    Policy {
        origin: RelayOrigin,
        code: String,
        name: Option<String>,
        policy: JoinPolicy,
    },
}

pub struct AddCommunity {
    step: Step,
    field: Entity<InputState>,
    agreed: bool,
    age_confirmed: bool,
    busy: bool,
    error: Option<SharedString>,
    /// Shown inside onboarding rather than as a dismissible dialog.
    embedded: bool,
    _subscription: Subscription,
}

impl EventEmitter<Dismissed> for AddCommunity {}

impl AddCommunity {
    pub fn new(embedded: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder("wss://relay.example.com"));
        field.update(cx, |state, cx| state.focus(window, cx));
        let subscription =
            cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.next(window, cx);
                }
            });
        Self {
            step: Step::Address,
            field,
            agreed: false,
            age_confirmed: false,
            busy: false,
            error: None,
            embedded,
            _subscription: subscription,
        }
    }

    /// Fills in `link` and checks it, as Continue would.
    pub fn submit(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.field
            .update(cx, |state, cx| state.set_value(link.to_owned(), window, cx));
        self.next(window, cx);
    }

    fn http(origin: &RelayOrigin, cx: &App) -> Result<RelayHttp, String> {
        match &cx.global::<AppState>().identity {
            IdentityState::Ready(identity) => Ok(RelayHttp::new(origin.clone(), identity.clone())),
            _ => Err("Set up your identity first".into()),
        }
    }

    /// Continue: check the relay and either open it, show its policy, or
    /// claim the invite.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let input = match community::parse_input(&self.field.read(cx).value()) {
            Ok(input) => input,
            Err(message) => {
                self.error = Some(message.into());
                cx.notify();
                return;
            }
        };
        let http = match Self::http(input.origin(), cx) {
            Ok(http) => http,
            Err(message) => {
                self.error = Some(message.into());
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let runtime = Runtime::handle(cx);
        cx.spawn_in(window, async move |this, cx| {
            let work = runtime.spawn(async move {
                let info = community::inspect(&http).await?;
                let member = community::probe(&http).await?;
                Ok::<(JoinInfo, bool), String>((info, member))
            });
            let result = work
                .await
                .unwrap_or_else(|_| Err("The check stopped.".into()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match (result, input) {
                    (Err(message), _) => this.error = Some(message.into()),
                    (Ok((_, true)), input) => this.finish(input.origin(), cx),
                    (Ok((_, false)), CommunityInput::Relay(_)) => {
                        this.error = Some(
                            "You’re not a member of this community yet. Paste an invite link \
                             to join."
                                .into(),
                        )
                    }
                    (Ok((info, false)), CommunityInput::Invite { origin, code }) => {
                        match info.policy {
                            Some(policy) => {
                                this.step = Step::Policy {
                                    origin,
                                    code,
                                    name: info.name,
                                    policy,
                                };
                            }
                            None => this.claim(origin, code, None, window, cx),
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn claim(
        &mut self,
        origin: RelayOrigin,
        code: String,
        policy: Option<JoinPolicy>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let http = match Self::http(&origin, cx) {
            Ok(http) => http,
            Err(message) => {
                self.error = Some(message.into());
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let age = self.age_confirmed;
        let runtime = Runtime::handle(cx);
        cx.spawn_in(window, async move |this, cx| {
            let work = runtime
                .spawn(async move { community::claim(&http, &code, policy.as_ref(), age).await });
            let result = work
                .await
                .unwrap_or_else(|_| Err("The join stopped.".into()));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => this.finish(&origin, cx),
                    Err(message) => this.error = Some(message.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn finish(&mut self, origin: &RelayOrigin, cx: &mut Context<Self>) {
        match AppState::add_community(origin.websocket(), cx) {
            Ok(()) => cx.emit(Dismissed),
            Err(message) => self.error = Some(message.into()),
        }
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Step::Policy {
            origin,
            code,
            policy,
            ..
        } = &self.step
        else {
            return;
        };
        if !self.agreed || (policy.age_required && !self.age_confirmed) {
            self.error = Some("Accept the community's terms to join.".into());
            cx.notify();
            return;
        }
        let (origin, code, policy) = (origin.clone(), code.clone(), policy.clone());
        self.claim(origin, code, Some(policy), window, cx);
    }
}

impl Render for AddCommunity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let embedded = self.embedded;
        let mut dialog = Dialog::new("add-community", "Add a community")
            .description(
                "Use your identity across communities. Your profile and conversations stay \
                 separate in each one.",
            )
            .when(!embedded, |d| {
                d.on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
            });
        match &self.step {
            Step::Address => {
                dialog = dialog
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .child(div().text_label_sm().child("Relay URL or invite link"))
                            .child(TextField::new(&self.field).surface(Surface::Floating))
                            .child(div().text_body_sm().text_color(c.text_subtle).child(
                                "Enter a wss:// or https:// relay address without a path, or \
                                     paste an invite link. Continue contacts this relay using \
                                     your Buzz identity.",
                            ))
                            .children(self.error.clone().map(|message| error(message, cx))),
                    )
                    .when(!embedded, |d| {
                        d.footer_leading(
                            Button::new("community-back", "Back")
                                .ghost()
                                .surface(Surface::Floating)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
                        )
                    })
                    .footer(
                        Button::new("community-continue", "Continue")
                            .prominent()
                            .loading(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                    );
            }
            Step::Policy { name, policy, .. } => {
                let documents = [("Terms", &policy.terms), ("Privacy", &policy.privacy)];
                let mut body =
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(1.))
                        .child(div().text_body_sm().child(format!(
                            "{} asks new members to accept its terms before joining.",
                            name.as_deref().unwrap_or("This community")
                        )));
                for (label, text) in documents {
                    if let Some(text) = text {
                        body = body.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(rems(0.5))
                                .child(div().text_label_sm().child(label))
                                .child(
                                    div()
                                        .id(label)
                                        .max_h(px(160.))
                                        .overflow_y_scroll()
                                        .p(rems(0.75))
                                        .rounded(px(12.))
                                        .bg(c.surface_inset)
                                        .text_body_sm()
                                        .child(text.clone()),
                                ),
                        );
                    }
                }
                let weak = cx.entity().downgrade();
                body = body.child(
                    Checkbox::new("accept-terms", self.agreed)
                        .label("I accept this community's terms")
                        .surface(Surface::Floating)
                        .on_change(move |checked, _, cx| {
                            let _ = weak.update(cx, |this, cx| {
                                this.agreed = checked;
                                cx.notify();
                            });
                        }),
                );
                if policy.age_required {
                    let weak = cx.entity().downgrade();
                    body = body.child(
                        Checkbox::new("confirm-age", self.age_confirmed)
                            .label("I confirm I meet this community's minimum age")
                            .surface(Surface::Floating)
                            .on_change(move |checked, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.age_confirmed = checked;
                                    cx.notify();
                                });
                            }),
                    );
                }
                body = body.children(self.error.clone().map(|message| error(message, cx)));
                dialog = dialog
                    .child(body)
                    .footer_leading(
                        Button::new("policy-back", "Back")
                            .ghost()
                            .surface(Surface::Floating)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step = Step::Address;
                                this.error = None;
                                cx.notify();
                            })),
                    )
                    .footer(
                        Button::new("policy-join", "Join community")
                            .prominent()
                            .loading(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    );
            }
        }
        dialog
    }
}
