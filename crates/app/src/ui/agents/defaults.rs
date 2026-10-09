//! A host's agent defaults (ui.md §5a.11, buzz-app's `AgentDefaultsCard`):
//! what its agents use for choices they leave empty. Opened from the host's
//! menu on the Agents page, as this app has no Settings page yet.

use super::editor::{Dismissed, connected};
use super::form::{AiFields, Choice, EnvFields, Form, Picker, Scope};
use crate::hosts::HostConnection;
use buzz_host::api::{
    AgentDefaults, AgentEdit, DefaultsEdit, Harness, HarnessEdit, HarnessKind, Request,
    SessionPolicy,
};
use buzz_host::harness::{defaults_name, from_defaults_name};
use buzz_ui::{ActiveTheme, Button, Dialog, DialogSize, Surface, TypeStyled};
use gpui_kit::*;

pub struct DefaultsDialog {
    host: Entity<HostConnection>,
    ai: AiFields,
    env: EnvFields,
    open: Option<Picker>,
    /// The saved defaults the form was filled from.
    loaded: Option<AgentDefaults>,
    busy: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for DefaultsDialog {}

impl Form for DefaultsDialog {
    fn open(&mut self) -> &mut Option<Picker> {
        &mut self.open
    }

    fn ai(&mut self) -> &mut AiFields {
        &mut self.ai
    }

    fn env(&mut self) -> &mut EnvFields {
        &mut self.env
    }

    fn models_request(&self, cx: &App) -> Result<(Entity<HostConnection>, Request), String> {
        if !connected(&self.host, cx) {
            return Err(format!("{} is not connected.", self.host.read(cx).label));
        }
        let kind = self.kind(cx);
        let edit = AgentEdit {
            name: "Agent defaults".into(),
            harness: HarnessEdit {
                command: self.command(kind, cx),
                provider: self.ai.provider_id(cx),
                model: self.ai.model_id(cx),
                ..Default::default()
            },
            environment: self.environment(cx),
            ..Default::default()
        };
        Ok((
            self.host.clone(),
            Request::Models {
                id: None,
                revision: 0,
                edit,
            },
        ))
    }
}

impl DefaultsDialog {
    pub fn new(host: Entity<HostConnection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        host.update(cx, |host, cx| host.load_defaults(cx));
        let subscriptions = vec![cx.observe_in(&host, window, |this, host, window, cx| {
            if this.loaded.is_none()
                && let Some(defaults) = host.read(cx).defaults.clone()
            {
                this.fill(defaults, window, cx);
            }
            cx.notify();
        })];
        let ai = AiFields::new(true, window, cx);
        let env = EnvFields::new(window, cx);
        let mut dialog = Self {
            host: host.clone(),
            ai,
            env,
            open: None,
            loaded: None,
            busy: false,
            error: None,
            _subscriptions: subscriptions,
        };
        if let Some(defaults) = host.read(cx).defaults.clone() {
            dialog.fill(defaults, window, cx);
        }
        dialog
    }

    fn fill(&mut self, defaults: AgentDefaults, window: &mut Window, cx: &mut Context<Self>) {
        let kind = from_defaults_name(&defaults.harness).unwrap_or(HarnessKind::BuzzAgent);
        self.ai.choice = Some(Choice::Harness(kind));
        self.ai
            .set_provider(Some(kind), &defaults.provider, window, cx);
        let model = defaults.model.clone();
        self.ai
            .model
            .update(cx, |state, cx| state.set_value(model, window, cx));
        self.ai
            .api_key
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.ai.effort = Some(defaults.effort.clone()).filter(|effort| !effort.is_empty());
        self.ai.session = Some(defaults.session_policy.unwrap_or(SessionPolicy::Thread));
        self.ai.saved_keys = defaults.environment_keys.clone();
        self.ai.invalidate();
        self.env.load(&defaults.environment_keys, &[], window, cx);
        self.loaded = Some(defaults);
        cx.notify();
    }

    fn harnesses(&self, cx: &App) -> Vec<Harness> {
        self.host.read(cx).harnesses.clone().unwrap_or_default()
    }

    fn kind(&self, cx: &App) -> HarnessKind {
        match self.ai.choice {
            Some(Choice::Harness(kind)) => kind,
            _ => self
                .loaded
                .as_ref()
                .or(self.host.read(cx).defaults.as_ref())
                .and_then(|defaults| from_defaults_name(&defaults.harness))
                .unwrap_or(HarnessKind::BuzzAgent),
        }
    }

    /// The installed command, which Pi's catalog needs as an absolute path.
    fn command(&self, kind: HarnessKind, cx: &App) -> String {
        self.harnesses(cx)
            .into_iter()
            .find(|harness| harness.kind == kind)
            .and_then(|harness| harness.command)
            .unwrap_or_else(|| defaults_name(kind).unwrap_or("buzz-agent").to_string())
    }

    fn environment(&self, cx: &App) -> std::collections::BTreeMap<String, Option<String>> {
        let harnesses = self.harnesses(cx);
        let scope = Scope {
            harnesses: &harnesses,
            harnesses_loading: false,
            defaults: None,
            open: None,
            busy: false,
        };
        let mut environment = self.env.patch(cx);
        environment.extend(self.ai.environment(&scope, cx));
        environment
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if !connected(&self.host, cx) {
            self.error = Some(format!("{} is not connected.", self.host.read(cx).label).into());
            return cx.notify();
        }
        let kind = self.kind(cx);
        let edit = DefaultsEdit {
            harness: defaults_name(kind).unwrap_or("buzz-agent").into(),
            provider: self.ai.provider_id(cx),
            model: self.ai.model_id(cx),
            effort: self.ai.effort.clone().unwrap_or_default(),
            session_policy: self.ai.session,
            environment: self.environment(cx),
        };
        self.busy = true;
        self.error = None;
        self.open = None;
        let response = self.host.update(cx, |host, cx| {
            host.request(Request::SaveDefaults { edit }, cx)
        });
        cx.spawn(async move |this, cx| {
            let response = response.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match response {
                    Ok(_) => cx.emit(Dismissed),
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for DefaultsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let harnesses = self.harnesses(cx);
        let loading = self.host.read(cx).harnesses.is_none();
        let label = self.host.read(cx).label.clone();
        let body = if self.loaded.is_none() {
            div().text_body_sm().child(if connected(&self.host, cx) {
                "Loading agent defaults…".to_string()
            } else {
                format!("{label} is not connected.")
            })
        } else {
            let scope = Scope {
                harnesses: &harnesses,
                harnesses_loading: loading,
                defaults: None,
                open: self.open,
                busy: self.busy,
            };
            self.ai.sync(&scope, window, cx);
            div()
                .flex()
                .flex_col()
                .gap(rems(2.))
                .child(self.ai.render(&scope, window, cx))
                .child(self.env.render("Environment variables", self.busy, cx))
        };
        Dialog::new("agent-defaults", "Agent defaults")
            .size(DialogSize::Px(640.))
            .heading_title()
            .description(format!(
                "Used when an agent on {label} starts without its own choices. Changes apply \
                 on its next start."
            ))
            .on_close(cx.listener(|this, _, _, cx| {
                if !this.busy {
                    cx.emit(Dismissed)
                }
            }))
            .child(
                div().flex().flex_col().gap(rems(1.5)).child(body).children(
                    self.error
                        .clone()
                        .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
                ),
            )
            .footer(
                Button::new("discard-defaults", "Discard")
                    .surface(Surface::Floating)
                    .disabled(self.busy || self.loaded.is_none())
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(defaults) = this.loaded.clone() {
                            this.error = None;
                            this.fill(defaults, window, cx);
                        }
                    })),
            )
            .footer(
                Button::new("save-defaults", "Save defaults")
                    .prominent()
                    .loading(self.busy)
                    .disabled(self.loaded.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }
}
