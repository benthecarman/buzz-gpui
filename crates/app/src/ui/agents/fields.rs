//! The settings the Add and Edit dialogs share: buzz-app's
//! `AgentSettingsFields`, `AgentHarnessEditor`, `AgentModelPicker`,
//! `ProviderApiKeyField` and `AgentEnvironmentEditor`.
//!
//! The [`Draft`] is the source of truth; text inputs copy into it as they
//! change. Environment values and API keys are write-only: the host lists
//! saved keys, never values.

use super::draft::{self, ApiKey, Draft};
use super::form::{field, legend};
use crate::hosts::HostConnection;
use crate::hosts::models::Lookup;
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_host::api::{
    AgentsSnapshot, Catalog, ConfigurationPolicy, DatabricksSettings, HarnessOption, ModelAction,
    ModelOption, ModelRequest, SessionPolicy,
};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, Colors, Icon, IconButton, IconButtonSize,
    IconName, Menu, MenuItem, MenuSize, Select, Spinner, Surface, TextArea, TextField, TypeStyled,
    tokens::radius,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Goose catalogs are capped in the list; typing searches the rest.
const VISIBLE_MODEL_LIMIT: usize = 10;

/// A dialog that holds [`Fields`].
pub trait Owner: Sized + 'static {
    fn fields(&mut self) -> &mut Fields;
    /// "Open Harnesses in Settings" was chosen.
    fn open_harnesses(&mut self, _: &mut Window, _: &mut Context<Self>) {}
}

/// The settings selects; one is open at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    Harness,
    Provider,
    Session,
    Model,
}

/// Which text input changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Input {
    Name,
    Instructions,
    Executable,
    CustomProvider,
    ApiKey,
    ModelQuery,
    ModelId,
    Workspace,
    Args,
    DatabricksHost,
    DatabricksFilter,
    NewKey,
}

#[derive(Clone, Debug)]
enum TestResult {
    Testing,
    Passed(Option<String>),
    Failed(String),
}

type Pick<T> = Rc<dyn Fn(&mut T, &mut Window, &mut Context<T>)>;

pub struct Fields {
    pub host: Entity<HostConnection>,
    /// The saved agent being edited: its ID and revision.
    pub saved: Option<(String, u64)>,
    pub environment_keys: Vec<String>,
    pub draft: Draft,
    /// Any field changed since the dialog opened.
    pub dirty: bool,
    /// The dialog's error, cleared by any change.
    pub error: Option<SharedString>,
    name: Entity<InputState>,
    instructions: Entity<TextareaState>,
    executable: Entity<InputState>,
    custom_provider: Entity<InputState>,
    api_key: Entity<InputState>,
    model_query: Entity<InputState>,
    model_id: Entity<InputState>,
    workspace: Entity<InputState>,
    args: Entity<TextareaState>,
    databricks_host: Entity<InputState>,
    databricks_filter: Entity<InputState>,
    env_inputs: BTreeMap<String, Entity<InputState>>,
    new_key: Entity<InputState>,
    env_error: Option<SharedString>,
    /// "Custom executable / current value" is chosen.
    harness_custom: bool,
    provider_custom: bool,
    pub open: Option<Open>,
    advanced_open: bool,
    environment_open: bool,
    revealed: bool,
    catalog: Option<(String, Catalog)>,
    status: String,
    busy: bool,
    lookup: Option<Lookup>,
    attempted: Option<String>,
    query: Option<String>,
    key: String,
    test: Option<(String, TestResult)>,
    testing: Option<Lookup>,
    /// Pi's signed-in providers; `None` while its catalog loads.
    pi_providers: Option<Vec<String>>,
    /// The command whose choice loaded Pi's catalog.
    pi_loaded: Option<String>,
    /// The placeholder each input was last given.
    placeholders: BTreeMap<String, String>,
    _subscriptions: Vec<Subscription>,
}

fn watch<T: Owner>(
    input: Input,
    state: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Subscription {
    cx.subscribe_in(
        state,
        window,
        move |this: &mut T, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => this.fields().changed(input, cx),
                InputEvent::PressEnter { .. } | InputEvent::Blur if input == Input::ModelQuery => {
                    let fields = this.fields();
                    fields.commit_query(window, cx);
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        fields.open = None;
                    }
                }
                InputEvent::PressEnter { .. } if input == Input::NewKey => {
                    this.fields().add_variable(window, cx)
                }
                _ => {}
            }
            cx.notify();
        },
    )
}

fn watch_area<T: Owner>(
    input: Input,
    state: &Entity<TextareaState>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Subscription {
    cx.subscribe_in(
        state,
        window,
        move |this: &mut T, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.fields().changed(input, cx);
                cx.notify();
            }
        },
    )
}

impl Fields {
    pub fn new<T: Owner>(
        host: Entity<HostConnection>,
        saved: Option<(String, u64)>,
        environment_keys: Vec<String>,
        draft: Draft,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Self {
        let input = |value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<T>| {
            let (value, placeholder) = (value.to_string(), placeholder.to_string());
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(value, window, cx);
                state
            })
        };
        let area = |value: &str, window: &mut Window, cx: &mut Context<T>| {
            let value = value.to_string();
            cx.new(|cx| {
                let mut state = TextareaState::new(window, cx);
                state.set_value(value, window, cx);
                state
            })
        };
        let data = host.read(cx).snapshot();
        let name = input(&draft.name, "", window, cx);
        let instructions = area(&draft.system_prompt, window, cx);
        let executable = input(&draft.command, "", window, cx);
        let custom_provider = input(&draft.provider, "", window, cx);
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let model_query = input(&draft.model, "Choose or enter a model", window, cx);
        let model_id = input(&draft.model, "", window, cx);
        let workspace = input(&draft.workspace, "", window, cx);
        let args = area(&draft.args, window, cx);
        let (host_value, filter_value) = match &draft.databricks {
            Some(settings) => (settings.host.clone(), settings.filter.clone()),
            None => {
                let build = data.databricks_defaults.clone().unwrap_or_default();
                (build.host, build.filter)
            }
        };
        let databricks_host = input(&host_value, "https://workspace.example.com", window, cx);
        let databricks_filter = input(&filter_value, "", window, cx);
        let new_key = input("", "", window, cx);
        let subscriptions = vec![
            watch(Input::Name, &name, window, cx),
            watch_area(Input::Instructions, &instructions, window, cx),
            watch(Input::Executable, &executable, window, cx),
            watch(Input::CustomProvider, &custom_provider, window, cx),
            watch(Input::ApiKey, &api_key, window, cx),
            watch(Input::ModelQuery, &model_query, window, cx),
            watch(Input::ModelId, &model_id, window, cx),
            watch(Input::Workspace, &workspace, window, cx),
            watch_area(Input::Args, &args, window, cx),
            watch(Input::DatabricksHost, &databricks_host, window, cx),
            watch(Input::DatabricksFilter, &databricks_filter, window, cx),
            watch(Input::NewKey, &new_key, window, cx),
        ];
        let mut fields = Self {
            host,
            saved,
            environment_keys,
            draft,
            dirty: false,
            error: None,
            name,
            instructions,
            executable,
            custom_provider,
            api_key,
            model_query,
            model_id,
            workspace,
            args,
            databricks_host,
            databricks_filter,
            env_inputs: BTreeMap::new(),
            new_key,
            env_error: None,
            harness_custom: false,
            provider_custom: false,
            open: None,
            advanced_open: false,
            environment_open: false,
            revealed: false,
            catalog: None,
            status: String::new(),
            busy: false,
            lookup: None,
            attempted: None,
            query: None,
            key: String::new(),
            test: None,
            testing: None,
            pi_providers: Some(Vec::new()),
            pi_loaded: None,
            placeholders: BTreeMap::new(),
            _subscriptions: subscriptions,
        };
        let options = fields.options(&data);
        fields.harness_custom = !options
            .iter()
            .any(|option| option.command == fields.draft.command)
            && buzz_host::harness::preset(&fields.draft.command).is_none();
        fields.provider_custom = fields.provider_needs_input(&data);
        fields.sync_env_inputs(window, cx);
        fields.key = fields.context_key();
        fields
    }

    pub fn focus_name(&self, window: &mut Window, cx: &mut App) {
        self.name.update(cx, |state, cx| state.focus(window, cx));
    }

    fn data(&self, cx: &App) -> AgentsSnapshot {
        self.host.read(cx).snapshot()
    }

    /// Copies a changed input into the draft.
    fn changed<T>(&mut self, input: Input, cx: &mut Context<T>) {
        let text = |state: &Entity<InputState>, cx: &App| state.read(cx).value().to_string();
        match input {
            Input::Name => self.draft.name = text(&self.name, cx),
            Input::Instructions => {
                self.draft.system_prompt = self.instructions.read(cx).value().to_string()
            }
            Input::Executable => self.draft.command = text(&self.executable, cx),
            Input::CustomProvider => self.draft.provider = text(&self.custom_provider, cx),
            Input::ApiKey => {
                let value = text(&self.api_key, cx);
                let data = self.data(cx);
                if let Some(key) = self.api_key(&data) {
                    if value.is_empty() {
                        self.draft.environment.remove(key.env);
                        self.revealed = false;
                    } else {
                        self.draft
                            .environment
                            .insert(key.env.to_string(), Some(value));
                    }
                }
            }
            Input::ModelQuery => {
                // Pending text is an unsaved edit too.
                self.query = Some(text(&self.model_query, cx));
                self.open = Some(Open::Model);
            }
            Input::ModelId => self.draft.model = text(&self.model_id, cx),
            Input::Workspace => self.draft.workspace = text(&self.workspace, cx),
            Input::Args => self.draft.args = self.args.read(cx).value().to_string(),
            Input::DatabricksHost | Input::DatabricksFilter => {
                // Editing either field saves both displayed values.
                self.draft.databricks = Some(DatabricksSettings {
                    host: text(&self.databricks_host, cx),
                    filter: text(&self.databricks_filter, cx),
                });
            }
            Input::NewKey => {
                self.env_error = None;
                return;
            }
        }
        self.dirty = true;
        self.error = None;
    }

    /// Replaces the draft (Discard changes) and shows it.
    pub fn reset(&mut self, draft: Draft, window: &mut Window, cx: &mut App) {
        let set = |state: &Entity<InputState>, value: &str, window: &mut Window, cx: &mut App| {
            let value = value.to_string();
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        };
        set(&self.name, &draft.name, window, cx);
        let prompt = draft.system_prompt.clone();
        self.instructions
            .update(cx, |state, cx| state.set_value(prompt, window, cx));
        set(&self.executable, &draft.command, window, cx);
        set(&self.custom_provider, &draft.provider, window, cx);
        set(&self.api_key, "", window, cx);
        set(&self.model_query, &draft.model, window, cx);
        set(&self.model_id, &draft.model, window, cx);
        set(&self.workspace, &draft.workspace, window, cx);
        let args = draft.args.clone();
        self.args
            .update(cx, |state, cx| state.set_value(args, window, cx));
        for state in self.env_inputs.values() {
            set(state, "", window, cx);
        }
        self.draft = draft;
        self.dirty = false;
        self.error = None;
        self.query = None;
    }

    /// Applies a change made by a control rather than by typing.
    fn patch(&mut self, change: impl FnOnce(&mut Draft), window: &mut Window, cx: &mut App) {
        let data = self.data(cx);
        let before = self.api_key(&data);
        let model = self.draft.model.clone();
        change(&mut self.draft);
        // A typed key belongs to the provider it was entered for.
        if let Some(before) = before
            && self.api_key(&data).map(|key| key.env) != Some(before.env)
            && matches!(self.draft.environment.get(before.env), Some(Some(_)))
        {
            self.draft.environment.remove(before.env);
            self.api_key
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.revealed = false;
        }
        if self.draft.model != model {
            let value = self.draft.model.clone();
            self.model_id
                .update(cx, |state, cx| state.set_value(value.clone(), window, cx));
            self.model_query
                .update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.dirty = true;
        self.error = None;
    }

    fn sync_env_inputs<T: Owner>(&mut self, window: &mut Window, cx: &mut Context<T>) {
        let mut names: Vec<String> = self.environment_keys.clone();
        for key in self.draft.environment.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
        for key in names {
            if self.env_inputs.contains_key(&key) {
                continue;
            }
            let masked = key != "BUZZ_ACP_AGENTS";
            let value = self
                .draft
                .environment
                .get(&key)
                .cloned()
                .flatten()
                .unwrap_or_default();
            let state = cx.new(|cx| {
                let mut state = InputState::new(window, cx).masked(masked);
                state.set_value(value, window, cx);
                state
            });
            let name = key.clone();
            self._subscriptions.push(cx.subscribe_in(
                &state,
                window,
                move |this: &mut T, state, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = state.read(cx).value().to_string();
                        let fields = this.fields();
                        fields.draft.environment.insert(name.clone(), Some(value));
                        fields.dirty = true;
                        fields.error = None;
                        cx.notify();
                    }
                },
            ));
            self.env_inputs.insert(key, state);
        }
    }

    fn add_variable(&mut self, window: &mut Window, cx: &mut App) {
        let key = self.new_key.read(cx).value().trim().to_string();
        let valid = key
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
            && key.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
        if !valid {
            self.env_error = Some(
                "Use letters, numbers and underscores; start with a letter or underscore.".into(),
            );
            return;
        }
        if self.environment_keys.contains(&key) || self.draft.environment.contains_key(&key) {
            self.env_error = Some("That variable is already listed.".into());
            return;
        }
        self.draft.environment.insert(key, Some(String::new()));
        self.dirty = true;
        self.env_error = None;
        self.new_key
            .update(cx, |state, cx| state.set_value("", window, cx));
    }

    /// The context a catalog belongs to: every draft value the host
    /// resolves it with.
    fn context_key(&self) -> String {
        let pi = draft::is_pi(&self.draft.command);
        let databricks = self.draft.databricks.clone().unwrap_or_default();
        serde_json::to_string(&(
            &self.saved,
            self.draft.revision,
            &self.draft.command,
            (!pi).then_some(&self.draft.provider),
            &self.draft.args,
            &self.draft.workspace,
            &self.draft.environment,
            (!pi).then_some((&databricks.host, &databricks.filter)),
        ))
        .unwrap_or_default()
    }

    fn test_key(&self) -> String {
        format!("{}|{}|{}", self.key, self.draft.provider, self.draft.model)
    }

    /// Keeps lookups and placeholders in step with the draft. Call before
    /// each render.
    pub fn sync<T: Owner>(&mut self, window: &mut Window, cx: &mut Context<T>) {
        self.sync_env_inputs(window, cx);
        let key = self.context_key();
        if key != self.key {
            self.key = key;
            if let Some(lookup) = self.lookup.take() {
                self.host
                    .update(cx, |host, cx| host.cancel_models(&lookup, cx));
            }
            self.busy = false;
            self.status.clear();
            self.query = None;
            if self.open == Some(Open::Model) {
                self.open = None;
            }
            self.attempted = None;
        }
        if self
            .test
            .as_ref()
            .is_some_and(|(key, _)| *key != self.test_key())
        {
            if let Some(testing) = self.testing.take() {
                self.host
                    .update(cx, |host, cx| host.cancel_models(&testing, cx));
            }
            self.test = None;
        }
        // Pi's catalog is headless and names its signed-in providers, so it
        // loads as soon as Pi is chosen.
        let pi = self.is_pi(cx);
        if pi && self.pi_loaded.as_deref() != Some(self.draft.command.as_str()) {
            self.pi_loaded = Some(self.draft.command.clone());
            run(self, ModelAction::Connect, cx);
        } else if !pi {
            self.pi_loaded = None;
            self.pi_providers = Some(Vec::new());
        }
        let data = self.data(cx);
        let placeholder = match self.default_model(&data) {
            Some(model) => format!("Use agent defaults ({model})"),
            None => "Choose or enter a model".into(),
        };
        Self::placeholder(
            &mut self.placeholders,
            "model",
            &self.model_query,
            placeholder,
            window,
            cx,
        );
        let (inherit_host, inherit_filter) = self.inherited_workspace(&data);
        let host_placeholder = if inherit_host {
            "Use agent defaults"
        } else {
            "https://workspace.example.com"
        };
        Self::placeholder(
            &mut self.placeholders,
            "host",
            &self.databricks_host,
            host_placeholder.into(),
            window,
            cx,
        );
        let filter_placeholder = if inherit_filter {
            "Use agent defaults"
        } else {
            ""
        };
        Self::placeholder(
            &mut self.placeholders,
            "filter",
            &self.databricks_filter,
            filter_placeholder.into(),
            window,
            cx,
        );
        let api = self.api_key_placeholder(&data);
        Self::placeholder(
            &mut self.placeholders,
            "api",
            &self.api_key,
            api,
            window,
            cx,
        );
        for (key, state) in &self.env_inputs {
            let placeholder = match self.draft.environment.get(key) {
                Some(None) => "Will be removed on save",
                _ if self.environment_keys.contains(key) => "Saved value unchanged",
                _ => "New value",
            };
            Self::placeholder(
                &mut self.placeholders,
                &format!("env:{key}"),
                state,
                placeholder.into(),
                window,
                cx,
            );
        }
    }

    fn placeholder(
        cache: &mut BTreeMap<String, String>,
        key: &str,
        state: &Entity<InputState>,
        value: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        if cache.get(key) != Some(&value) {
            cache.insert(key.to_string(), value.clone());
            state.update(cx, |state, cx| state.set_placeholder(value, window, cx));
        }
    }

    fn options(&self, data: &AgentsSnapshot) -> Vec<HarnessOption> {
        data.harness_options.clone().unwrap_or_default()
    }

    fn policy(&self, data: &AgentsSnapshot) -> Option<ConfigurationPolicy> {
        draft::policy(&self.options(data), &self.draft.command).cloned()
    }

    fn is_pi(&self, cx: &App) -> bool {
        self.discovered(&self.data(cx))
    }

    fn discovered(&self, data: &AgentsSnapshot) -> bool {
        match self.policy(data) {
            Some(policy) => policy.provider == "discovered",
            None => draft::is_pi(&self.draft.command),
        }
    }

    /// The harness owns sign-in, with provider overrides.
    fn external(&self, data: &AgentsSnapshot) -> bool {
        match self.policy(data) {
            Some(policy) => policy.authentication == "harnessWithOverrides",
            None => {
                buzz_host::harness::preset(&self.draft.command).is_some()
                    || draft::is_goose(&self.draft.command)
                    || draft::is_pi(&self.draft.command)
            }
        }
    }

    fn buzz_agent(&self) -> bool {
        draft::kind(&self.draft.command).as_deref() == Some("buzz-agent")
    }

    fn own_override(&self, key: &str) -> bool {
        match self.draft.environment.get(key) {
            Some(Some(_)) => true,
            Some(None) => false,
            None => self.environment_keys.iter().any(|saved| saved == key),
        }
    }

    fn overridden(&self, key: &str, data: &AgentsSnapshot) -> bool {
        self.own_override(key)
            || data
                .default_settings
                .as_ref()
                .is_some_and(|d| d.environment_keys.iter().any(|saved| saved == key))
    }

    fn selector_keys(&self, data: &AgentsSnapshot) -> (Option<String>, Option<String>) {
        match self.policy(data) {
            Some(policy) => match policy.selector_environment {
                Some(selector) => (Some(selector.model), Some(selector.provider)),
                None => (None, None),
            },
            None if self.buzz_agent() => (
                Some("BUZZ_AGENT_MODEL".into()),
                Some("BUZZ_AGENT_PROVIDER".into()),
            ),
            None if draft::is_goose(&self.draft.command) => {
                (Some("GOOSE_MODEL".into()), Some("GOOSE_PROVIDER".into()))
            }
            None => (None, None),
        }
    }

    fn inherited<'a>(&self, data: &'a AgentsSnapshot) -> Option<&'a buzz_host::api::AgentDefaults> {
        data.default_settings
            .as_ref()
            .filter(|defaults| Some(defaults.harness.clone()) == draft::kind(&self.draft.command))
    }

    fn default_provider(&self, data: &AgentsSnapshot) -> Option<String> {
        let (_, provider_key) = self.selector_keys(data);
        if provider_key.is_some_and(|key| self.overridden(&key, data)) {
            return None;
        }
        self.inherited(data)
            .map(|d| d.provider.clone())
            .filter(|p| !p.is_empty())
            .or_else(|| {
                self.buzz_agent()
                    .then(|| data.agent_defaults.as_ref().map(|d| d.provider.clone()))
                    .flatten()
            })
            .filter(|p| !p.is_empty())
    }

    fn default_model(&self, data: &AgentsSnapshot) -> Option<String> {
        let (model_key, _) = self.selector_keys(data);
        let known = (matches!(
            self.draft.environment.get("BUZZ_AGENT_PROVIDER"),
            Some(Some(_))
        ) || !self.overridden("BUZZ_AGENT_PROVIDER", data))
            && ["BUZZ_AGENT_MODEL", "DATABRICKS_MODEL"]
                .iter()
                .all(|key| !self.overridden(key, data));
        let hidden = model_key.is_some_and(|key| self.overridden(&key, data))
            || (self.buzz_agent() && !known);
        if hidden {
            return None;
        }
        let provider = draft::effective_buzz_provider(&self.draft, &self.environment_keys, data)
            .unwrap_or_default();
        let databricks = matches!(
            provider.as_str(),
            "databricks_v2" | "databricks-v2" | "databricks"
        );
        self.inherited(data)
            .map(|d| d.model.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| {
                (self.buzz_agent() && databricks)
                    .then(|| data.agent_defaults.as_ref().map(|d| d.model.clone()))
                    .flatten()
            })
            .filter(|m| !m.is_empty())
    }

    /// Agent defaults set the workspace and filter, and this draft does not.
    fn inherited_workspace(&self, data: &AgentsSnapshot) -> (bool, bool) {
        let inherited = |key: &str| {
            self.buzz_agent()
                && self.draft.databricks.is_none()
                && data
                    .default_settings
                    .as_ref()
                    .is_some_and(|d| d.environment_keys.iter().any(|saved| saved == key))
                && !self.own_override(key)
        };
        (
            inherited("DATABRICKS_HOST"),
            inherited("DATABRICKS_MODEL_FILTER"),
        )
    }

    /// The workspace and filter a lookup uses: the draft's, else blank when
    /// inherited, else the build's.
    fn workspace_pair(&self, data: &AgentsSnapshot) -> (String, String) {
        let (inherit_host, inherit_filter) = self.inherited_workspace(data);
        let build = data.databricks_defaults.clone().unwrap_or_default();
        match &self.draft.databricks {
            Some(settings) => (settings.host.clone(), settings.filter.clone()),
            None => (
                if inherit_host {
                    String::new()
                } else {
                    build.host
                },
                if inherit_filter {
                    String::new()
                } else {
                    build.filter
                },
            ),
        }
    }

    fn api_key(&self, data: &AgentsSnapshot) -> Option<ApiKey> {
        draft::provider_api_key(&self.draft, &self.environment_keys, data)
    }

    fn api_key_placeholder(&self, data: &AgentsSnapshot) -> String {
        let Some(key) = self.api_key(data) else {
            return String::new();
        };
        match self.draft.environment.get(key.env) {
            Some(None) => "Will remove on save".into(),
            _ if self.environment_keys.iter().any(|saved| saved == key.env) => {
                "Saved key unchanged".into()
            }
            _ if self.buzz_agent() => "Paste API key".into(),
            _ if draft::is_pi(&self.draft.command) => {
                "Paste API key or use an existing Pi sign-in".into()
            }
            _ => "Paste API key or use existing Goose credentials".into(),
        }
    }

    fn provider_needs_input(&self, data: &AgentsSnapshot) -> bool {
        !self.draft.provider.is_empty()
            && !self
                .provider_choices(data)
                .iter()
                .any(|(value, _)| *value == self.draft.provider)
    }

    /// Provider choices after "Use agent defaults"/"Not set".
    fn provider_choices(&self, data: &AgentsSnapshot) -> Vec<(String, String)> {
        if self.discovered(data) {
            let label = |value: &str| {
                draft::pi_api_key(value)
                    .map(|key| key.label.to_string())
                    .unwrap_or_else(|| value.to_string())
            };
            let current = &self.draft.provider;
            let known: Vec<String> = match &self.pi_providers {
                Some(signed_in) => signed_in.clone(),
                None if !current.is_empty() && draft::pi_api_key(current).is_none() => {
                    vec![current.clone()]
                }
                None => Vec::new(),
            };
            let mut choices: Vec<(String, String)> = known
                .iter()
                .map(|value| (value.clone(), label(value)))
                .collect();
            for (value, _) in draft::PI_API_KEYS {
                if !known.iter().any(|known| known == value) {
                    choices.push((
                        value.to_string(),
                        format!("{} (API key needed)", label(value)),
                    ));
                }
            }
            return choices;
        }
        let options = self.options(data);
        draft::option(&options, &self.draft.command)
            .or_else(|| {
                buzz_host::harness::preset(&self.draft.command).and_then(|_| {
                    options.iter().find(|option| {
                        draft::kind(&option.command) == draft::kind(&self.draft.command)
                    })
                })
            })
            .map(|option| {
                option
                    .providers
                    .iter()
                    .map(|provider| (provider.value.clone(), provider.label.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn entries(&self) -> Vec<ModelOption> {
        let Some(catalog) = self.fresh() else {
            return Vec::new();
        };
        let pi = draft::is_pi(&self.draft.command);
        catalog
            .models
            .iter()
            .filter(|model| {
                !pi || self.draft.provider.is_empty()
                    || model.id.starts_with(&format!("{}/", self.draft.provider))
            })
            .cloned()
            .collect()
    }

    fn fresh(&self) -> Option<&Catalog> {
        self.catalog
            .as_ref()
            .filter(|(key, _)| *key == self.key)
            .map(|(_, catalog)| catalog)
    }

    fn selected_id(&self) -> String {
        if draft::is_pi(&self.draft.command)
            && !self.draft.provider.is_empty()
            && !self.draft.model.is_empty()
        {
            format!("{}/{}", self.draft.provider, self.draft.model)
        } else {
            self.draft.model.clone()
        }
    }

    /// A Pi `provider/model` choice sets both.
    fn choose_model(&mut self, value: &str, window: &mut Window, cx: &mut App) {
        let pi = draft::is_pi(&self.draft.command);
        let entries = self.entries();
        let value = value.to_string();
        if pi
            && entries.iter().any(|model| model.id == value)
            && let Some((provider, model)) = value.split_once('/')
        {
            let (provider, model) = (provider.to_string(), model.to_string());
            self.patch(
                |draft| {
                    draft.provider = provider;
                    draft.model = model;
                },
                window,
                cx,
            );
        } else {
            let prefix = format!("{}/", self.draft.provider);
            let model = if pi && !self.draft.provider.is_empty() && value.starts_with(&prefix) {
                value[prefix.len()..].to_string()
            } else {
                value
            };
            self.patch(|draft| draft.model = model, window, cx);
        }
        self.query = None;
    }

    fn commit_query(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(query) = self.query.clone() {
            let matched = self
                .entries()
                .into_iter()
                .find(|model| model.id == query || model.name == query)
                .map(|model| model.id)
                .unwrap_or(query);
            self.choose_model(&matched, window, cx);
        }
    }
}

/// Starts a model lookup for the draft (`run` in `AgentModelPicker`).
fn run<T: Owner>(fields: &mut Fields, action: ModelAction, cx: &mut Context<T>) {
    if fields.lookup.is_some() {
        return;
    }
    if let Some(testing) = fields.testing.take() {
        fields
            .host
            .update(cx, |host, cx| host.cancel_models(&testing, cx));
    }
    fields.test = None;
    let data = fields.data(cx);
    let external = fields.external(&data);
    let (host, filter) = fields.workspace_pair(&data);
    let (inherit_host, inherit_filter) = fields.inherited_workspace(&data);
    if !external && host.trim().is_empty() && !inherit_host {
        fields.status =
            "Set your Databricks workspace under Advanced → Model to browse models.".into();
        return;
    }
    let edit = match action {
        ModelAction::Disconnect => None,
        _ => match draft::edit(&fields.draft, true) {
            Ok(edit) => Some(edit),
            Err(error) => {
                fields.status = error;
                return;
            }
        },
    };
    fields.attempted = Some(fields.key.clone());
    fields.busy = true;
    fields.catalog = None;
    let pi = draft::is_pi(&fields.draft.command);
    fields.status = match action {
        ModelAction::Connect if external => {
            format!("Loading {} models…", if pi { "Pi" } else { "Goose" })
        }
        ModelAction::Connect => "Loading models… sign in through your browser if asked.".into(),
        ModelAction::Refresh => "Loading models…".into(),
        _ => "Removing this app’s credentials for this workspace…".into(),
    };
    if pi {
        fields.pi_providers = None;
    }
    let request = ModelRequest {
        id: fields.saved.as_ref().map(|(id, _)| id.clone()),
        expected_revision: fields.saved.as_ref().map(|_| fields.draft.revision),
        edit,
        host: if external {
            String::new()
        } else {
            host.clone()
        },
        filter: if external {
            String::new()
        } else {
            filter.clone()
        },
        action,
        inherit_workspace: !external
            && ((inherit_host && host.is_empty()) || (inherit_filter && filter.is_empty())),
    };
    let key = fields.key.clone();
    let goose = draft::is_goose(&fields.draft.command);
    let (task, lookup) = fields.host.update(cx, |host, cx| host.models(request, cx));
    fields.lookup = Some(lookup.clone());
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |this, cx| {
            let fields = this.fields();
            if !fields
                .lookup
                .as_ref()
                .is_some_and(|current| current.same(&lookup))
            {
                return;
            }
            fields.lookup = None;
            fields.busy = false;
            if lookup.cancelled() || fields.key != key {
                return;
            }
            match result {
                Ok(catalog) => {
                    fields.status = if catalog.disconnected {
                        "Disconnected from this workspace in Foundation.".into()
                    } else if !catalog.models.is_empty() {
                        String::new()
                    } else if goose {
                        "No models found for this Goose provider. Check its configuration or \
                         enter a custom ID."
                            .into()
                    } else if pi {
                        "No signed-in Pi providers found. Buzz doesn’t use API keys exported in \
                         your shell profile. Choose a provider under LLM Provider to add its API \
                         key, or enter a custom ID."
                            .into()
                    } else {
                        "No models found. Enter a custom ID or check the workspace/filter under \
                         Advanced → Model."
                            .into()
                    };
                    if pi {
                        let mut providers: Vec<String> = Vec::new();
                        for model in &catalog.models {
                            let provider =
                                model.id.split('/').next().unwrap_or_default().to_string();
                            if !providers.contains(&provider) {
                                providers.push(provider);
                            }
                        }
                        fields.pi_providers = Some(providers);
                    }
                    fields.catalog = Some((key, catalog));
                }
                Err(error) => {
                    if pi {
                        fields.pi_providers = Some(Vec::new());
                    }
                    fields.status = error;
                }
            }
            cx.notify();
        });
    })
    .detach();
}

/// "Test connection": one short completion through the draft.
fn test<T: Owner>(fields: &mut Fields, cx: &mut Context<T>) {
    if fields.testing.is_some() {
        return;
    }
    if let Some(lookup) = fields.lookup.take() {
        fields
            .host
            .update(cx, |host, cx| host.cancel_models(&lookup, cx));
    }
    fields.attempted = None;
    fields.busy = false;
    fields.status.clear();
    fields.open = None;
    let key = fields.test_key();
    let edit = match draft::edit(&fields.draft, true) {
        Ok(edit) => edit,
        Err(error) => {
            fields.test = Some((key, TestResult::Failed(error)));
            return;
        }
    };
    fields.test = Some((key.clone(), TestResult::Testing));
    let request = ModelRequest {
        id: fields.saved.as_ref().map(|(id, _)| id.clone()),
        expected_revision: fields.saved.as_ref().map(|_| fields.draft.revision),
        edit: Some(edit),
        action: ModelAction::Test,
        ..Default::default()
    };
    let (task, lookup) = fields.host.update(cx, |host, cx| host.models(request, cx));
    fields.testing = Some(lookup.clone());
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |this, cx| {
            let fields = this.fields();
            if !fields
                .testing
                .as_ref()
                .is_some_and(|current| current.same(&lookup))
            {
                return;
            }
            fields.testing = None;
            if lookup.cancelled() || fields.test_key() != key {
                fields.test = None;
                return;
            }
            fields.test = Some((
                key,
                match result {
                    Ok(catalog) => TestResult::Passed(catalog.tested_model),
                    Err(error) => TestResult::Failed(error),
                },
            ));
            cx.notify();
        });
    })
    .detach();
}

fn cancel<T: Owner>(fields: &mut Fields, cx: &mut Context<T>) {
    if let Some(lookup) = fields.lookup.take() {
        fields
            .host
            .update(cx, |host, cx| host.cancel_models(&lookup, cx));
    }
    fields.busy = false;
    fields.status = "Cancelled. Retry when ready.".into();
    if fields.pi_providers.is_none() {
        fields.pi_providers = Some(Vec::new());
    }
}

fn note(text: impl Into<SharedString>, c: &Colors) -> Div {
    div()
        .text_body_sm()
        .text_color(c.text_subtle)
        .child(text.into())
}

/// Preset setup hints keep their backticked commands as inline code.
pub fn setup_hint(hint: &str, c: &Colors) -> Div {
    let mut text = div().flex().flex_wrap().items_baseline();
    for (index, part) in hint.split('`').enumerate() {
        if part.is_empty() {
            continue;
        }
        text = if index % 2 == 1 {
            text.child(
                div()
                    .px(px(4.))
                    .rounded(px(4.))
                    .bg(c.affordance_subtle)
                    .text_mono()
                    .child(part.to_string()),
            )
        } else {
            text.child(part.to_string())
        };
    }
    text
}

/// An accordion header (`Accordion variant="form"`).
fn accordion<T: Owner>(
    id: &'static str,
    title: &'static str,
    open: bool,
    c: &Colors,
    cx: &mut Context<T>,
    toggle: impl Fn(&mut Fields) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .py(rems(0.5))
        .px(rems(0.5))
        .mx(rems(-0.5))
        .rounded(radius::CONTROL)
        .cursor_pointer()
        .hover(|this| this.bg(c.affordance_subtle_hover))
        .child(div().text_label().child(title))
        .child(
            Icon::new(if open {
                IconName::CaretUp
            } else {
                IconName::CaretDown
            })
            .size(16.)
            .color(c.text_subtle),
        )
        .on_click(cx.listener(move |this: &mut T, _, _, cx| {
            toggle(this.fields());
            cx.notify();
        }))
}

impl Fields {
    fn dismiss<T: Owner>(cx: &mut Context<T>) -> Dismiss {
        let owner = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = owner.update(cx, |this, cx| {
                this.fields().open = None;
                cx.notify();
            });
        })
    }

    /// A field select with its menu while open.
    #[allow(clippy::too_many_arguments)]
    fn select<T: Owner>(
        &self,
        which: Open,
        id: &'static str,
        value: String,
        disabled: bool,
        items: Vec<(String, bool, bool, Pick<T>)>,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let open = self.open == Some(which);
        let mut menu = Menu::new().size(MenuSize::Wide);
        let mut list = div()
            .id(SharedString::from(format!("{id}-options")))
            .flex()
            .flex_col()
            .max_h(rems(23.))
            .overflow_y_scroll();
        for (index, (label, checked, item_disabled, pick)) in items.into_iter().enumerate() {
            list = list.child(
                MenuItem::new((id, index), label)
                    .checked(checked)
                    .disabled(item_disabled)
                    .on_click(cx.listener(move |this: &mut T, _, window, cx| {
                        this.fields().open = None;
                        pick(this, window, cx);
                        cx.notify();
                    })),
            );
        }
        menu = menu.child(list);
        div()
            .relative()
            .w_full()
            .child(
                Select::new(id)
                    .value(value)
                    .open(open)
                    .surface(Surface::Floating)
                    .disabled(disabled)
                    .on_click(cx.listener(move |this: &mut T, _, _, cx| {
                        let fields = this.fields();
                        fields.open = (fields.open != Some(which)).then_some(which);
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(overlay::dropdown(
                    window,
                    Align::Start,
                    div().min_w(rems(20.)).child(menu),
                    Self::dismiss(cx),
                ))
            })
    }

    /// `AgentSettingsFields`.
    pub fn render<T: Owner>(
        &self,
        disabled: bool,
        discard_note: bool,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let data = self.data(cx);
        let preset = buzz_host::harness::preset(&self.draft.command);
        let goose = draft::is_goose(&self.draft.command);
        let pi = draft::is_pi(&self.draft.command);
        let goose_provider = goose
            .then(|| {
                draft::effective_goose_provider(
                    &self.draft.provider,
                    &self.draft.environment,
                    &self.environment_keys,
                )
            })
            .flatten();
        let mut ai = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(legend("AI configuration"))
            .child(self.harness_editor(disabled, discard_note, &data, window, cx));
        if goose && goose_provider.is_none() {
            ai = ai.child(note(
                "This agent has a saved GOOSE_PROVIDER override whose value is hidden. Replace \
                 or remove it under Advanced → Environment to enter the matching API key here.",
                &c,
            ));
        }
        if let Some(key) = self.api_key(&data) {
            ai = ai.child(self.api_key_field(key, disabled, &data, cx));
        }
        if let Some(preset) = &preset {
            let mut block = div()
                .flex()
                .flex_col()
                .gap(rems(0.75))
                .text_body_sm()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .text_color(c.text_subtle)
                        .child(format!(
                            "{} uses its own default model and sign-in. ",
                            preset.label
                        ))
                        .child(setup_hint(&preset.setup_hint, &c)),
                );
            if !self.draft.model.is_empty() || !self.draft.provider.is_empty() {
                let code = |text: String| {
                    div()
                        .px(px(4.))
                        .rounded(px(4.))
                        .bg(c.affordance_subtle)
                        .text_mono()
                        .child(text)
                };
                block = block
                    .child(div().text_color(c.text_danger).child(format!(
                        "This agent has model or provider settings that Buzz cannot apply to {} \
                         yet. Use {} defaults before saving or starting.",
                        preset.label, preset.label
                    )))
                    .when(!self.draft.model.is_empty(), |this| {
                        this.child(
                            div()
                                .flex()
                                .gap(rems(0.25))
                                .child("Current model:")
                                .child(code(self.draft.model.clone())),
                        )
                    })
                    .when(!self.draft.provider.is_empty(), |this| {
                        this.child(
                            div()
                                .flex()
                                .gap(rems(0.25))
                                .child("Current provider:")
                                .child(code(self.draft.provider.clone())),
                        )
                    })
                    .child(
                        div().flex().child(
                            Button::new(
                                "use-preset-defaults",
                                format!("Use {} defaults", preset.label),
                            )
                            .surface(Surface::Floating)
                            .disabled(disabled)
                            .on_click(cx.listener(
                                |this: &mut T, _, window, cx| {
                                    let fields = this.fields();
                                    fields.patch(
                                        |draft| {
                                            draft.model.clear();
                                            draft.provider.clear();
                                        },
                                        window,
                                        cx,
                                    );
                                    cx.notify();
                                },
                            )),
                        ),
                    );
            }
            ai = ai.child(block);
        } else {
            ai = ai.child(self.model_picker(disabled, &data, window, cx));
        }
        ai = ai.child(self.session_field(disabled, &data, window, cx));
        if pi {
            ai = ai.child(note(
                "Providers and models load from your local Pi configuration, including \
                 extensions. To use a provider that isn’t signed in, choose it and add its API \
                 key. Save restarts a running agent to apply changes.",
                &c,
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(rems(2.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .child(field(
                        "Name",
                        None,
                        TextField::new(&self.name)
                            .surface(Surface::Floating)
                            .disabled(disabled),
                        &c,
                    ))
                    .child(field(
                        "Agent instructions",
                        None,
                        TextArea::new(&self.instructions)
                            .rows(6)
                            .surface(Surface::Floating)
                            .disabled(disabled),
                        &c,
                    )),
            )
            .child(ai)
            .when(
                data.agent_defaults
                    .as_ref()
                    .is_some_and(|defaults| defaults.owner_only),
                |this| {
                    this.child(note(
                        "This build allows instructions only from the owner and verified \
                         same-owner agents.",
                        &c,
                    ))
                },
            )
            .child(self.environment(disabled, preset.as_ref(), pi, cx))
    }

    /// `AgentHarnessEditor`.
    fn harness_editor<T: Owner>(
        &self,
        disabled: bool,
        discard_note: bool,
        data: &AgentsSnapshot,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let options = self.options(data);
        let preset = buzz_host::harness::preset(&self.draft.command);
        let harness = draft::option(&options, &self.draft.command)
            .cloned()
            .or_else(|| {
                preset.as_ref().and_then(|_| {
                    options
                        .iter()
                        .find(|o| draft::kind(&o.command) == draft::kind(&self.draft.command))
                        .cloned()
                })
            });
        let external = self.external(data);
        let discovered = self.discovered(data);
        let pi_loading = discovered && self.pi_providers.is_none();
        let missing_pi = options
            .iter()
            .any(|o| draft::kind(&o.command).as_deref() == Some("pi") && !o.available);
        let missing_preset = preset.is_some() && harness.as_ref().is_none_or(|h| !h.available);
        let mut items: Vec<(String, bool, bool, Pick<T>)> = Vec::new();
        let custom = self.harness_custom;
        let exact = options.iter().any(|o| o.command == self.draft.command);
        for option in &options {
            let label = if option.available {
                option.label.clone()
            } else {
                format!("{} (install first)", option.label)
            };
            let command = option.command.clone();
            let default_args = option.default_args.clone();
            let first_provider = option
                .providers
                .first()
                .map(|p| p.value.clone())
                .unwrap_or_default();
            let entering_external = buzz_host::harness::preset(&option.command).is_some()
                || match &option.configuration_policy {
                    Some(policy) => policy.authentication == "harnessWithOverrides",
                    None => matches!(
                        draft::kind(&option.command).as_deref(),
                        Some("goose" | "pi")
                    ),
                };
            let reset = entering_external || external || preset.is_some();
            items.push((
                label,
                !custom && option.command == self.draft.command,
                !option.available,
                Rc::new(move |this: &mut T, window, cx| {
                    let fields = this.fields();
                    fields.harness_custom = false;
                    let command = command.clone();
                    let args = serde_json::to_string(&default_args).unwrap_or_else(|_| "[]".into());
                    let provider = if entering_external {
                        String::new()
                    } else {
                        first_provider.clone()
                    };
                    fields.patch(
                        |draft| {
                            draft.command = command;
                            if reset {
                                draft.args = args;
                                draft.provider = provider;
                                draft.model.clear();
                            }
                        },
                        window,
                        cx,
                    );
                    let (args, provider, command) = (
                        fields.draft.args.clone(),
                        fields.draft.provider.clone(),
                        fields.draft.command.clone(),
                    );
                    fields
                        .args
                        .update(cx, |state, cx| state.set_value(args, window, cx));
                    fields
                        .custom_provider
                        .update(cx, |state, cx| state.set_value(provider, window, cx));
                    fields
                        .executable
                        .update(cx, |state, cx| state.set_value(command, window, cx));
                    let data = fields.data(cx);
                    fields.provider_custom = fields.provider_needs_input(&data);
                }),
            ));
        }
        if preset.is_some() && !exact {
            let label = format!(
                "{} (current executable)",
                harness
                    .as_ref()
                    .map(|h| h.label.clone())
                    .or(preset.as_ref().map(|p| p.label.clone()))
                    .unwrap_or_default()
            );
            items.push((
                label,
                !custom,
                false,
                Rc::new(|this: &mut T, _, _| this.fields().harness_custom = false),
            ));
        }
        items.push((
            "Custom executable / current value".into(),
            custom || (!exact && preset.is_none()),
            false,
            Rc::new(|this: &mut T, _, _| this.fields().harness_custom = true),
        ));
        let show_input = custom || (!exact && preset.is_none());
        let value = items
            .iter()
            .find(|(_, checked, _, _)| *checked)
            .map(|(label, ..)| label.clone())
            .unwrap_or_default();
        let mut editor = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(field(
                "Harness",
                None,
                self.select(
                    Open::Harness,
                    "harness-select",
                    value,
                    disabled,
                    items,
                    window,
                    cx,
                ),
                &c,
            ))
            .when(show_input, |this| {
                this.child(field(
                    "Executable",
                    None,
                    TextField::new(&self.executable)
                        .surface(Surface::Floating)
                        .disabled(disabled),
                    &c,
                ))
            });
        if missing_pi {
            editor = editor.child(note(
                "Pi needs its CLI, Node.js and buzz-pi-acp before you can select it.",
                &c,
            ));
        }
        if let (true, Some(preset)) = (missing_preset, &preset) {
            editor = editor.child(note(
                format!(
                    "{} needs its ACP launcher. Set it up under Settings → Agents → Harnesses.",
                    preset.label
                ),
                &c,
            ));
        }
        if missing_pi || missing_preset {
            editor = editor.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(rems(0.25))
                    .child(
                        Button::new("open-harnesses", "Open Harnesses in Settings")
                            .variant(ButtonVariant::Link)
                            .disabled(disabled)
                            .on_click(cx.listener(|this: &mut T, _, window, cx| {
                                this.open_harnesses(window, cx)
                            })),
                    )
                    .when(discard_note, |this| {
                        this.child(note("Opening Settings discards unsaved edits.", &c))
                    }),
            );
        }
        if preset.is_none() {
            let default_provider = self.default_provider(data);
            let mut provider_items: Vec<(String, bool, bool, Pick<T>)> = Vec::new();
            let pcustom = self.provider_custom;
            let first = match &default_provider {
                Some(provider) => format!("Use agent defaults ({provider})"),
                None => "Not set".into(),
            };
            let mut choices = vec![(String::new(), first)];
            choices.extend(self.provider_choices(data));
            let goose = draft::is_goose(&self.draft.command);
            for (value, label) in choices {
                let checked = !pcustom && value == self.draft.provider;
                let pick = value.clone();
                provider_items.push((
                    label,
                    checked,
                    false,
                    Rc::new(move |this: &mut T, window, cx| {
                        let fields = this.fields();
                        fields.provider_custom = false;
                        let provider = pick.clone();
                        fields.patch(
                            |draft| {
                                draft.provider = provider;
                                if external {
                                    draft.model.clear();
                                }
                            },
                            window,
                            cx,
                        );
                        let provider = fields.draft.provider.clone();
                        fields
                            .custom_provider
                            .update(cx, |state, cx| state.set_value(provider, window, cx));
                        // Choosing a Goose provider loads its catalog.
                        if goose && !fields.draft.provider.is_empty() {
                            fields.key = fields.context_key();
                            fields.catalog = None;
                            fields.lookup = None;
                            run(fields, ModelAction::Connect, cx);
                        }
                    }),
                ));
            }
            let in_list = provider_items.iter().any(|(_, checked, ..)| *checked);
            provider_items.push((
                "Custom provider / current value".into(),
                pcustom || !in_list,
                false,
                Rc::new(|this: &mut T, _, _| this.fields().provider_custom = true),
            ));
            let value = provider_items
                .iter()
                .find(|(_, checked, ..)| *checked)
                .map(|(label, ..)| label.clone())
                .unwrap_or_default();
            editor = editor
                .child(field(
                    if external { "LLM Provider" } else { "Provider" },
                    None,
                    self.select(
                        Open::Provider,
                        "provider-select",
                        value,
                        disabled || pi_loading,
                        provider_items,
                        window,
                        cx,
                    ),
                    &c,
                ))
                .when(pcustom || !in_list, |this| {
                    this.child(field(
                        "Custom provider",
                        None,
                        TextField::new(&self.custom_provider)
                            .surface(Surface::Floating)
                            .disabled(disabled || pi_loading),
                        &c,
                    ))
                })
                .when(pi_loading, |this| {
                    this.child(note("Loading signed-in providers…", &c))
                });
        }
        editor
    }

    /// `ProviderApiKeyField` and its helper.
    fn api_key_field<T: Owner>(
        &self,
        key: ApiKey,
        disabled: bool,
        _data: &AgentsSnapshot,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let typed = !self.api_key.read(cx).value().is_empty();
        let revealed = self.revealed && typed;
        let toggle = typed.then(|| {
            IconButton::new(
                "reveal-api-key",
                if revealed {
                    IconName::EyeSlash
                } else {
                    IconName::Eye
                },
            )
            .size(IconButtonSize::Sm)
            .tooltip(if revealed {
                "Hide API key"
            } else {
                "Show API key"
            })
            .disabled(disabled)
            .on_click(cx.listener(|this: &mut T, _, window, cx| {
                let fields = this.fields();
                fields.revealed = !fields.revealed;
                fields
                    .api_key
                    .update(cx, |state, cx| state.toggle_masked(window, cx));
                cx.notify();
            }))
        });
        let mut input = TextField::new(&self.api_key)
            .surface(Surface::Floating)
            .disabled(disabled);
        if let Some(toggle) = toggle {
            input = input.trailing(toggle);
        }
        let pi = draft::is_pi(&self.draft.command);
        let helper = if self.buzz_agent() {
            format!(
                "{} is required for OpenAI. Leave blank to keep a saved key, if present, or use \
                 one from Agent defaults. Keys exported in your shell profile are not used. Saved \
                 keys are stored in this device’s local agent settings files.",
                key.env
            )
        } else {
            format!(
                "{} is used for this agent and model lookup. Leave blank to keep a saved key, if \
                 present, or use {}. Keys exported in your shell profile are not used. Saved keys \
                 are stored in this device’s local agent settings files.",
                key.env,
                if pi {
                    "your Pi sign-in"
                } else {
                    "Goose credentials"
                }
            )
        };
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .child(field(format!("{} API key", key.label), None, input, &c))
            .child(note(helper, &c))
    }

    /// `AgentModelPicker`.
    fn model_picker<T: Owner>(
        &self,
        disabled: bool,
        data: &AgentsSnapshot,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let goose = draft::is_goose(&self.draft.command);
        let pi = self.discovered(data);
        let external = self.external(data);
        let fresh = self.fresh().cloned();
        let entries = self.entries();
        let selected_id = self.selected_id();
        let mut section = div().flex().flex_col().gap(rems(0.75));

        if external && !self.draft.provider.is_empty() {
            let result = self.test.as_ref().map(|(_, result)| result.clone());
            let testing = matches!(result, Some(TestResult::Testing));
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(rems(0.5))
                    .child(
                        Button::new("test-connection", "Test connection")
                            .surface(Surface::Floating)
                            .loading(testing)
                            .disabled(disabled)
                            .on_click(cx.listener(|this: &mut T, _, _, cx| {
                                test(this.fields(), cx);
                                cx.notify();
                            })),
                    )
                    .children(result.map(|result| {
                        let (icon, color, text) = match result {
                            TestResult::Testing => (
                                None,
                                c.text_subtle,
                                "Sending a short test message…".to_string(),
                            ),
                            TestResult::Passed(model) => (
                                Some(IconName::CheckCircle),
                                c.text_success,
                                match model {
                                    Some(model) => format!("Connected using {model}."),
                                    None => "Connected. The model replied.".into(),
                                },
                            ),
                            TestResult::Failed(error) => {
                                (Some(IconName::WarningCircle), c.text_danger, error)
                            }
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.5))
                            .text_body_sm()
                            .text_color(color)
                            .children(icon.map(|icon| Icon::new(icon).size(16.).color(color)))
                            .child(text)
                    })),
            );
        }

        // The combobox: input, browse trigger, and the list while open.
        let open = self.open == Some(Open::Model);
        let trailing = if self.busy {
            Spinner::new().size(16.).into_any_element()
        } else {
            IconButton::new("browse-models", IconName::CaretDown)
                .size(IconButtonSize::Xs)
                .tooltip("Browse models")
                .disabled(disabled)
                .on_click(cx.listener(|this: &mut T, _, _, cx| {
                    let fields = this.fields();
                    let opening = fields.open != Some(Open::Model);
                    fields.open = opening.then_some(Open::Model);
                    if opening
                        && fields.fresh().is_none()
                        && fields.attempted.as_deref() != Some(fields.key.as_str())
                    {
                        run(fields, ModelAction::Connect, cx);
                    }
                    cx.notify();
                }))
                .into_any_element()
        };
        let mut combobox = div().relative().w_full().child(
            TextField::new(&self.model_query)
                .surface(Surface::Floating)
                .disabled(disabled)
                .trailing(trailing),
        );
        if open {
            combobox = combobox.child(overlay::dropdown(
                window,
                Align::Start,
                div().min_w(rems(24.)).child(self.model_menu(
                    goose,
                    pi,
                    &entries,
                    &selected_id,
                    cx,
                )),
                Self::dismiss(cx),
            ));
        }
        section = section.child(field("Model", None, combobox, &c));
        if goose && fresh.is_some() && entries.len() > VISIBLE_MODEL_LIMIT {
            section = section.child(note(
                format!(
                    "Showing up to {VISIBLE_MODEL_LIMIT} models. Type to search all {}.",
                    entries.len()
                ),
                &c,
            ));
        }
        if !self.status.is_empty() {
            section = section.child(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .text_body_sm()
                    .text_color(if self.busy && goose {
                        c.text_standard
                    } else {
                        c.text_subtle
                    })
                    .when(self.busy && goose, |this| {
                        this.child(Spinner::new().size(16.))
                    })
                    .child(self.status.clone()),
            );
        }
        if self.busy {
            section = section.child(
                div().flex().child(
                    Button::new(
                        "cancel-models",
                        if external {
                            "Cancel model lookup"
                        } else {
                            "Cancel sign-in"
                        },
                    )
                    .surface(Surface::Floating)
                    .disabled(disabled)
                    .on_click(cx.listener(|this: &mut T, _, _, cx| {
                        cancel(this.fields(), cx);
                        cx.notify();
                    })),
                ),
            );
        } else if !self.status.is_empty() {
            section = section.child(
                div().flex().child(
                    Button::new("retry-models", "Retry models")
                        .surface(Surface::Floating)
                        .disabled(disabled)
                        .on_click(cx.listener(|this: &mut T, _, _, cx| {
                            run(this.fields(), ModelAction::Connect, cx);
                            cx.notify();
                        })),
                ),
            );
        }
        let with_provider = match self.policy(data) {
            Some(policy) => policy.model == "withProvider",
            None => pi,
        };
        let warning = |text: String| div().text_body_sm().text_color(c.text_warning).child(text);
        let pi_no_models = "No Pi models for this provider. Buzz doesn’t use API keys exported in \
                            your shell profile. Add this provider’s API key for this agent, then \
                            browse models again.";
        if with_provider && !self.draft.provider.is_empty() && self.draft.model.is_empty() {
            section = section.child(warning(
                "Choose a model for this provider before starting, or clear Provider to use Pi \
                 defaults."
                    .into(),
            ));
        }
        if pi && fresh.is_some() && entries.is_empty() && !self.draft.provider.is_empty() {
            section = section.child(note(pi_no_models, &c));
        }
        if pi
            && fresh.is_some()
            && !self.draft.model.is_empty()
            && !entries.iter().any(|model| model.id == selected_id)
        {
            section = section.child(warning(
                "This model ID is not in Pi’s available catalog. Select a listed model or confirm \
                 the exact custom ID before starting; Pi may accept an invalid ID until the first \
                 message."
                    .into(),
            ));
        }
        if fresh
            .as_ref()
            .is_some_and(|catalog| catalog.model_overridden)
        {
            section = section.child(warning(format!(
                "{} environment override takes precedence. Change it in Advanced → Environment \
                 to use this selection.",
                if goose {
                    "A GOOSE_MODEL"
                } else {
                    "A saved BUZZ_AGENT_MODEL"
                }
            )));
        }
        if goose
            && fresh.is_some()
            && !self.draft.model.is_empty()
            && !entries.iter().any(|model| model.id == self.draft.model)
        {
            section = section.child(warning(
                "This model ID is not in Goose’s current provider list. Select a listed model or \
                 confirm the custom ID before starting."
                    .into(),
            ));
        }
        if goose {
            section = section.child(note(
                "Models load for the selected provider using credentials entered above or \
                 already configured in Goose. You can also enter a custom model ID.",
                &c,
            ));
        }

        // Advanced → Model.
        let advanced_open = self.advanced_open;
        let mut advanced = div().flex().flex_col().gap(rems(0.75)).child(accordion(
            "advanced-model",
            "Model",
            advanced_open,
            &c,
            cx,
            |fields| fields.advanced_open = !fields.advanced_open,
        ));
        if advanced_open {
            let refresh = |id: &'static str| {
                Button::new(id, "Refresh models")
                    .surface(Surface::Floating)
                    .disabled(disabled || self.busy)
                    .on_click(cx.listener(|this: &mut T, _, _, cx| {
                        run(this.fields(), ModelAction::Refresh, cx);
                        cx.notify();
                    }))
            };
            advanced = advanced
                .child(field(
                    "Model ID (custom or blank)",
                    None,
                    TextField::new(&self.model_id)
                        .surface(Surface::Floating)
                        .disabled(disabled),
                    &c,
                ))
                .when(pi, |this| {
                    this.child(note(
                        "This field uses the exact model ID, including any namespace slashes, \
                         without adding the provider. Its text is saved literally.",
                        &c,
                    ))
                    .child(div().flex().child(refresh("refresh-pi-models")))
                });
            if !external {
                advanced = advanced
                    .child(field(
                        "Databricks workspace (HTTPS origin)",
                        None,
                        TextField::new(&self.databricks_host)
                            .surface(Surface::Floating)
                            .disabled(disabled),
                        &c,
                    ))
                    .child(field(
                        "Model filter (optional)",
                        None,
                        TextField::new(&self.databricks_filter)
                            .surface(Surface::Floating)
                            .disabled(disabled),
                        &c,
                    ))
                    .child(note(
                        "Editing either field saves both displayed values.",
                        &c,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(rems(0.5))
                            .child(refresh("refresh-databricks-models"))
                            .child(
                                Button::new("databricks-disconnect", "Disconnect")
                                    .surface(Surface::Floating)
                                    .disabled(disabled || self.busy)
                                    .on_click(cx.listener(|this: &mut T, _, _, cx| {
                                        run(this.fields(), ModelAction::Disconnect, cx);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(note(
                        "Credentials are shared within Foundation for this workspace, not with \
                         old Buzz. Disconnect removes this app’s cache, not your browser session.",
                        &c,
                    ));
            }
        }
        section
            .child(div().mt(rems(1.)).text_label().child("Advanced"))
            .child(advanced)
    }

    fn model_menu<T: Owner>(
        &self,
        goose: bool,
        pi: bool,
        entries: &[ModelOption],
        selected_id: &str,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let mut items: Vec<ModelOption> = entries.to_vec();
        let selected = entries
            .iter()
            .find(|model| model.id == selected_id)
            .cloned()
            .or_else(|| {
                (!self.draft.model.is_empty()).then(|| ModelOption {
                    id: self.draft.model.clone(),
                    name: self.draft.model.clone(),
                })
            });
        if let Some(selected) = &selected
            && !entries.iter().any(|model| model.id == selected.id)
        {
            items.insert(0, selected.clone());
        }
        if let Some(custom) = self
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            && !items
                .iter()
                .any(|model| model.id == custom || model.name == custom)
        {
            items.push(ModelOption {
                id: custom.into(),
                name: custom.into(),
            });
        }
        let matching: Vec<ModelOption> = match &self.query {
            None => items,
            Some(query) => {
                let query = query.to_lowercase();
                items
                    .into_iter()
                    .filter(|item| {
                        format!("{} {}", item.name, item.id)
                            .to_lowercase()
                            .contains(&query)
                    })
                    .collect()
            }
        };
        let shown: Vec<ModelOption> = if goose && self.busy {
            Vec::new()
        } else if goose {
            matching.into_iter().take(VISIBLE_MODEL_LIMIT).collect()
        } else {
            matching
        };
        let mut list = div()
            .id("model-options")
            .flex()
            .flex_col()
            .max_h(rems(20.))
            .overflow_y_scroll();
        if self.busy {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .px(rems(0.75))
                    .py(rems(0.5))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(Spinner::new().size(16.))
                    .child(if pi {
                        "Loading Pi models…"
                    } else {
                        "Loading models…"
                    }),
            );
        }
        let pi_no_models =
            self.fresh().is_some() && !self.draft.provider.is_empty() && entries.is_empty();
        if pi && !self.busy && (!self.status.is_empty() || pi_no_models) {
            list = list.child(
                div()
                    .px(rems(0.75))
                    .py(rems(0.5))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(if pi_no_models {
                        "No Pi models for this provider. Buzz doesn’t use API keys exported in \
                         your shell profile. Add this provider’s API key for this agent, then \
                         browse models again."
                            .to_string()
                    } else {
                        self.status.clone()
                    }),
            );
        }
        if shown.is_empty() && !self.busy {
            list = list.child(
                MenuItem::new("models-empty", "Type a model ID to use a custom model.")
                    .disabled(true),
            );
        }
        for (index, model) in shown.into_iter().enumerate() {
            let custom = !entries.iter().any(|entry| entry.id == model.id);
            let description = if goose && model.name == model.id {
                custom.then(|| "Custom ID".to_string())
            } else {
                Some(format!(
                    "{}{}",
                    model.id,
                    if custom { " · Custom ID" } else { "" }
                ))
            };
            let id = model.id.clone();
            list = list.child(
                MenuItem::new(("model", index), model.name.clone())
                    .checked(model.id == selected_id)
                    .when_some(description, |this, description| {
                        this.trailing(div().text_caption().child(description))
                    })
                    .on_click(cx.listener(move |this: &mut T, _, window, cx| {
                        let fields = this.fields();
                        fields.choose_model(&id, window, cx);
                        fields.open = None;
                        cx.notify();
                    })),
            );
        }
        div().child(Menu::new().size(MenuSize::Wide).child(list))
    }

    fn session_field<T: Owner>(
        &self,
        disabled: bool,
        data: &AgentsSnapshot,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let label = |policy: SessionPolicy| match policy {
            SessionPolicy::Channel => "Entire channel",
            SessionPolicy::Thread => "Each thread",
        };
        let inherited = data
            .default_settings
            .as_ref()
            .and_then(|defaults| defaults.session_policy)
            .unwrap_or(SessionPolicy::Thread);
        let choices = [
            (None, format!("Use agent defaults ({})", label(inherited))),
            (Some(SessionPolicy::Channel), "Entire channel".to_string()),
            (Some(SessionPolicy::Thread), "Each thread".to_string()),
        ];
        let value = choices
            .iter()
            .find(|(policy, _)| *policy == self.draft.session_policy)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let items: Vec<(String, bool, bool, Pick<T>)> = choices
            .into_iter()
            .map(|(policy, label)| {
                let pick: Pick<T> = Rc::new(move |this: &mut T, window, cx| {
                    this.fields()
                        .patch(|draft| draft.session_policy = policy, window, cx)
                });
                (label, policy == self.draft.session_policy, false, pick)
            })
            .collect();
        field(
            "Conversation context",
            Some(
                "Entire channel shares one conversation across threads. Each thread keeps a \
                 separate conversation; direct messages remain shared."
                    .into(),
            ),
            self.select(
                Open::Session,
                "session-select",
                value,
                disabled,
                items,
                window,
                cx,
            ),
            &c,
        )
    }

    /// The Environment accordion: workspace, arguments and overrides.
    fn environment<T: Owner>(
        &self,
        disabled: bool,
        preset: Option<&buzz_host::harness::Preset>,
        pi: bool,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let open = self.environment_open;
        let mut section = div().flex().flex_col().gap(rems(1.)).child(accordion(
            "environment-toggle",
            "Environment",
            open,
            &c,
            cx,
            |fields| fields.environment_open = !fields.environment_open,
        ));
        if !open {
            return section;
        }
        let mut names: Vec<String> = self.environment_keys.clone();
        for key in self.draft.environment.keys() {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
        names.sort();
        let mut rows = div().flex().flex_col().gap(rems(0.75));
        for (index, key) in names.into_iter().enumerate() {
            let Some(state) = self.env_inputs.get(&key) else {
                continue;
            };
            let changed = self.draft.environment.contains_key(&key);
            let removed = matches!(self.draft.environment.get(&key), Some(None));
            let value = self
                .draft
                .environment
                .get(&key)
                .cloned()
                .flatten()
                .unwrap_or_default();
            let (remove_key, undo_key) = (key.clone(), key.clone());
            rows = rows.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_end()
                    .gap(rems(0.5))
                    .child(
                        div()
                            .flex_1()
                            .min_w(rems(12.))
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .child(div().text_mono().child(key.clone()))
                            .child(
                                TextField::new(state)
                                    .surface(Surface::Floating)
                                    .disabled(disabled || removed),
                            ),
                    )
                    .when(!removed, |this| {
                        this.child(
                            Button::new(("env-remove", index), "Remove")
                                .surface(Surface::Floating)
                                .disabled(disabled)
                                .on_click(cx.listener(move |this: &mut T, _, window, cx| {
                                    let fields = this.fields();
                                    fields.draft.environment.insert(remove_key.clone(), None);
                                    fields.dirty = true;
                                    if let Some(state) = fields.env_inputs.get(&remove_key) {
                                        state.update(cx, |state, cx| {
                                            state.set_value("", window, cx)
                                        });
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .when(changed, |this| {
                        this.child(
                            Button::new(("env-undo", index), "Undo")
                                .surface(Surface::Floating)
                                .disabled(disabled)
                                .on_click(cx.listener(move |this: &mut T, _, window, cx| {
                                    let fields = this.fields();
                                    fields.draft.environment.remove(&undo_key);
                                    if let Some(state) = fields.env_inputs.get(&undo_key) {
                                        state.update(cx, |state, cx| {
                                            state.set_value("", window, cx)
                                        });
                                    }
                                    // A new variable leaves the list with its undo.
                                    if !fields.environment_keys.contains(&undo_key) {
                                        fields.env_inputs.remove(&undo_key);
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .when(changed && !removed, |this| {
                        this.child(note(
                            if value.is_empty() {
                                "Will save an empty value"
                            } else {
                                "Will replace on save"
                            },
                            &c,
                        ))
                    }),
            );
        }
        let footer = if pi {
            "Pi needs both Provider and Model to override its defaults. Advanced Pi options \
             follow --; for example: [\"--\", \"--extension\", \"/absolute/path/to/extension.ts\"]. \
             PI_CODING_AGENT_DIR can select a local Pi configuration directory."
                .to_string()
        } else if let Some(preset) = preset {
            format!(
                "Configure {}'s model and sign-in in the harness itself. Buzz model and provider \
                 overrides are unavailable.",
                preset.label
            )
        } else {
            "Environment overrides take precedence over provider and model selections.".into()
        };
        section =
            section
                .child(field(
                    "Workspace",
                    None,
                    TextField::new(&self.workspace)
                        .surface(Surface::Floating)
                        .disabled(disabled),
                    &c,
                ))
                .child(field(
                    "Arguments (JSON array)",
                    None,
                    TextArea::new(&self.args)
                        .rows(3)
                        .code()
                        .surface(Surface::Floating)
                        .disabled(disabled),
                    &c,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(1.))
                        .child(legend("Environment overrides"))
                        .child(note(
                            "Saved values stay on the host. Leave unchanged to preserve them; \
                         replacements are write-only. Remove takes effect only when you save. Do \
                         not put secrets in arguments or prompts.",
                            &c,
                        ))
                        .child(rows)
                        .child(field(
                            "Variable name",
                            None,
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(rems(0.5))
                                .child(
                                    div().flex_1().min_w(rems(10.)).child(
                                        TextField::new(&self.new_key)
                                            .surface(Surface::Floating)
                                            .invalid(self.env_error.is_some())
                                            .disabled(disabled),
                                    ),
                                )
                                .child(
                                    Button::new("env-add", "Add variable")
                                        .surface(Surface::Floating)
                                        .disabled(disabled)
                                        .on_click(cx.listener(|this: &mut T, _, window, cx| {
                                            this.fields().add_variable(window, cx);
                                            cx.notify();
                                        })),
                                ),
                            &c,
                        ))
                        .children(self.env_error.clone().map(|error| {
                            div().text_body_sm().text_color(c.text_danger).child(error)
                        })),
                )
                .child(note(
                    format!("{footer} Arguments are passed literally, not through a shell."),
                    &c,
                ));
        let _ = ButtonSize::Sm;
        section
    }
}
