//! The Add agent dialog (ui.md §5a.7): buzz-app's `AgentCreateDialog`, for
//! Add agent, Duplicate and Clone.
//!
//! Creating takes two round trips: the host generates the agent's key
//! (`CreatePrepare`), this process signs the owner attestation for it with
//! the human key, which never leaves the GUI, and the host saves the agent
//! (`CreateCommit`). Then it starts, and its profile is published. Each step
//! that fails reads the host again and says what is known.

use super::draft::{self, Draft, Seed};
use super::editor::{Dismissed, connected};
use super::fields::{Fields, Owner};
use super::form::field;
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::hosts::control::{Lane, Status};
use crate::ui::overlay::{self, Align};
use buzz_host::api::{Agent, AgentAction, ProcessStatus, Request, Response};
use buzz_ui::{
    ActiveTheme, Button, Dialog, DialogSize, Menu, MenuItem, MenuSize, Select, Surface, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// Asks the page to open Settings → Agents.
pub struct OpenHarnesses;

/// What the dialog was opened for.
#[derive(Clone)]
pub enum Kind {
    Add,
    /// From one of the viewer's agents, with a new key.
    Duplicate(Box<Agent>),
    /// From an agent's saved name and instructions.
    Clone(Seed),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Creating,
    Starting,
    Publishing,
    Checking,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Start,
    Profile,
}

pub struct CreateDialog {
    hosts: Vec<Entity<HostConnection>>,
    host: Entity<HostConnection>,
    fields: Fields,
    kind: Kind,
    /// The community's HTTP origin; empty when none is selected.
    destination: String,
    request_id: String,
    saved: Option<Agent>,
    next: Step,
    phase: Option<Phase>,
    host_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for CreateDialog {}
impl EventEmitter<OpenHarnesses> for CreateDialog {}

/// The created agent, by ID; the page hands it the focus.
pub struct Created(pub Entity<HostConnection>, pub String);
impl EventEmitter<Created> for CreateDialog {}

impl Owner for CreateDialog {
    fn fields(&mut self) -> &mut Fields {
        &mut self.fields
    }

    fn open_harnesses(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(OpenHarnesses);
    }
}

fn seeded(host: &Entity<HostConnection>, kind: &Kind, cx: &App) -> Draft {
    let data = host.read(cx).snapshot();
    let mut draft = match kind {
        Kind::Duplicate(source) => Draft {
            name: format!("{} copy", source.name),
            revision: 0,
            ..draft::from_agent(source)
        },
        Kind::Clone(seed) => Draft {
            name: seed.name.clone(),
            system_prompt: seed.system_prompt.clone(),
            ..draft::new_draft(&data)
        },
        Kind::Add => draft::new_draft(&data),
    };
    // One listener worker by default; editable like any variable.
    draft.environment = [("BUZZ_ACP_AGENTS".to_string(), Some("10".to_string()))].into();
    draft
}

impl CreateDialog {
    pub fn new(
        hosts: Vec<Entity<HostConnection>>,
        host: Entity<HostConnection>,
        kind: Kind,
        destination: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = seeded(&host, &kind, cx);
        let fields = Fields::new(host.clone(), None, Vec::new(), draft, window, cx);
        fields.focus_name(window, cx);
        let subscriptions = hosts
            .iter()
            .map(|host| cx.observe(host, |_, _, cx| cx.notify()))
            .collect();
        Self {
            hosts,
            host,
            fields,
            kind,
            destination,
            request_id: uuid::Uuid::new_v4().to_string(),
            saved: None,
            next: Step::Start,
            phase: None,
            host_open: false,
            _subscriptions: subscriptions,
        }
    }

    fn set_host(
        &mut self,
        host: Entity<HostConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if host.entity_id() == self.host.entity_id() || self.saved.is_some() {
            return;
        }
        let draft = seeded(&host, &self.kind, cx);
        let name = self.fields.draft.name.clone();
        let prompt = self.fields.draft.system_prompt.clone();
        self.fields = Fields::new(
            host.clone(),
            None,
            Vec::new(),
            Draft {
                name,
                system_prompt: prompt,
                ..draft
            },
            window,
            cx,
        );
        self.host = host;
        self.request_id = uuid::Uuid::new_v4().to_string();
    }

    fn owner(cx: &App) -> Option<std::sync::Arc<buzz_identity::Identity>> {
        match &cx.global::<AppState>().identity {
            IdentityState::Ready(identity) => Some(identity.clone()),
            _ => None,
        }
    }

    fn available(&self, cx: &App) -> bool {
        !self.destination.is_empty()
            && Self::owner(cx).is_some()
            && self.host.read(cx).agents.create_available != Some(false)
            && connected(&self.host, cx)
    }

    fn blocked(&self, cx: &App) -> bool {
        let host = self.host.read(cx);
        self.phase.is_some() || host.control.busy || host.status() != Status::Ready
    }

    fn runtime_blocked(&self, cx: &App) -> bool {
        !self.host.read(cx).agents.runtime_available
            && (self.saved.is_none() || self.next == Step::Start)
    }

    fn fail(&mut self, error: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.phase = None;
        self.fields.error = Some(error.into());
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        if self.blocked(cx) || self.runtime_blocked(cx) {
            return;
        }
        if self.saved.is_none() && !self.available(cx) {
            return;
        }
        self.fields.error = None;
        self.fields.open = None;
        if let Some(agent) = self.saved.clone() {
            return self.after_create(agent, cx);
        }
        let edit = match draft::edit(&self.fields.draft, false) {
            Ok(edit) => edit,
            Err(error) => return self.fail(error, cx),
        };
        let Some(identity) = Self::owner(cx) else {
            return;
        };
        self.phase = Some(Phase::Creating);
        cx.notify();
        let host = self.host.clone();
        let request_id = self.request_id.clone();
        let prepare = host.update(cx, |host, cx| {
            host.request(
                Request::CreatePrepare {
                    request_id: request_id.clone(),
                    relay_url: self.destination.clone(),
                    owner: identity.pubkey_hex(),
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result: Result<(String, Option<Response>), String> = match prepare.await {
                Ok(Response::Prepared {
                    id, saved: true, ..
                }) => Ok((id, None)),
                Ok(Response::Prepared { id, pubkey, .. }) => match identity.attest_agent(&pubkey) {
                    Ok(auth) => {
                        let commit = host.update(cx, |host, cx| {
                            host.write(
                                Request::CreateCommit {
                                    request_id,
                                    edit,
                                    auth,
                                },
                                Lane {
                                    credential_write: true,
                                    ..Default::default()
                                },
                                cx,
                            )
                        });
                        commit
                            .await
                            .map(|response| (id, Some(response)))
                            .map_err(|failure| failure.reason.unwrap_or_default())
                    }
                    Err(error) => Err(error.to_string()),
                },
                Ok(_) => Err(String::new()),
                Err(buzz_host_rpc::RpcError::Failed(reason)) => Err(reason),
                Err(_) => Err(String::new()),
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok((id, _)) => match this.host.read(cx).agent(id.as_str()).cloned() {
                    Some(agent) => {
                        cx.emit(Created(this.host.clone(), agent.id.clone()));
                        this.saved = Some(agent.clone());
                        // Saved values are write-only; they are not kept.
                        this.fields.draft.environment.clear();
                        this.after_create(agent, cx);
                    }
                    None => this.check(None, Phase::Creating, None, String::new(), cx),
                },
                Err(reason) => this.check(None, Phase::Creating, None, reason, cx),
            });
        })
        .detach();
    }

    /// Starts the agent unless it runs, then finishes its profile.
    fn after_create(&mut self, agent: Agent, cx: &mut Context<Self>) {
        let current = self.host.read(cx).agent(&agent.id).cloned();
        if self.next == Step::Start
            && current
                .as_ref()
                .is_none_or(|current| current.status != ProcessStatus::Running)
        {
            self.phase = Some(Phase::Starting);
            cx.notify();
            let owner = Self::owner(cx).map(|identity| identity.pubkey_hex());
            let start = self.host.update(cx, |host, cx| {
                host.act(&agent.id, AgentAction::Start, owner, None, cx)
            });
            cx.spawn(async move |this, cx| {
                let result = start.await;
                let _ = this.update(cx, |this, cx| match result {
                    Ok(Response::Agents(snapshot)) => {
                        let started = snapshot.agents.iter().find(|a| a.id == agent.id);
                        let failure = (started.map(|a| a.status) != Some(ProcessStatus::Running))
                            .then(|| {
                                format!(
                                    "{} was created, but couldn't start. {}",
                                    agent.name,
                                    started.and_then(|a| a.error.clone()).unwrap_or_else(|| {
                                        "Check its settings, then try Start again.".into()
                                    })
                                )
                            });
                        if failure.is_none() {
                            this.next = Step::Profile;
                        }
                        this.publish(agent, failure, cx);
                    }
                    Ok(_) => this.check(Some(agent), Phase::Starting, None, String::new(), cx),
                    Err(failure) => this.check(
                        Some(agent),
                        Phase::Starting,
                        None,
                        failure.reason.unwrap_or_default(),
                        cx,
                    ),
                });
            })
            .detach();
        } else {
            self.next = Step::Profile;
            self.publish(agent, None, cx);
        }
    }

    fn publish(&mut self, agent: Agent, start_failure: Option<String>, cx: &mut Context<Self>) {
        let pending = self
            .host
            .read(cx)
            .agent(&agent.id)
            .is_none_or(|current| current.profile_pending);
        if !pending {
            return self.finish(start_failure, cx);
        }
        self.phase = Some(Phase::Publishing);
        cx.notify();
        let publish = self.host.update(cx, |host, cx| {
            host.write(
                Request::PublishProfile {
                    id: agent.id.clone(),
                },
                Lane {
                    credential_write: true,
                    ..Default::default()
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = publish.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.finish(start_failure, cx),
                Err(failure) => this.check(
                    Some(agent),
                    Phase::Publishing,
                    start_failure,
                    failure.reason.unwrap_or_default(),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn finish(&mut self, start_failure: Option<String>, cx: &mut Context<Self>) {
        self.phase = None;
        match start_failure {
            Some(failure) => self.fields.error = Some(failure.into()),
            None => cx.emit(Dismissed),
        }
        cx.notify();
    }

    /// A step failed: read the host again and say what is known.
    fn check(
        &mut self,
        agent: Option<Agent>,
        step: Phase,
        start_failure: Option<String>,
        reason: String,
        cx: &mut Context<Self>,
    ) {
        self.phase = Some(Phase::Checking);
        cx.notify();
        let refresh = self.host.update(cx, |host, cx| host.refresh(cx));
        cx.spawn(async move |this, cx| {
            refresh.await;
            let _ = this.update(cx, |this, cx| {
                this.phase = None;
                let reason = if reason.is_empty() {
                    String::new()
                } else {
                    format!(" {reason}")
                };
                let host = this.host.read(cx);
                let ready = host.status() == Status::Ready;
                let current = agent
                    .as_ref()
                    .and_then(|agent| host.agent(&agent.id).cloned());
                let name = agent
                    .as_ref()
                    .map(|agent| agent.name.clone())
                    .unwrap_or_else(|| this.fields.draft.name.clone());
                let error = match (step, &current) {
                    (Phase::Starting, Some(current))
                        if current.status == ProcessStatus::Running =>
                    {
                        this.next = Step::Profile;
                        None
                    }
                    (Phase::Publishing, Some(current)) if !current.profile_pending => {
                        return this.finish(start_failure, cx);
                    }
                    (Phase::Creating, _) => Some(format!(
                        "We couldn't confirm whether the agent was created.{reason} Check the \
                         agent list before trying again."
                    )),
                    (_, Some(_)) if ready => Some(match (step, &start_failure) {
                        (Phase::Starting, _) => format!(
                            "{name} was saved, but couldn't start.{reason} Check its card, then \
                             select Start agent to try again."
                        ),
                        (_, Some(failure)) => {
                            format!("{failure} Profile setup also didn't finish.{reason}")
                        }
                        _ => format!(
                            "{name} was saved and started, but profile setup didn't \
                             finish.{reason} Select Finish profile to try again."
                        ),
                    }),
                    _ => Some(if step == Phase::Starting {
                        format!(
                            "{name} was saved, but we couldn't confirm whether it started. \
                             Refresh status before trying again."
                        )
                    } else {
                        format!(
                            "{name} was saved, but we couldn't confirm its profile setup. \
                             Refresh status before trying again."
                        )
                    }),
                };
                this.fields.error = error.map(Into::into);
                cx.notify();
            });
        })
        .detach();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(Dismissed);
    }

    fn host_field(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let mut menu = Menu::new().size(MenuSize::Wide);
        for (index, host) in self.hosts.iter().enumerate() {
            let state = host.read(cx);
            let online = connected(host, cx);
            let label: SharedString = if online {
                state.label.clone()
            } else {
                format!("{} (offline)", state.label).into()
            };
            let pick = host.clone();
            menu = menu.child(
                MenuItem::new(("host", index), label)
                    .checked(host.entity_id() == self.host.entity_id())
                    .disabled(!online)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.host_open = false;
                        this.set_host(pick.clone(), window, cx);
                        cx.notify();
                    })),
            );
        }
        let dialog = cx.entity().downgrade();
        let dismiss: overlay::Dismiss = Rc::new(move |_, cx| {
            let _ = dialog.update(cx, |this, cx| {
                this.host_open = false;
                cx.notify();
            });
        });
        field(
            "Host",
            Some("The machine that runs this agent.".into()),
            div()
                .relative()
                .w_full()
                .child(
                    Select::new("host-select")
                        .value(self.host.read(cx).label.clone())
                        .open(self.host_open)
                        .surface(Surface::Floating)
                        .disabled(self.phase.is_some() || self.saved.is_some())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.host_open = !this.host_open;
                            cx.notify();
                        })),
                )
                .when(self.host_open, |this| {
                    this.child(overlay::dropdown(window, Align::Start, menu, dismiss))
                }),
            &c,
        )
    }
}

impl Render for CreateDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        self.fields.sync(window, cx);
        let blocked = self.blocked(cx);
        let runtime_blocked = self.runtime_blocked(cx);
        let available = self.available(cx);
        let busy = self.phase.is_some();
        let dirty = self.fields.dirty;
        let name = self
            .saved
            .as_ref()
            .map(|agent| agent.name.clone())
            .unwrap_or_else(|| self.fields.draft.name.clone());
        let cloned = matches!(self.kind, Kind::Clone(_));
        let title = match &self.kind {
            Kind::Duplicate(source) => format!("Duplicate {}", source.name),
            Kind::Clone(_) => "Clone agent".into(),
            Kind::Add => "Add agent".into(),
        };
        let destination = if self.destination.is_empty() {
            "a connected community".to_string()
        } else {
            self.destination.clone()
        };
        let host = self.host.read(cx);
        let host_error = host.status() == Status::Error;
        let runtime_message = host.agents.runtime_message.clone();
        let line = |text: String| div().text_body().child(text);
        let mut body = div()
            .flex()
            .flex_col()
            .gap(rems(2.))
            .when(cloned, |this| {
                this.child(div().text_body_sm().text_color(c.text_subtle).child(
                    "Only the name and instructions were copied. Review them for embedded \
                     secrets. Choose this computer’s workspace and runtime settings. Identity \
                     keys, environment values, history and community membership are not copied. \
                     The source stays unchanged.",
                ))
            })
            .when(self.hosts.len() > 1, |this| {
                this.child(self.host_field(window, cx))
            })
            .child(
                self.fields
                    .render(blocked || self.saved.is_some(), dirty, window, cx),
            );
        if let Kind::Duplicate(source) = &self.kind
            && !source.harness.environment_keys.is_empty()
        {
            body = body.child(div().text_body_sm().text_color(c.text_subtle).child(format!(
                "Re-enter environment values for {}. Saved values cannot be copied into a new \
                 identity.",
                source.harness.environment_keys.join(", ")
            )));
        }
        let mut messages = div().flex().flex_col().gap(rems(0.5));
        if !available {
            messages = messages.child(line(
                "Connect to a community and use a rebuilt desktop app to create an agent.".into(),
            ));
        }
        if runtime_blocked {
            messages = messages.child(
                line(format!(
                    "This app’s agent runtime is unavailable. Repair or rebuild the desktop app \
                     before {} an agent.{}",
                    if self.saved.is_some() {
                        "starting"
                    } else {
                        "creating"
                    },
                    runtime_message
                        .map(|message| format!(" {message}"))
                        .unwrap_or_default()
                ))
                .text_color(c.text_danger),
            );
        }
        if let Some(phase) = self.phase {
            messages = messages.child(line(match phase {
                Phase::Creating => "Creating agent…".into(),
                Phase::Starting => format!("{name} was created. Starting it…"),
                Phase::Publishing => format!("{name} was saved. Finishing its profile…"),
                Phase::Checking => "Checking agent status…".into(),
            }));
        }
        if let Some(saved) = &self.saved
            && !busy
            && self.fields.error.is_none()
        {
            messages = messages.child(line(match self.next {
                Step::Start => format!("{} was saved. Start it to finish setup.", saved.name),
                Step::Profile => format!(
                    "{} was saved and started. Finish its profile setup.",
                    saved.name
                ),
            }));
        }
        if let Some(error) = self.fields.error.clone() {
            messages = messages.child(div().text_body().text_color(c.text_danger).child(error));
        }
        if host_error && !busy {
            let host = self.host.clone();
            messages = messages.child(
                div().flex().child(
                    Button::new("retry-status", "Retry status")
                        .surface(Surface::Floating)
                        .on_click(move |_, _, cx| {
                            host.update(cx, |host, cx| host.refresh(cx).detach())
                        }),
                ),
            );
        }
        body = body.child(messages);
        let label = if let Some(phase) = self.phase {
            match phase {
                Phase::Starting => "Starting…",
                Phase::Publishing => "Finishing…",
                Phase::Checking => "Checking…",
                Phase::Creating => "Creating…",
            }
        } else if self.saved.is_some() {
            match self.next {
                Step::Start => "Start agent",
                Step::Profile => "Finish profile",
            }
        } else if cloned {
            "Clone agent"
        } else {
            "Create agent"
        };
        Dialog::new("add-agent", title)
            .size(DialogSize::Px(860.))
            .heading_title()
            .description(format!(
                "Create and start an agent in {destination}. It won't join a channel \
                 automatically."
            ))
            .on_close(cx.listener(move |this, _, _, cx| {
                if !dirty && !this.blocked(cx) {
                    this.close(cx)
                }
            }))
            .child(body)
            .footer(
                Button::new(
                    "cancel-agent",
                    if busy || self.saved.is_some() {
                        "Close"
                    } else {
                        "Cancel"
                    },
                )
                .surface(Surface::Floating)
                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .footer(
                Button::new("create-agent", label)
                    .prominent()
                    .disabled(blocked || runtime_blocked || (self.saved.is_none() && !available))
                    .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
            )
    }
}
