//! The Add agent (§5a.7) and Edit agent (§5a.8) dialogs.
//!
//! Creating takes two round trips: the host generates the agent's key
//! (`CreatePrepare`), this process signs the owner attestation for it with
//! the human key, which never leaves the GUI, and the host saves the agent
//! (`CreateCommit`) and publishes its profile.
//!
//! The Add dialog's sidebar also imports agents from an earlier Buzz
//! install on the chosen host (see [`super::import`]).

use super::form::{
    self, AiFields, Choice, EnvFields, Form, Picker, ProviderChoice, Scope, field, legend, note,
};
use super::harness;
use super::import::ImportPane;
use crate::community::AppState;
use crate::community::IdentityState;
use crate::hosts::HostConnection;
use buzz_host::api::{
    Agent, AgentAction, AgentDefaults, AgentEdit, Harness, HarnessEdit, Request, Response,
    SessionPolicy,
};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, Dialog, DialogSize, Icon, IconName,
    MenuItem, NavigationItem, Select, Surface, TextArea, TextField, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

/// The dialog is done; the page closes it.
pub struct Dismissed;

enum Mode {
    Create { hosts: Vec<Entity<HostConnection>> },
    Edit { agent: Box<Agent> },
}

/// What the Add dialog shows beside its sidebar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Create,
    Import,
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
    effort: Option<String>,
    session: Option<SessionPolicy>,
    workspace: String,
    args: String,
}

pub struct AgentEditor {
    mode: Mode,
    host: Option<Entity<HostConnection>>,
    /// The community's relay URL (`wss://…`).
    community: Option<String>,
    name: Entity<InputState>,
    instructions: Entity<TextareaState>,
    workspace: Entity<InputState>,
    args: Entity<TextareaState>,
    ai: AiFields,
    env: EnvFields,
    /// The Environment accordion.
    environment_open: bool,
    /// The values the dialog opened with (edit only).
    original: Option<Values>,
    open: Option<Picker>,
    pane: Pane,
    import: Option<Entity<ImportPane>>,
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

pub fn connected(host: &Entity<HostConnection>, cx: &App) -> bool {
    matches!(host.read(cx).state, ConnectionState::Connected { .. })
}

/// The communities agents can join: the controller only accepts TLS.
pub fn communities(cx: &App) -> Vec<(String, String)> {
    cx.global::<AppState>()
        .communities
        .iter()
        .map(|community| {
            let community = community.read(cx);
            (community.origin.websocket().to_string(), community.name())
        })
        .collect()
}

impl Form for AgentEditor {
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
        let host = self.host.clone().ok_or("Choose a host.")?;
        if !connected(&host, cx) {
            return Err(format!("{} is not connected.", host.read(cx).label));
        }
        let edit = self.edit_from(cx, true)?;
        let (id, revision) = match &self.mode {
            Mode::Edit { agent } => (Some(agent.id.clone()), agent.revision),
            Mode::Create { .. } => (None, 0),
        };
        Ok((host, Request::Models { id, revision, edit }))
    }
}

impl AgentEditor {
    fn new(
        mode: Mode,
        host: Option<Entity<HostConnection>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        let workspace = cx.new(|cx| InputState::new(window, cx));
        let instructions = cx.new(|cx| TextareaState::new(window, cx));
        let args = cx.new(|cx| TextareaState::new(window, cx));
        args.update(cx, |state, cx| state.set_value("[]", window, cx));
        let mut subscriptions = vec![
            cx.subscribe(&instructions, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&args, |this: &mut Self, _, _: &InputEvent, cx| {
                // Pi browses with its arguments.
                this.ai.invalidate();
                cx.notify()
            }),
        ];
        for state in [&name, &workspace] {
            subscriptions.push(cx.subscribe(state, |_, _, _: &InputEvent, cx| cx.notify()));
        }
        let hosts = match &mode {
            Mode::Create { hosts } => hosts.clone(),
            Mode::Edit { .. } => host.iter().cloned().collect(),
        };
        for host in &hosts {
            subscriptions.push(cx.observe(host, |_, _, cx| cx.notify()));
        }
        if let Some(host) = &host
            && connected(host, cx)
        {
            host.update(cx, |host, cx| host.load_defaults(cx));
        }
        name.update(cx, |state, cx| state.focus(window, cx));
        let ai = AiFields::new(false, window, cx);
        let env = EnvFields::new(window, cx);
        Self {
            mode,
            host,
            community: None,
            name,
            instructions,
            workspace,
            args,
            ai,
            env,
            environment_open: false,
            original: None,
            open: None,
            pane: Pane::Create,
            import: None,
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
        // One listener worker by default; editable like any variable.
        editor
            .env
            .load(&[], &[("BUZZ_ACP_AGENTS", "10")], window, cx);
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
        let kind = match choice {
            Choice::Harness(kind) => Some(kind),
            Choice::Custom => None,
        };
        let set = |state: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut App| {
            let value = value.to_string();
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        };
        set(&self.name, &agent.name, window, cx);
        set(
            &self.ai.executable,
            if choice == Choice::Custom {
                &agent.harness.command
            } else {
                ""
            },
            window,
            cx,
        );
        set(&self.ai.model, &agent.harness.model, window, cx);
        set(&self.ai.api_key, "", window, cx);
        set(&self.workspace, &agent.workspace, window, cx);
        let instructions = agent.system_prompt.clone();
        self.instructions
            .update(cx, |state, cx| state.set_value(instructions, window, cx));
        let args = serde_json::to_string(&agent.harness.args).unwrap_or_else(|_| "[]".into());
        self.args
            .update(cx, |state, cx| state.set_value(args, window, cx));
        self.ai.choice = Some(choice);
        self.ai
            .set_provider(kind, &agent.harness.provider, window, cx);
        self.ai.effort = agent.launch_effort.clone();
        self.ai.session = agent.session_policy;
        self.ai.saved_keys = agent.harness.environment_keys.clone();
        self.ai.invalidate();
        self.env
            .load(&agent.harness.environment_keys, &[], window, cx);
        if let Mode::Edit { agent: saved } = &mut self.mode {
            **saved = agent.clone();
        }
        self.original = Some(self.values(cx));
        cx.notify();
    }

    fn harnesses(&self, cx: &App) -> Vec<Harness> {
        self.host
            .as_ref()
            .and_then(|host| host.read(cx).harnesses.clone())
            .unwrap_or_default()
    }

    fn defaults(&self, cx: &App) -> Option<AgentDefaults> {
        self.host
            .as_ref()
            .and_then(|host| host.read(cx).defaults.clone())
    }

    fn values(&self, cx: &App) -> Values {
        let text = |state: &Entity<InputState>| state.read(cx).value().trim().to_string();
        let harnesses = self.harnesses(cx);
        let defaults = self.defaults(cx);
        let scope = Scope {
            harnesses: &harnesses,
            harnesses_loading: false,
            defaults: defaults.as_ref(),
            open: None,
            busy: false,
        };
        Values {
            name: text(&self.name),
            instructions: self.instructions.read(cx).value().to_string(),
            choice: self.ai.choice(&scope),
            executable: text(&self.ai.executable),
            provider: self.ai.provider_id(cx),
            model: self.ai.model_id(cx),
            effort: self.ai.effort.clone(),
            session: self.ai.session,
            workspace: text(&self.workspace),
            args: self.args.read(cx).value().trim().to_string(),
        }
    }

    fn dirty(&self, cx: &App) -> bool {
        self.original
            .as_ref()
            .is_none_or(|original| *original != self.values(cx))
            || !self.env.patch(cx).is_empty()
            || !self.ai.api_key.read(cx).value().trim().is_empty()
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

    /// The form as an edit. `for_models` builds the draft a model lookup
    /// needs, which may still lack a name.
    fn edit_from(&self, cx: &App, for_models: bool) -> Result<AgentEdit, String> {
        let values = self.values(cx);
        if values.name.is_empty() && !for_models {
            return Err("Enter an agent name.".into());
        }
        let saved = match &self.mode {
            Mode::Edit { agent } => Some(agent),
            Mode::Create { .. } => None,
        };
        let harnesses = self.harnesses(cx);
        let (command, kind) = match values.choice {
            Choice::Custom if values.executable.is_empty() => {
                return Err("Enter a harness executable.".into());
            }
            Choice::Custom => (values.executable.clone(), None),
            Choice::Harness(kind) => {
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
                (command, Some(kind))
            }
        };
        if saved.is_some() && values.workspace.is_empty() && !for_models {
            return Err("Enter a workspace path.".into());
        }
        let args: Vec<String> = serde_json::from_str(if values.args.is_empty() {
            "[]"
        } else {
            &values.args
        })
        .map_err(|_| "Arguments must be a JSON array of strings.")?;
        if args.iter().any(|arg| arg.is_empty() || arg.contains(',')) {
            return Err("ACP arguments must be nonempty and cannot contain commas.".into());
        }
        // Presets use their own model and sign-in; custom harnesses take
        // their provider from their own environment.
        let (model, provider) = match kind {
            Some(kind) if harness::is_preset(kind) => (String::new(), String::new()),
            None => (values.model.clone(), String::new()),
            Some(_) => (values.model.clone(), values.provider.clone()),
        };
        let original = self.original.as_ref();
        let session_policy = match original {
            Some(original) if original.session == values.session => None,
            _ => Some(values.session),
        };
        let effort = values
            .effort
            .clone()
            .filter(|_| original.is_none_or(|original| original.effort != values.effort))
            .filter(|_| kind.is_some_and(|kind| !buzz_host::harness::efforts(kind).is_empty()));
        let defaults = self.defaults(cx);
        let scope = Scope {
            harnesses: &harnesses,
            harnesses_loading: false,
            defaults: defaults.as_ref(),
            open: None,
            busy: false,
        };
        let mut environment = self.env.patch(cx);
        environment.extend(self.ai.environment(&scope, cx));
        Ok(AgentEdit {
            name: values.name,
            picture: saved.and_then(|agent| agent.picture.clone()),
            system_prompt: values.instructions,
            session_policy,
            workspace: values.workspace,
            harness: HarnessEdit {
                command,
                args,
                model,
                provider,
            },
            environment,
            effort,
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
        let edit = match self.edit_from(cx, false) {
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

    fn set_host(&mut self, host: Entity<HostConnection>, cx: &mut Context<Self>) {
        if self.host.as_ref().map(Entity::entity_id) == Some(host.entity_id()) {
            return;
        }
        host.update(cx, |host, cx| host.load_defaults(cx));
        self.host = Some(host);
        self.ai.choice = None;
        self.ai.provider = ProviderChoice::Inherit;
        self.ai.invalidate();
        self.request_id = uuid::Uuid::new_v4().to_string();
        self.import = None;
    }

    fn host_field(
        &self,
        hosts: &[Entity<HostConnection>],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let selected = self.host.as_ref().map(|host| host.read(cx).label.clone());
        let mut menu = form::Options::new("options");
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
                        this.set_host(pick.clone(), cx);
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
            form::select(
                self.open,
                self.busy,
                Picker::Host,
                trigger,
                menu,
                window,
                cx,
            ),
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
        let mut menu = form::Options::new("options");
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
                            this.import = None;
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
            form::select(
                self.open,
                self.busy,
                Picker::Community,
                trigger,
                menu,
                window,
                cx,
            ),
            &c,
        )
    }

    fn location(
        &self,
        hosts: &[Entity<HostConnection>],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
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
                    .child(self.host_field(hosts, window, cx))
                    .child(self.community_field(window, cx)),
            )
    }

    /// The Environment accordion: workspace, arguments and overrides.
    fn environment(&self, cx: &mut Context<Self>) -> Div {
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
        let open = self.environment_open;
        let header = div()
            .id("environment-toggle")
            .flex()
            .items_center()
            .justify_between()
            .py(rems(0.5))
            .px(rems(0.5))
            .mx(rems(-0.5))
            .rounded(buzz_ui::tokens::radius::CONTROL)
            .cursor_pointer()
            .hover(|this| this.bg(c.affordance_subtle_hover))
            .child(div().text_label().child("Environment"))
            .child(
                Icon::new(if open {
                    IconName::CaretUp
                } else {
                    IconName::CaretDown
                })
                .size(16.)
                .color(c.text_subtle),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.environment_open = !this.environment_open;
                cx.notify();
            }));
        let mut section = div().flex().flex_col().gap(rems(1.)).child(header);
        if open {
            section = section
                .child(field(
                    "Workspace",
                    help,
                    TextField::new(&self.workspace).surface(Surface::Floating),
                    &c,
                ))
                .child(field(
                    "Arguments (JSON array)",
                    None,
                    TextArea::new(&self.args)
                        .rows(3)
                        .code()
                        .surface(Surface::Floating),
                    &c,
                ))
                .child(self.env.render("Environment overrides", self.busy, cx))
                .child(note(
                    "Environment overrides take precedence over provider and model \
                     selections. Arguments are passed literally, not through a shell.",
                    &c,
                ));
        }
        section
    }

    fn settings(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let harnesses = self.harnesses(cx);
        let loading = self
            .host
            .as_ref()
            .is_some_and(|host| host.read(cx).harnesses.is_none());
        let defaults = self.defaults(cx);
        let scope = Scope {
            harnesses: &harnesses,
            harnesses_loading: loading,
            defaults: defaults.as_ref(),
            open: self.open,
            busy: self.busy,
        };
        let ai = self.ai.render(&scope, window, cx);
        div()
            .flex()
            .flex_col()
            .gap(rems(2.))
            .child(ai)
            .child(self.environment(cx))
    }

    fn basics(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        div()
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
            ))
    }

    fn messages(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        div()
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
            )
    }

    fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pane = Pane::Import;
        self.open = None;
        if self.import.is_none() {
            let editor = cx.entity().downgrade();
            let pane = cx.new(|cx| ImportPane::new(editor, window, cx));
            self._subscriptions
                .push(cx.observe(&pane, |_, _, cx| cx.notify()));
            self.import = Some(pane);
        }
        if let Some(pane) = &self.import {
            let (host, community) = (self.host.clone(), self.community.clone());
            pane.update(cx, |pane, cx| pane.ensure(host, community, cx));
        }
        cx.notify();
    }

    /// The Add dialog's sidebar (`.agent-add-sidebar`).
    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let mut sidebar = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(rems(12.))
            .gap(rems(0.25))
            .pr(rems(0.75))
            .border_r_1()
            .border_color(c.border_standard)
            .child(
                NavigationItem::new("create-agent", "Create agent")
                    .icon(IconName::Plus)
                    .selected(self.pane == Pane::Create)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pane = Pane::Create;
                        if let Some(pane) = &this.import {
                            pane.update(cx, |pane, _| pane.selected = None);
                        }
                        cx.notify();
                    })),
            )
            .child(
                div().pt(rems(0.25)).child(
                    Button::new("import-agents", "Import from Buzz")
                        .outline()
                        .size(ButtonSize::Sm)
                        .icon(IconName::Download)
                        .full_width()
                        .selected(self.pane == Pane::Import)
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.open_import(window, cx))),
                ),
            );
        if let Some(pane) = &self.import {
            sidebar = sidebar.child(pane.read(cx).sidebar(pane.clone(), cx));
        }
        sidebar
    }
}

impl Render for AgentEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        {
            let harnesses = self.harnesses(cx);
            let defaults = self.defaults(cx);
            let scope = Scope {
                harnesses: &harnesses,
                harnesses_loading: false,
                defaults: defaults.as_ref(),
                open: None,
                busy: false,
            };
            self.ai.sync(&scope, window, cx);
        }
        let close = cx.listener(|this, _, _, cx| {
            if !this.busy || this.created.is_some() {
                cx.emit(Dismissed)
            }
        });

        match &self.mode {
            Mode::Create { hosts } => {
                let hosts = hosts.clone();
                let destination = self
                    .community
                    .clone()
                    .unwrap_or_else(|| "a connected community".into());
                let body = match (self.pane, &self.import) {
                    (Pane::Import, Some(pane)) => div()
                        .flex()
                        .flex_col()
                        .gap(rems(2.))
                        .child(self.location(&hosts, window, cx))
                        .child(pane.clone()),
                    _ => div()
                        .flex()
                        .flex_col()
                        .gap(rems(2.))
                        .child(self.basics(cx))
                        .child(self.location(&hosts, window, cx))
                        .child(self.settings(window, cx))
                        .child(self.messages(cx)),
                };
                let mut dialog = Dialog::new("add-agent", "Add agent")
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
                            .gap(rems(1.5))
                            .child(self.sidebar(cx))
                            .child(div().flex_1().min_w_0().child(body)),
                    );
                match (self.pane, &self.import) {
                    (Pane::Import, Some(pane)) => {
                        for button in pane.read(cx).footer(pane.clone(), cx) {
                            dialog = dialog.footer(button);
                        }
                    }
                    _ => {
                        let label = if self.created.is_some() {
                            "Start agent"
                        } else if self.busy {
                            "Creating…"
                        } else {
                            "Create agent"
                        };
                        dialog = dialog
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
                            );
                    }
                }
                dialog
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
                    .child(note(mode_line, &c))
                    .child(note(
                        match agent.running_revision {
                            Some(running) => format!(
                                "Saved revision {} · Running revision {running}.",
                                agent.revision
                            ),
                            None => format!("Saved revision {}.", agent.revision),
                        },
                        &c,
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
                            .child(self.basics(cx))
                            .child(self.settings(window, cx))
                            .child(runtime)
                            .child(technical)
                            .child(self.messages(cx)),
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
