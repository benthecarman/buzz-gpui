//! The Add agent (§5a.7) and Edit agent (§5a.8) dialogs.
//!
//! Creating takes two round trips: the host generates the agent's key
//! (`CreatePrepare`), this process signs the owner attestation for it with
//! the human key, which never leaves the GUI, and the host saves the agent
//! (`CreateCommit`) and publishes its profile.

use super::harness;
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_host::api::{
    Agent, AgentAction, AgentEdit, Harness, HarnessEdit, HarnessKind, Request, Response,
};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, Colors, Dialog, DialogSize, Menu, MenuItem, MenuSize,
    Select, Surface, TextArea, TextField, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::rc::Rc;

/// The dialog is done; the page closes it.
pub struct Dismissed;

/// `Field`: label, control, then the description (§6.3).
pub fn field(
    label: &'static str,
    description: Option<SharedString>,
    control: impl IntoElement,
    c: &Colors,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(0.5))
        .min_w_0()
        .child(div().text_label_sm().child(label))
        .child(control)
        .children(
            description.map(|text| div().text_body_sm().text_color(c.text_subtle).child(text)),
        )
}

/// A `<fieldset>` legend in `text-label`.
fn legend(text: &'static str) -> Div {
    div().text_label().child(text)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Harness(HarnessKind),
    /// "Custom executable": the command is typed in.
    Custom,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Host,
    Community,
    Harness,
}

enum Mode {
    Create { hosts: Vec<Entity<HostConnection>> },
    Edit { agent: Box<Agent> },
}

/// Everything the form edits, to tell whether anything changed.
#[derive(Clone, PartialEq)]
struct Values {
    name: String,
    instructions: String,
    choice: Choice,
    executable: String,
    provider: String,
    model: String,
    workspace: String,
}

pub struct AgentEditor {
    mode: Mode,
    host: Option<Entity<HostConnection>>,
    /// The community's relay URL (`wss://…`).
    community: Option<String>,
    /// `None` follows the first installed harness until one is picked.
    choice: Option<Choice>,
    name: Entity<InputState>,
    instructions: Entity<TextareaState>,
    executable: Entity<InputState>,
    provider: Entity<InputState>,
    model: Entity<InputState>,
    workspace: Entity<InputState>,
    /// The values the dialog opened with (edit only).
    original: Option<Values>,
    open: Option<Picker>,
    /// Identifies this create to the host, so a retry reuses the same key.
    request_id: String,
    error: Option<SharedString>,
    status: Option<SharedString>,
    busy: bool,
    /// The agent exists but did not start; the primary button retries.
    created: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for AgentEditor {}

fn connected(host: &Entity<HostConnection>, cx: &App) -> bool {
    matches!(host.read(cx).state, ConnectionState::Connected { .. })
}

/// The communities agents can join: the controller only accepts TLS.
fn communities(cx: &App) -> Vec<(String, String)> {
    cx.global::<AppState>()
        .communities
        .iter()
        .map(|community| {
            let community = community.read(cx);
            (community.origin.websocket().to_string(), community.name())
        })
        .collect()
}

impl AgentEditor {
    fn new(
        mode: Mode,
        host: Option<Entity<HostConnection>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let name = input("", window, cx);
        let executable = input("/path/to/acp-agent", window, cx);
        let provider = input("Use the harness default", window, cx);
        let model = input("Choose or enter a model", window, cx);
        let workspace = input("", window, cx);
        let instructions = cx.new(|cx| TextareaState::new(window, cx));
        let mut subscriptions =
            vec![cx.subscribe(&instructions, |_, _, _: &InputEvent, cx| cx.notify())];
        for state in [&name, &executable, &provider, &model, &workspace] {
            subscriptions.push(cx.subscribe(state, |_, _, _: &InputEvent, cx| cx.notify()));
        }
        let hosts = match &mode {
            Mode::Create { hosts } => hosts.clone(),
            Mode::Edit { .. } => host.iter().cloned().collect(),
        };
        for host in &hosts {
            subscriptions.push(cx.observe(host, |_, _, cx| cx.notify()));
        }
        name.update(cx, |state, cx| state.focus(window, cx));
        Self {
            mode,
            host,
            community: None,
            choice: None,
            name,
            instructions,
            executable,
            provider,
            model,
            workspace,
            original: None,
            open: None,
            request_id: uuid::Uuid::new_v4().to_string(),
            error: None,
            status: None,
            busy: false,
            created: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn create(
        hosts: Vec<Entity<HostConnection>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = hosts
            .iter()
            .find(|host| connected(host, cx))
            .or(hosts.first())
            .cloned();
        let mut editor = Self::new(Mode::Create { hosts }, host, window, cx);
        let selected = AppState::current(cx).map(|c| c.read(cx).origin.websocket().to_string());
        let available: Vec<String> = communities(cx)
            .into_iter()
            .map(|(url, _)| url)
            .filter(|url| url.starts_with("wss://"))
            .collect();
        editor.community = selected
            .filter(|url| available.contains(url))
            .or_else(|| available.first().cloned());
        editor
    }

    pub fn edit(
        host: Entity<HostConnection>,
        agent: Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = Self::new(
            Mode::Edit {
                agent: Box::new(agent.clone()),
            },
            Some(host),
            window,
            cx,
        );
        editor.community = Some(agent.relay_url.clone());
        editor.load(&agent, window, cx);
        editor
    }

    /// Fills the form from the agent's saved settings.
    fn load(&mut self, agent: &Agent, window: &mut Window, cx: &mut Context<Self>) {
        let harnesses = self.harnesses(cx);
        let choice = match harness::kind_of(&agent.harness.command, &harnesses) {
            Some(kind) => Choice::Harness(kind),
            None => Choice::Custom,
        };
        let values = Values {
            name: agent.name.clone(),
            instructions: agent.system_prompt.clone(),
            choice,
            executable: if choice == Choice::Custom {
                agent.harness.command.clone()
            } else {
                String::new()
            },
            provider: agent.harness.provider.clone(),
            model: agent.harness.model.clone(),
            workspace: agent.workspace.clone(),
        };
        let set = |state: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut App| {
            let value = value.to_string();
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        };
        set(&self.name, &values.name, window, cx);
        set(&self.executable, &values.executable, window, cx);
        set(&self.provider, &values.provider, window, cx);
        set(&self.model, &values.model, window, cx);
        set(&self.workspace, &values.workspace, window, cx);
        let instructions = values.instructions.clone();
        self.instructions
            .update(cx, |state, cx| state.set_value(instructions, window, cx));
        self.choice = Some(choice);
        if let Mode::Edit { agent: saved } = &mut self.mode {
            **saved = agent.clone();
        }
        self.original = Some(values);
        cx.notify();
    }

    fn harnesses(&self, cx: &App) -> Vec<Harness> {
        self.host
            .as_ref()
            .and_then(|host| host.read(cx).harnesses.clone())
            .unwrap_or_default()
    }

    /// The picked harness, else the first installed one.
    fn choice(&self, cx: &App) -> Choice {
        self.choice.unwrap_or_else(|| {
            self.harnesses(cx)
                .iter()
                .find(|harness| harness.command.is_some())
                .map_or(Choice::Custom, |harness| Choice::Harness(harness.kind))
        })
    }

    fn values(&self, cx: &App) -> Values {
        let text = |state: &Entity<InputState>| state.read(cx).value().trim().to_string();
        Values {
            name: text(&self.name),
            instructions: self.instructions.read(cx).value().to_string(),
            choice: self.choice(cx),
            executable: text(&self.executable),
            provider: text(&self.provider),
            model: text(&self.model),
            workspace: text(&self.workspace),
        }
    }

    fn dirty(&self, cx: &App) -> bool {
        self.original
            .as_ref()
            .is_none_or(|original| *original != self.values(cx))
    }

    /// The agent as currently saved on its host, when it changed since the
    /// dialog opened.
    fn newer(&self, cx: &App) -> Option<Agent> {
        let Mode::Edit { agent } = &self.mode else {
            return None;
        };
        let host = self.host.as_ref()?;
        host.read(cx)
            .agents
            .agents
            .iter()
            .find(|current| current.id == agent.id && current.revision != agent.revision)
            .cloned()
    }

    fn edit_from(&self, cx: &App) -> Result<AgentEdit, String> {
        let values = self.values(cx);
        if values.name.is_empty() {
            return Err("Enter an agent name.".into());
        }
        let saved = match &self.mode {
            Mode::Edit { agent } => Some(agent),
            Mode::Create { .. } => None,
        };
        let (command, preset) = match values.choice {
            Choice::Custom if values.executable.is_empty() => {
                return Err("Enter a harness executable.".into());
            }
            Choice::Custom => (values.executable.clone(), false),
            Choice::Harness(kind) => {
                let harnesses = self.harnesses(cx);
                let detected = harnesses.iter().find(|harness| harness.kind == kind);
                let kept = saved
                    .filter(|agent| {
                        harness::kind_of(&agent.harness.command, &harnesses) == Some(kind)
                    })
                    .map(|agent| agent.harness.command.clone());
                let command = detected
                    .and_then(|harness| harness.command.clone())
                    .or(kept)
                    .ok_or_else(|| match detected {
                        Some(harness) => format!(
                            "{} is not installed on this host. {}",
                            harness.label, harness.setup_hint
                        ),
                        None => "Choose a harness.".into(),
                    })?;
                (command, harness::is_preset(kind))
            }
        };
        if saved.is_some() && values.workspace.is_empty() {
            return Err("Enter a workspace path.".into());
        }
        let args = saved
            .filter(|agent| agent.harness.command == command)
            .map(|agent| agent.harness.args.clone())
            .unwrap_or_default();
        // Presets use their own model and sign-in.
        let (model, provider) = if preset {
            (String::new(), String::new())
        } else {
            (values.model, values.provider)
        };
        Ok(AgentEdit {
            name: values.name,
            picture: saved.and_then(|agent| agent.picture.clone()),
            system_prompt: values.instructions,
            session_policy: None,
            workspace: values.workspace,
            harness: HarnessEdit {
                command,
                args,
                model,
                provider,
            },
            environment: BTreeMap::new(),
            effort: None,
        })
    }

    fn fail(&mut self, error: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.busy = false;
        self.status = None;
        self.error = Some(error.into());
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.open = None;
        let Some(host) = self.host.clone() else {
            return self.fail("Choose a host.", cx);
        };
        if !connected(&host, cx) {
            let label = host.read(cx).label.clone();
            return self.fail(format!("{label} is not connected."), cx);
        }
        if let Some(id) = self.created.clone() {
            return self.start(host, id, cx);
        }
        let edit = match self.edit_from(cx) {
            Ok(edit) => edit,
            Err(error) => return self.fail(error, cx),
        };
        match &self.mode {
            Mode::Create { .. } => self.commit_create(host, edit, cx),
            Mode::Edit { agent } => {
                let (id, revision) = (agent.id.clone(), agent.revision);
                self.save(host, id, revision, edit, cx)
            }
        }
    }

    fn commit_create(
        &mut self,
        host: Entity<HostConnection>,
        edit: AgentEdit,
        cx: &mut Context<Self>,
    ) {
        let Some(relay_url) = self.community.clone() else {
            return self.fail("Choose a community.", cx);
        };
        let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
            return self.fail("Set up your identity first.", cx);
        };
        let identity = identity.clone();
        self.busy = true;
        self.error = None;
        self.status = Some("Creating agent…".into());
        cx.notify();
        let request_id = self.request_id.clone();
        let name = edit.name.clone();
        let prepare = host.update(cx, |host, cx| {
            host.request(
                Request::CreatePrepare {
                    request_id: request_id.clone(),
                    relay_url,
                    owner: identity.pubkey_hex(),
                },
                cx,
            )
        });
        let weak_host = host.downgrade();
        cx.spawn(async move |this, cx| {
            let id = match prepare.await {
                Ok(Response::Prepared { id, pubkey }) => match identity.attest_agent(&pubkey) {
                    Ok(auth) => {
                        let commit = weak_host.update(cx, |host, cx| {
                            host.request(
                                Request::CreateCommit {
                                    request_id,
                                    edit,
                                    auth,
                                },
                                cx,
                            )
                        });
                        match commit {
                            Ok(commit) => commit.await.map(|_| id).map_err(|e| e.to_string()),
                            Err(_) => Err("The host went away.".into()),
                        }
                    }
                    Err(error) => Err(error.to_string()),
                },
                Ok(_) => Err("The host sent an unexpected reply.".into()),
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update(cx, |this, cx| match id {
                Ok(id) => {
                    this.created = Some(id.clone());
                    this.status = Some(format!("{name} was created. Starting it…").into());
                    cx.notify();
                    this.start(host, id, cx);
                }
                Err(error) => this.fail(error, cx),
            });
        })
        .detach();
    }

    fn start(&mut self, host: Entity<HostConnection>, id: String, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        let name = self.values(cx).name;
        let response = host.update(cx, |host, cx| {
            host.request(
                Request::Action {
                    id,
                    action: AgentAction::Start,
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = response.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => cx.emit(Dismissed),
                Err(error) => this.fail(
                    format!("{name} was created, but it did not start: {error}"),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn save(
        &mut self,
        host: Entity<HostConnection>,
        id: String,
        revision: u64,
        edit: AgentEdit,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        self.error = None;
        self.status = Some("Saving…".into());
        cx.notify();
        let response = host.update(cx, |host, cx| {
            host.request(
                Request::Save {
                    id: id.clone(),
                    revision,
                    edit,
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = response.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(response) => {
                    // A renamed agent needs its profile published again.
                    let pending = matches!(&response, Response::Agents(snapshot)
                        if snapshot.agents.iter().any(|a| a.id == id && a.profile_pending));
                    if pending {
                        host.update(cx, |host, cx| {
                            host.request(Request::PublishProfile { id }, cx).detach()
                        });
                    }
                    cx.emit(Dismissed);
                }
                Err(error) => this.fail(error.to_string(), cx),
            });
        })
        .detach();
    }

    fn toggle(&mut self, picker: Picker, cx: &mut Context<Self>) {
        self.open = (self.open != Some(picker)).then_some(picker);
        cx.notify();
    }

    fn dismiss(cx: &mut Context<Self>) -> Dismiss {
        let editor = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = editor.update(cx, |this, cx| {
                this.open = None;
                cx.notify();
            });
        })
    }

    /// A select with its menu while open.
    fn select(
        &self,
        picker: Picker,
        trigger: Select,
        options: Menu,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.open == Some(picker);
        div()
            .relative()
            .w_full()
            .child(
                trigger
                    .open(open)
                    .surface(Surface::Floating)
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(picker, cx))),
            )
            .when(open, |this| {
                this.child(overlay::dropdown(
                    window,
                    Align::Start,
                    div().min_w(rems(20.)).child(options),
                    Self::dismiss(cx),
                ))
            })
    }

    fn host_field(
        &self,
        hosts: &[Entity<HostConnection>],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let selected = self.host.as_ref().map(|host| host.read(cx).label.clone());
        let mut menu = Menu::new().size(MenuSize::Wide);
        for (index, host) in hosts.iter().enumerate() {
            let state = host.read(cx);
            let online = connected(host, cx);
            let label: SharedString = if online {
                state.label.clone()
            } else {
                format!("{} (offline)", state.label).into()
            };
            let checked = self.host.as_ref().map(Entity::entity_id) == Some(host.entity_id());
            let pick = host.clone();
            menu = menu.child(
                MenuItem::new(("host", index), label)
                    .checked(checked)
                    .disabled(!online)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.host.as_ref().map(Entity::entity_id) != Some(pick.entity_id()) {
                            this.host = Some(pick.clone());
                            this.choice = None;
                            this.request_id = uuid::Uuid::new_v4().to_string();
                        }
                        this.open = None;
                        cx.notify();
                    })),
            );
        }
        let trigger = Select::new("host-select")
            .placeholder("Choose where it runs")
            .when_some(selected, |this, label| this.value(label));
        field(
            "Host",
            Some("The machine that runs this agent.".into()),
            self.select(Picker::Host, trigger, menu, window, cx),
            &c,
        )
    }

    fn community_field(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let options = communities(cx);
        let selected = self.community.as_ref().map(|url| {
            options
                .iter()
                .find(|(option, _)| option == url)
                .map_or(url.clone(), |(_, name)| name.clone())
        });
        let mut menu = Menu::new().size(MenuSize::Wide);
        for (index, (url, name)) in options.iter().enumerate() {
            let tls = url.starts_with("wss://");
            let label: SharedString = if tls {
                format!("{name} · {url}").into()
            } else {
                format!("{name} (agents need wss://)").into()
            };
            let pick = url.clone();
            menu = menu.child(
                MenuItem::new(("community", index), label)
                    .checked(self.community.as_ref() == Some(url))
                    .disabled(!tls)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.community.as_ref() != Some(&pick) {
                            this.community = Some(pick.clone());
                            this.request_id = uuid::Uuid::new_v4().to_string();
                        }
                        this.open = None;
                        cx.notify();
                    })),
            );
        }
        if options.is_empty() {
            menu =
                menu.child(MenuItem::new("no-community", "Add a community first").disabled(true));
        }
        let trigger = Select::new("community-select")
            .placeholder("Choose a community")
            .when_some(selected, |this, name| this.value(name));
        field(
            "Community",
            Some("The agent joins this relay with its own identity.".into()),
            self.select(Picker::Community, trigger, menu, window, cx),
            &c,
        )
    }

    fn harness_fields(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let harnesses = self.harnesses(cx);
        let loading = self
            .host
            .as_ref()
            .is_some_and(|host| host.read(cx).harnesses.is_none());
        let choice = self.choice(cx);
        let current = match choice {
            Choice::Harness(kind) => harnesses.iter().find(|harness| harness.kind == kind),
            Choice::Custom => None,
        };
        let mut menu = Menu::new().size(MenuSize::Wide);
        for (index, harness) in harnesses.iter().enumerate() {
            let ready = harness.command.is_some();
            let label: SharedString = if ready {
                harness.label.clone().into()
            } else {
                format!(
                    "{} (install first: {})",
                    harness.label,
                    harness.missing.join(", ")
                )
                .into()
            };
            let kind = harness.kind;
            menu = menu.child(
                MenuItem::new(("harness", index), label)
                    .checked(choice == Choice::Harness(kind))
                    .disabled(!ready)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choice = Some(Choice::Harness(kind));
                        this.open = None;
                        cx.notify();
                    })),
            );
        }
        menu = menu.child(
            MenuItem::new("harness-custom", "Custom executable")
                .checked(choice == Choice::Custom)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.choice = Some(Choice::Custom);
                    this.open = None;
                    cx.notify();
                })),
        );
        let (value, kind) = match (choice, current) {
            (Choice::Harness(kind), Some(harness)) => (harness.label.clone(), Some(kind)),
            (Choice::Harness(kind), None) => (format!("{kind:?}"), Some(kind)),
            (Choice::Custom, _) => ("Custom executable".to_string(), None),
        };
        let trigger = Select::new("harness-select")
            .value(value.clone())
            .when_some(kind, |this, kind| this.leading(harness::mark(kind, 16.)));
        let missing: Vec<String> = harnesses
            .iter()
            .filter(|harness| harness.command.is_none())
            .map(|harness| format!("{} needs {}", harness.label, harness.missing.join(", ")))
            .collect();
        let help: Option<SharedString> = if loading {
            Some("Checking installed harnesses…".into())
        } else if let Some(harness) = current.filter(|harness| harness.command.is_none()) {
            Some(format!("{}. {}", harness.missing.join(", "), harness.setup_hint).into())
        } else if !missing.is_empty() {
            Some(format!("Not installed on this host: {}.", missing.join("; ")).into())
        } else {
            None
        };
        let preset = kind.filter(|kind| harness::is_preset(*kind));
        let provider_label = match kind {
            Some(HarnessKind::BuzzAgent) => "Provider",
            _ => "LLM Provider",
        };
        let mut fields = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(legend("AI configuration"))
            .child(field(
                "Harness",
                help,
                self.select(Picker::Harness, trigger, menu, window, cx),
                &c,
            ));
        if choice == Choice::Custom {
            fields = fields.child(field(
                "Executable",
                Some("An ACP agent command, run under buzz-acp.".into()),
                TextField::new(&self.executable).surface(Surface::Floating),
                &c,
            ));
        }
        if preset.is_some() {
            let hint = current.map(|h| h.setup_hint.clone()).unwrap_or_default();
            fields = fields.child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(format!(
                        "{value} uses its own default model and sign-in. {hint}"
                    )),
            );
        } else {
            fields = fields.child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap(rems(1.))
                    .child(field(
                        provider_label,
                        None,
                        TextField::new(&self.provider).surface(Surface::Floating),
                        &c,
                    ))
                    .child(field(
                        "Model",
                        None,
                        TextField::new(&self.model).surface(Surface::Floating),
                        &c,
                    )),
            );
        }
        fields
    }

    fn workspace_field(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let default = self
            .host
            .as_ref()
            .map(|host| host.read(cx).agents.default_workspace.clone())
            .filter(|path| !path.is_empty());
        let help: Option<SharedString> = match (&self.mode, default) {
            (Mode::Create { .. }, Some(path)) => {
                Some(format!("Leave empty to use the host's default, {path}.").into())
            }
            (Mode::Create { .. }, None) => Some("Leave empty to use the host's default.".into()),
            (Mode::Edit { .. }, _) => None,
        };
        div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(legend("Environment"))
            .child(field(
                "Workspace",
                help,
                TextField::new(&self.workspace).surface(Surface::Floating),
                &c,
            ))
    }
}

impl Render for AgentEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let close = cx.listener(|this, _, _, cx| {
            if !this.busy || this.created.is_some() {
                cx.emit(Dismissed)
            }
        });
        let basics = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(field(
                "Name",
                None,
                TextField::new(&self.name).surface(Surface::Floating),
                &c,
            ))
            .child(field(
                "Agent instructions",
                None,
                TextArea::new(&self.instructions)
                    .rows(6)
                    .surface(Surface::Floating),
                &c,
            ));
        let messages = div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .children(
                self.status
                    .clone()
                    .map(|status| div().text_body_sm().child(status)),
            )
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            );

        match &self.mode {
            Mode::Create { hosts } => {
                let destination = self
                    .community
                    .clone()
                    .unwrap_or_else(|| "a connected community".into());
                let label = if self.created.is_some() {
                    "Start agent"
                } else if self.busy {
                    "Creating…"
                } else {
                    "Create agent"
                };
                let hosts = hosts.clone();
                Dialog::new("add-agent", "Add agent")
                    .size(DialogSize::Px(860.))
                    .heading_title()
                    .description(format!(
                        "Create and start an agent in {destination}. It won't join a channel \
                         automatically."
                    ))
                    .on_close(close)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(2.))
                            .child(basics)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(rems(1.))
                                    .child(legend("Location"))
                                    .child(
                                        div()
                                            .grid()
                                            .grid_cols(2)
                                            .gap(rems(1.))
                                            .child(self.host_field(&hosts, window, cx))
                                            .child(self.community_field(window, cx)),
                                    ),
                            )
                            .child(self.harness_fields(window, cx))
                            .child(self.workspace_field(cx))
                            .child(messages),
                    )
                    .footer(
                        Button::new(
                            "cancel-agent",
                            if self.created.is_some() {
                                "Close"
                            } else {
                                "Cancel"
                            },
                        )
                        .surface(Surface::Floating)
                        .disabled(self.busy && self.created.is_none())
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
                    )
                    .footer(
                        Button::new("create-agent", label)
                            .prominent()
                            .loading(self.busy)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    )
            }
            Mode::Edit { agent } => {
                let agent = agent.clone();
                let host_label = self
                    .host
                    .as_ref()
                    .map(|host| host.read(cx).label.clone())
                    .unwrap_or_default();
                let newer = self.newer(cx);
                let dirty = self.dirty(cx);
                let mut avatar = Avatar::new(agent.name.clone())
                    .squircle()
                    .size(AvatarSize::Large);
                if let Some(picture) = agent.picture.clone().filter(|p| !p.is_empty()) {
                    avatar = avatar.image(SharedString::from(picture));
                }
                let identity = div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(rems(0.75))
                    .text_center()
                    .child(avatar)
                    .child(div().text_label().child(agent.name.clone()))
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(format!("{} · {host_label}", agent.relay_url)),
                    );
                let mode_line = if agent.start_on_app_launch {
                    "Enabled · starts with this app"
                } else {
                    "Manual start · a later sent mention can start this agent"
                };
                let runtime = div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(legend("Runtime"))
                    .child(
                        div()
                            .text_body_sm()
                            .child(super::process_label(agent.status)),
                    )
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(mode_line),
                    )
                    .child(div().text_body_sm().text_color(c.text_subtle).child(
                        match agent.running_revision {
                            Some(running) => format!(
                                "Saved revision {} · Running revision {running}.",
                                agent.revision
                            ),
                            None => format!("Saved revision {}.", agent.revision),
                        },
                    ));
                let detail = |label: &'static str, value: String| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.25))
                        .child(div().text_label_sm().child(label))
                        .child(div().text_mono().text_color(c.text_subtle).child(value))
                };
                let technical = div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(legend("Technical details"))
                    .child(detail("Public key", agent.pubkey.clone()))
                    .child(detail("Relay", agent.relay_url.clone()))
                    .when(!agent.diagnostics.is_empty(), |this| {
                        this.child(detail("Host diagnostics", agent.diagnostics.join("\n")))
                    });
                Dialog::new("edit-agent", "Edit agent")
                    .size(DialogSize::Px(640.))
                    .heading_title()
                    .on_close(close)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(2.))
                            .child(identity)
                            .when(newer.is_some(), |this| {
                                this.child(div().text_body_sm().text_color(c.text_warning).child(
                                    "This agent changed on its host since you opened \
                                             it. Discard your changes to load the saved \
                                             settings.",
                                ))
                            })
                            .child(basics)
                            .child(self.harness_fields(window, cx))
                            .child(self.workspace_field(cx))
                            .child(runtime)
                            .child(technical)
                            .child(messages),
                    )
                    .footer(
                        Button::new("cancel-edit", "Cancel")
                            .surface(Surface::Floating)
                            .disabled(self.busy)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
                    )
                    .when_some(newer, |this, newer| {
                        this.footer(
                            Button::new("discard-edit", "Discard changes")
                                .surface(Surface::Floating)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.error = None;
                                    this.load(&newer, window, cx)
                                })),
                        )
                    })
                    .footer(
                        Button::new("save-edit", "Save changes")
                            .prominent()
                            .loading(self.busy)
                            .disabled(!dirty)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    )
            }
        }
    }
}
