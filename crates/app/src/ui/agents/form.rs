//! Fields shared by the agent dialogs (ui.md §5a.7, §5a.8) and the agent
//! defaults dialog (§5a.11): the AI configuration and the environment
//! overrides.
//!
//! Environment values and API keys are write-only: the host lists saved
//! keys, never values, so a field left empty keeps what is saved.

use super::harness;
use crate::hosts::HostConnection;
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_host::api::{
    AgentDefaults, DatabricksSettings, Harness, HarnessKind, ModelOption, Request, Response,
    SessionPolicy,
};
use buzz_host::harness::{Provider, defaults_name, efforts, providers};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, Colors, Icon, IconButton, IconButtonSize, IconName, Menu,
    MenuItem, MenuSize, Select, Spinner, Surface, TextField, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::rc::Rc;

/// `Field`: label, control, then the description (§6.3).
pub fn field(
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    c: &Colors,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(0.5))
        .min_w_0()
        .child(div().text_label_sm().child(label.into()))
        .child(control)
        .children(
            description.map(|text| div().text_body_sm().text_color(c.text_subtle).child(text)),
        )
}

/// A `<fieldset>` legend in `text-label`.
pub fn legend(text: &'static str) -> Div {
    div().text_label().child(text)
}

pub fn note(text: impl Into<SharedString>, c: &Colors) -> Div {
    div()
        .text_body_sm()
        .text_color(c.text_subtle)
        .child(text.into())
}

/// The form's selects; one is open at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picker {
    Host,
    Community,
    Harness,
    Provider,
    Model,
    Effort,
    Session,
}

/// A dialog holding [`AiFields`] and [`EnvFields`].
pub trait Form: Sized + 'static {
    fn open(&mut self) -> &mut Option<Picker>;
    fn ai(&mut self) -> &mut AiFields;
    fn env(&mut self) -> &mut EnvFields;
    /// The host and the `Models` request for the current draft.
    fn models_request(&self, cx: &App) -> Result<(Entity<HostConnection>, Request), String>;
}

/// Escape closes an open select before the dialog: the form captures the
/// dialog's `Cancel` on its way down while a menu is open.
pub fn escape<T: Form>(
    cx: &mut Context<T>,
) -> impl Fn(&buzz_ui::Cancel, &mut Window, &mut App) + use<T> {
    let form = cx.entity().downgrade();
    move |_, _, cx| {
        let closed = form
            .update(cx, |this, cx| {
                let open = this.open().take().is_some();
                if open {
                    cx.notify();
                }
                open
            })
            .unwrap_or(false);
        if closed {
            cx.stop_propagation();
        }
    }
}

fn dismiss<T: Form>(cx: &mut Context<T>) -> Dismiss {
    let form = cx.entity().downgrade();
    Rc::new(move |_, cx| {
        let _ = form.update(cx, |this, cx| {
            *this.open() = None;
            cx.notify();
        });
    })
}

/// A select's menu. Long lists scroll inside the menu's surface, which
/// would otherwise cut them off.
#[derive(IntoElement)]
pub struct Options {
    id: &'static str,
    items: Vec<AnyElement>,
}

impl Options {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            items: Vec::new(),
        }
    }

    pub fn child(mut self, item: impl IntoElement) -> Self {
        self.items.push(item.into_any_element());
        self
    }
}

impl RenderOnce for Options {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Menu::new().size(MenuSize::Wide).child(
            div()
                .id(self.id)
                .flex()
                .flex_col()
                .max_h(rems(23.))
                .overflow_y_scroll()
                .children(self.items),
        )
    }
}

/// A select with its menu while open.
pub fn select<T: Form>(
    open: Option<Picker>,
    busy: bool,
    picker: Picker,
    trigger: Select,
    options: impl IntoElement,
    window: &Window,
    cx: &mut Context<T>,
) -> Div {
    let is_open = open == Some(picker);
    div()
        .relative()
        .w_full()
        .child(
            trigger
                .open(is_open)
                .surface(Surface::Floating)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let open = this.open();
                    *open = (*open != Some(picker)).then_some(picker);
                    // Pi lists its signed-in providers through its catalog.
                    if *open == Some(Picker::Provider)
                        && matches!(this.ai().catalog, Catalog::Idle)
                        && matches!(this.models_request(cx), Ok((_, Request::Models { edit, .. }))
                            if harness::kind_of(&edit.harness.command, &[]) == Some(HarnessKind::Pi))
                    {
                        browse(this, cx);
                    }
                    cx.notify();
                })),
        )
        .when(is_open, |this| {
            this.child(overlay::dropdown(
                window,
                Align::Start,
                div().min_w(rems(20.)).child(options),
                dismiss(cx),
            ))
        })
}

fn pick<T: Form>(
    cx: &mut Context<T>,
    f: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |this, _, window, cx| {
        *this.open() = None;
        f(this, window, cx);
        cx.notify();
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Harness(HarnessKind),
    /// "Custom executable": the command is typed in.
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderChoice {
    /// Agent defaults' provider, or the harness's own.
    Inherit,
    Id(String),
    /// Typed in.
    Custom,
}

pub enum Catalog {
    Idle,
    Loading,
    Ready {
        models: Vec<ModelOption>,
        overridden: bool,
        /// Pi lists `provider/model`; choosing one sets both.
        pi: bool,
    },
    Failed(SharedString),
}

/// "Test connection" (Pi and Goose).
pub enum Test {
    Idle,
    Testing,
    /// The model that answered, when the host may say.
    Passed(Option<String>),
    Failed(SharedString),
}

/// What the AI fields render against.
pub struct Scope<'a> {
    pub harnesses: &'a [Harness],
    pub harnesses_loading: bool,
    pub defaults: Option<&'a AgentDefaults>,
    pub open: Option<Picker>,
    pub busy: bool,
}

pub struct AiFields {
    /// Editing the host's agent defaults rather than an agent.
    pub for_defaults: bool,
    /// `None` follows the default harness until one is picked.
    pub choice: Option<Choice>,
    pub executable: Entity<InputState>,
    pub provider: ProviderChoice,
    pub custom_provider: Entity<InputState>,
    pub api_key: Entity<InputState>,
    pub model: Entity<InputState>,
    /// `None`: not set, so the harness (or agent defaults) decides.
    pub effort: Option<String>,
    /// `None`: inherit agent defaults. Always set for defaults.
    pub session: Option<SessionPolicy>,
    pub catalog: Catalog,
    generation: u64,
    /// Environment keys saved on the host, values unknown.
    pub saved_keys: Vec<String>,
    pub test: Test,
    /// Buzz Agent's Databricks workspace and model filter.
    pub databricks_host: Entity<InputState>,
    pub databricks_filter: Entity<InputState>,
    /// What the agent has saved.
    pub databricks_saved: Option<DatabricksSettings>,
    /// The Advanced → Model accordion.
    advanced_open: bool,
    /// "Disconnect": its progress, then its outcome.
    disconnect: Option<Result<SharedString, SharedString>>,
    disconnecting: bool,
    placeholders: (SharedString, SharedString, SharedString),
    _subscriptions: Vec<Subscription>,
}

/// Valid names: letters, digits and underscores, not starting with a digit.
pub fn valid_env_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "Extra high".into(),
        other => other.to_string(),
    }
}

fn provider_label(kind: HarnessKind, id: &str) -> String {
    providers(kind)
        .iter()
        .find(|provider| provider.id == id)
        .map_or_else(|| id.to_string(), |provider| provider.label.to_string())
}

impl AiFields {
    pub fn new<T: Form>(for_defaults: bool, window: &mut Window, cx: &mut Context<T>) -> Self {
        let input = |placeholder: &str, masked: bool, window: &mut Window, cx: &mut Context<T>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(masked)
                    .placeholder(placeholder)
            })
        };
        let executable = input("/path/to/acp-agent", false, window, cx);
        let custom_provider = input("Provider ID", false, window, cx);
        let api_key = input("Paste API key", true, window, cx);
        let model = input("Choose or enter a model", false, window, cx);
        let databricks_host = input("https://workspace.example.com", false, window, cx);
        let databricks_filter = input("", false, window, cx);
        let mut subscriptions =
            vec![cx.subscribe(&executable, |_, _, _: &InputEvent, cx| cx.notify())];
        // A test result is about the draft it tested.
        for state in [&api_key, &model, &databricks_host, &databricks_filter] {
            subscriptions.push(cx.subscribe(state, |this: &mut T, _, _: &InputEvent, cx| {
                let ai = this.ai();
                if !matches!(ai.test, Test::Testing) {
                    ai.test = Test::Idle;
                }
                if !ai.disconnecting {
                    ai.disconnect = None;
                }
                cx.notify()
            }));
        }
        // A different provider has a different catalog.
        subscriptions.push(cx.subscribe(
            &custom_provider,
            |this: &mut T, _, _: &InputEvent, cx| {
                this.ai().invalidate();
                cx.notify();
            },
        ));
        Self {
            for_defaults,
            choice: None,
            executable,
            provider: ProviderChoice::Inherit,
            custom_provider,
            api_key,
            model,
            effort: None,
            session: for_defaults.then_some(SessionPolicy::Thread),
            catalog: Catalog::Idle,
            generation: 0,
            saved_keys: Vec::new(),
            test: Test::Idle,
            databricks_host,
            databricks_filter,
            databricks_saved: None,
            advanced_open: false,
            disconnect: None,
            disconnecting: false,
            placeholders: Default::default(),
            _subscriptions: subscriptions,
        }
    }

    /// A different harness or provider: catalogs and test results are
    /// stale.
    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.catalog = Catalog::Idle;
        self.test = Test::Idle;
    }

    /// The Databricks settings to save: the displayed fields once either
    /// is filled in, else none, which follows the agent defaults.
    pub fn databricks(&self, cx: &App) -> Option<DatabricksSettings> {
        let host = self.databricks_host.read(cx).value().trim().to_string();
        let filter = self.databricks_filter.read(cx).value().trim().to_string();
        if host.is_empty() && filter.is_empty() {
            return None;
        }
        Some(DatabricksSettings { host, filter })
    }

    /// Fills the Databricks fields from saved settings.
    pub fn set_databricks(
        &mut self,
        saved: Option<DatabricksSettings>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (host, filter) = saved
            .clone()
            .map(|saved| (saved.host, saved.filter))
            .unwrap_or_default();
        self.databricks_host
            .update(cx, |state, cx| state.set_value(host, window, cx));
        self.databricks_filter
            .update(cx, |state, cx| state.set_value(filter, window, cx));
        self.databricks_saved = saved;
    }

    /// Agent defaults set the Databricks workspace, and this agent does
    /// not.
    fn databricks_inherited(&self, cx: &Scope) -> bool {
        self.inherited(HarnessKind::BuzzAgent, cx)
            .is_some_and(|defaults| {
                defaults
                    .environment_keys
                    .iter()
                    .any(|key| key == "DATABRICKS_HOST")
            })
            && self.databricks_saved.is_none()
            && !self.saved_keys.iter().any(|key| key == "DATABRICKS_HOST")
    }

    /// The picked harness; else the defaults' harness when it is
    /// installed; else the first installed one.
    pub fn choice(&self, cx: &Scope) -> Choice {
        if let Some(choice) = self.choice {
            return choice;
        }
        let installed = |kind: HarnessKind| {
            cx.harnesses
                .iter()
                .any(|harness| harness.kind == kind && harness.command.is_some())
        };
        let default = cx
            .defaults
            .and_then(|defaults| buzz_host::harness::from_defaults_name(&defaults.harness));
        if self.for_defaults {
            return Choice::Harness(default.unwrap_or(HarnessKind::BuzzAgent));
        }
        if let Some(kind) = default.filter(|kind| installed(*kind)) {
            return Choice::Harness(kind);
        }
        cx.harnesses
            .iter()
            .find(|harness| harness.command.is_some())
            .map_or(Choice::Custom, |harness| Choice::Harness(harness.kind))
    }

    pub fn kind(&self, cx: &Scope) -> Option<HarnessKind> {
        match self.choice(cx) {
            Choice::Harness(kind) => Some(kind),
            Choice::Custom => None,
        }
    }

    /// Agent defaults this agent inherits from: those for the same harness.
    fn inherited<'a>(&self, kind: HarnessKind, cx: &Scope<'a>) -> Option<&'a AgentDefaults> {
        if self.for_defaults {
            return None;
        }
        cx.defaults
            .filter(|defaults| Some(defaults.harness.as_str()) == defaults_name(kind))
    }

    pub fn provider_id(&self, cx: &App) -> String {
        match &self.provider {
            ProviderChoice::Inherit => String::new(),
            ProviderChoice::Id(id) => id.clone(),
            ProviderChoice::Custom => self.custom_provider.read(cx).value().trim().to_string(),
        }
    }

    pub fn model_id(&self, cx: &App) -> String {
        self.model.read(cx).value().trim().to_string()
    }

    /// The provider whose key the form asks for.
    fn key_provider(&self, kind: HarnessKind, cx: &Scope, app: &App) -> Option<Provider> {
        let id = match self.provider_id(app) {
            id if !id.is_empty() => id,
            _ => self.inherited(kind, cx)?.provider.clone(),
        };
        providers(kind)
            .iter()
            .find(|provider| provider.id == id && provider.key.is_some())
            .copied()
    }

    /// The API key typed in, as an environment patch.
    pub fn environment(&self, cx: &Scope, app: &App) -> BTreeMap<String, Option<String>> {
        let value = self.api_key.read(app).value().trim().to_string();
        let key = self
            .kind(cx)
            .and_then(|kind| self.key_provider(kind, cx, app))
            .and_then(|provider| provider.key);
        match key {
            Some(key) if !value.is_empty() => BTreeMap::from([(key.to_string(), Some(value))]),
            _ => BTreeMap::new(),
        }
    }

    /// Fills the provider choice from a saved value.
    pub fn set_provider(
        &mut self,
        kind: Option<HarnessKind>,
        id: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.provider = if id.is_empty() {
            ProviderChoice::Inherit
        } else if kind.is_some_and(|kind| providers(kind).iter().any(|p| p.id == id)) {
            ProviderChoice::Id(id.to_string())
        } else {
            let id = id.to_string();
            self.custom_provider
                .update(cx, |state, cx| state.set_value(id, window, cx));
            ProviderChoice::Custom
        };
    }

    fn pick_harness(&mut self, choice: Choice) {
        if self.choice != Some(choice) {
            self.choice = Some(choice);
            self.provider = ProviderChoice::Inherit;
            self.effort = None;
            self.invalidate();
        }
    }

    /// Keeps placeholders in step with what the form inherits.
    pub fn sync<T>(&mut self, cx: &Scope, window: &mut Window, app: &mut Context<T>) {
        let kind = self.kind(cx);
        let inherited = kind.and_then(|kind| self.inherited(kind, cx));
        let model = match inherited.filter(|defaults| !defaults.model.is_empty()) {
            Some(defaults) => format!("Use agent defaults ({})", defaults.model),
            None => "Choose or enter a model".into(),
        };
        let key = kind.and_then(|kind| self.key_provider(kind, cx, app));
        let api_key = match (kind, key.and_then(|p| p.key)) {
            (_, Some(env)) if self.saved_keys.iter().any(|saved| saved == env) => {
                "Saved key unchanged".to_string()
            }
            (Some(HarnessKind::Goose), _) => {
                "Paste API key or use existing Goose credentials".into()
            }
            (Some(HarnessKind::Pi), _) => "Paste API key or use an existing Pi sign-in".into(),
            _ => "Paste API key".into(),
        };
        let inherited = self.databricks_inherited(cx);
        let workspace = if inherited {
            "Use agent defaults"
        } else {
            "https://workspace.example.com"
        };
        let wanted: (SharedString, SharedString, SharedString) =
            (model.into(), api_key.into(), workspace.into());
        if wanted != self.placeholders {
            let (model, api_key, workspace) = wanted.clone();
            let filter = if inherited { "Use agent defaults" } else { "" };
            self.model
                .update(app, |state, cx| state.set_placeholder(model, window, cx));
            self.api_key
                .update(app, |state, cx| state.set_placeholder(api_key, window, cx));
            self.databricks_host.update(app, |state, cx| {
                state.set_placeholder(workspace, window, cx)
            });
            self.databricks_filter
                .update(app, |state, cx| state.set_placeholder(filter, window, cx));
            self.placeholders = wanted;
        }
    }

    /// Providers Pi reported signed in, from its catalog.
    fn pi_signed_in(&self) -> Vec<String> {
        let Catalog::Ready { models, .. } = &self.catalog else {
            return Vec::new();
        };
        let mut providers: Vec<String> = models
            .iter()
            .filter_map(|model| model.id.split_once('/').map(|(p, _)| p.to_string()))
            .collect();
        providers.dedup();
        providers
    }

    pub fn render<T: Form>(&self, ctx: &Scope, window: &Window, cx: &mut Context<T>) -> Div {
        let c = *cx.colors();
        let choice = self.choice(ctx);
        let kind = self.kind(ctx);
        let preset = kind.is_some_and(harness::is_preset);
        let mut fields = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .when(!self.for_defaults, |this| {
                this.child(legend("AI configuration"))
            })
            .child(self.harness_field(ctx, choice, window, cx));
        if choice == Choice::Custom {
            fields = fields.child(field(
                "Executable",
                Some("An ACP agent command, run under buzz-acp.".into()),
                TextField::new(&self.executable).surface(Surface::Floating),
                &c,
            ));
        }
        if preset {
            let current = ctx.harnesses.iter().find(|h| Some(h.kind) == kind);
            let label = current.map(|h| h.label.clone()).unwrap_or_default();
            let hint = current.map(|h| h.setup_hint.clone()).unwrap_or_default();
            fields = fields.child(note(
                format!("{label} uses its own default model and sign-in. {hint}"),
                &c,
            ));
        } else if let Some(kind) = kind {
            fields = fields.child(self.provider_field(ctx, kind, window, cx));
            if let Some(provider) = self.key_provider(kind, ctx, cx) {
                fields = fields.child(self.api_key_field(provider, &c));
            }
            // buzz-app tests only a provider the draft names itself.
            if !self.for_defaults
                && matches!(kind, HarnessKind::Pi | HarnessKind::Goose)
                && !self.provider_id(cx).is_empty()
            {
                fields = fields.child(self.test_field(ctx, cx));
            }
            fields = fields.child(self.model_field(ctx, Some(kind), window, cx));
            fields = fields.child(self.effort_field(ctx, kind, window, cx));
            if !self.for_defaults && kind == HarnessKind::BuzzAgent {
                fields = fields.child(self.advanced(ctx, cx));
            }
        } else {
            fields = fields.child(self.model_field(ctx, None, window, cx));
        }
        fields.child(self.session_field(ctx, window, cx))
    }

    fn harness_field<T: Form>(
        &self,
        ctx: &Scope,
        choice: Choice,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let current = match choice {
            Choice::Harness(kind) => ctx.harnesses.iter().find(|harness| harness.kind == kind),
            Choice::Custom => None,
        };
        let mut menu = Options::new("options");
        let listed = ctx
            .harnesses
            .iter()
            .filter(|harness| !self.for_defaults || defaults_name(harness.kind).is_some());
        for (index, harness) in listed.enumerate() {
            let ready = harness.command.is_some();
            let label: SharedString = if ready {
                harness.label.clone().into()
            } else {
                format!("{} (install first)", harness.label).into()
            };
            let kind = harness.kind;
            menu = menu.child(
                MenuItem::new(("harness", index), label)
                    .checked(choice == Choice::Harness(kind))
                    // A default may name a harness installed later.
                    .disabled(!ready && !self.for_defaults)
                    .on_click(pick(cx, move |this: &mut T, _, _| {
                        this.ai().pick_harness(Choice::Harness(kind))
                    })),
            );
        }
        if !self.for_defaults {
            menu = menu.child(
                MenuItem::new("harness-custom", "Custom executable")
                    .checked(choice == Choice::Custom)
                    .on_click(pick(cx, |this: &mut T, _, _| {
                        this.ai().pick_harness(Choice::Custom)
                    })),
            );
        }
        let (value, kind) = match (choice, current) {
            (Choice::Harness(kind), Some(harness)) => (harness.label.clone(), Some(kind)),
            (Choice::Harness(kind), None) => (format!("{kind:?}"), Some(kind)),
            (Choice::Custom, _) => ("Custom executable".to_string(), None),
        };
        let trigger = Select::new("harness-select")
            .value(value)
            .when_some(kind, |this, kind| this.leading(harness::mark(kind, 16.)));
        let help: Option<SharedString> = if ctx.harnesses_loading {
            Some("Checking installed harnesses…".into())
        } else {
            current
                .filter(|harness| harness.command.is_none())
                .map(|harness| {
                    format!(
                        "{} needs {} before it can run. {}",
                        harness.label,
                        harness.missing.join(", "),
                        harness.setup_hint
                    )
                    .into()
                })
        };
        field(
            if self.for_defaults {
                "Default harness"
            } else {
                "Harness"
            },
            help,
            select(
                ctx.open,
                ctx.busy,
                Picker::Harness,
                trigger,
                menu,
                window,
                cx,
            ),
            &c,
        )
    }

    fn provider_field<T: Form>(
        &self,
        ctx: &Scope,
        kind: HarnessKind,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let inherit_label = match self.inherited(kind, ctx) {
            _ if self.for_defaults => "Not set (use harness default)".to_string(),
            Some(defaults) if !defaults.provider.is_empty() => format!(
                "Use agent defaults ({})",
                provider_label(kind, &defaults.provider)
            ),
            _ => "Not set".to_string(),
        };
        let mut menu = Options::new("options").child(
            MenuItem::new("provider-inherit", inherit_label.clone())
                .checked(self.provider == ProviderChoice::Inherit)
                .on_click(pick(cx, |this: &mut T, _, _| {
                    this.ai().provider = ProviderChoice::Inherit;
                    this.ai().invalidate();
                })),
        );
        let mut options: Vec<(String, String)> = Vec::new();
        if kind == HarnessKind::Pi {
            let signed_in = self.pi_signed_in();
            for id in &signed_in {
                options.push((id.clone(), provider_label(kind, id)));
            }
            for provider in providers(kind) {
                if !signed_in.iter().any(|id| id == provider.id) {
                    options.push((
                        provider.id.to_string(),
                        format!("{} (API key needed)", provider.label),
                    ));
                }
            }
        } else {
            options.extend(
                providers(kind)
                    .iter()
                    .map(|p| (p.id.to_string(), p.label.to_string())),
            );
        }
        for (index, (id, label)) in options.iter().enumerate() {
            let pick_id = id.clone();
            menu = menu.child(
                MenuItem::new(("provider", index), label.clone())
                    .checked(self.provider == ProviderChoice::Id(id.clone()))
                    .on_click(pick(cx, move |this: &mut T, _, _| {
                        let ai = this.ai();
                        if ai.provider != ProviderChoice::Id(pick_id.clone()) {
                            ai.provider = ProviderChoice::Id(pick_id.clone());
                            if kind != HarnessKind::Pi {
                                ai.invalidate();
                            }
                        }
                    })),
            );
        }
        menu = menu.child(
            MenuItem::new("provider-custom", "Custom provider")
                .checked(self.provider == ProviderChoice::Custom)
                .on_click(pick(cx, |this: &mut T, _, _| {
                    this.ai().provider = ProviderChoice::Custom;
                    this.ai().invalidate();
                })),
        );
        let value = match &self.provider {
            ProviderChoice::Inherit => inherit_label,
            ProviderChoice::Id(id) => options
                .iter()
                .find(|(option, _)| option == id)
                .map_or_else(|| id.clone(), |(_, label)| label.clone()),
            ProviderChoice::Custom => "Custom provider".into(),
        };
        let label = match (self.for_defaults, kind) {
            (true, _) => "Default provider",
            (false, HarnessKind::BuzzAgent) => "Provider",
            _ => "LLM Provider",
        };
        let help: Option<SharedString> = (kind == HarnessKind::Pi
            && matches!(self.catalog, Catalog::Loading))
        .then(|| "Loading signed-in providers…".into());
        let mut column = div().flex().flex_col().gap(rems(1.)).child(field(
            label,
            help,
            select(
                ctx.open,
                ctx.busy,
                Picker::Provider,
                Select::new("provider-select").value(value),
                menu,
                window,
                cx,
            ),
            &c,
        ));
        if self.provider == ProviderChoice::Custom {
            column = column.child(field(
                "Custom provider",
                None,
                TextField::new(&self.custom_provider).surface(Surface::Floating),
                &c,
            ));
        }
        column
    }

    /// "Test connection" and its result, above the model.
    fn test_field<T: Form>(&self, ctx: &Scope, cx: &mut Context<T>) -> Div {
        let c = *cx.colors();
        let testing = matches!(self.test, Test::Testing);
        let status = match &self.test {
            Test::Idle => None,
            Test::Testing => Some((
                None,
                c.text_subtle,
                "Sending a short test message…".to_string(),
            )),
            Test::Passed(model) => Some((
                Some(IconName::CheckCircle),
                c.text_success,
                match model {
                    Some(model) => format!("Connected using {model}."),
                    None => "Connected. The model replied.".into(),
                },
            )),
            Test::Failed(error) => Some((
                Some(IconName::WarningCircle),
                c.text_danger,
                error.to_string(),
            )),
        };
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(rems(0.5))
            .child(
                Button::new("test-connection", "Test connection")
                    .surface(Surface::Floating)
                    .loading(testing)
                    .disabled(ctx.busy || testing)
                    .on_click(cx.listener(|this: &mut T, _, _, cx| test(this, cx))),
            )
            .children(status.map(|(icon, color, text)| {
                div()
                    .flex()
                    .items_start()
                    .w_full()
                    .gap(rems(0.5))
                    .text_body_sm()
                    .text_color(color)
                    .children(icon.map(|icon| {
                        div()
                            .flex_none()
                            .pt(px(2.))
                            .child(Icon::new(icon).size(16.).color(color))
                    }))
                    .child(div().flex_1().min_w_0().child(text))
            }))
    }

    /// Advanced → Model (§5a.7 6c): Buzz Agent's Databricks workspace and
    /// model filter, and "Disconnect" from the workspace's sign-in.
    fn advanced<T: Form>(&self, ctx: &Scope, cx: &mut Context<T>) -> Div {
        let c = *cx.colors();
        let open = self.advanced_open;
        let header = div()
            .id("advanced-model-toggle")
            .flex()
            .items_center()
            .justify_between()
            .py(rems(0.5))
            .px(rems(0.5))
            .mx(rems(-0.5))
            .rounded(buzz_ui::tokens::radius::CONTROL)
            .cursor_pointer()
            .hover(|this| this.bg(c.affordance_subtle_hover))
            .child(div().text_label().child("Model"))
            .child(
                Icon::new(if open {
                    IconName::CaretUp
                } else {
                    IconName::CaretDown
                })
                .size(16.)
                .color(c.text_subtle),
            )
            .on_click(cx.listener(|this: &mut T, _, _, cx| {
                let ai = this.ai();
                ai.advanced_open = !ai.advanced_open;
                cx.notify();
            }));
        let mut section = div().flex().flex_col().gap(rems(1.)).child(header);
        if open {
            let status = self.disconnect.clone().map(|outcome| match outcome {
                Ok(text) => div().text_body_sm().text_color(c.text_subtle).child(text),
                Err(text) => div().text_body_sm().text_color(c.text_danger).child(text),
            });
            section = section
                .child(field(
                    "Databricks workspace (HTTPS origin)",
                    None,
                    TextField::new(&self.databricks_host).surface(Surface::Floating),
                    &c,
                ))
                .child(field(
                    "Model filter (optional)",
                    Some("Editing either field saves both displayed values.".into()),
                    TextField::new(&self.databricks_filter).surface(Surface::Floating),
                    &c,
                ))
                .child(
                    div().flex().child(
                        Button::new("databricks-disconnect", "Disconnect")
                            .surface(Surface::Floating)
                            .size(ButtonSize::Sm)
                            .loading(self.disconnecting)
                            .disabled(ctx.busy || self.disconnecting)
                            .on_click(cx.listener(|this: &mut T, _, _, cx| disconnect(this, cx))),
                    ),
                )
                .children(status)
                .child(note(
                    "Credentials are kept on the agent’s host for this workspace, not shared \
                     with buzz-app or old Buzz. Disconnect removes the host’s cache, not your \
                     browser session.",
                    &c,
                ));
        }
        // `h3 text-label`, 32px above and 8px below.
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .mt(rems(1.))
            .child(div().text_label().child("Advanced"))
            .child(section)
    }

    fn api_key_field(&self, provider: Provider, c: &Colors) -> Div {
        let env = provider.key.unwrap_or_default();
        field(
            format!("{} API key", provider.label),
            Some(
                format!(
                    "{env} is used for {}. Leave blank to keep a saved key, if present, or use \
                     one from agent defaults. Saved keys stay on the host and are never shown \
                     again.",
                    provider.label
                )
                .into(),
            ),
            TextField::new(&self.api_key).surface(Surface::Floating),
            c,
        )
    }

    fn model_field<T: Form>(
        &self,
        ctx: &Scope,
        kind: Option<HarnessKind>,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let browsable = matches!(kind, Some(HarnessKind::Goose | HarnessKind::Pi));
        let loading = matches!(self.catalog, Catalog::Loading);
        let is_open = ctx.open == Some(Picker::Model);
        let trailing = if loading {
            Spinner::new().size(16.).into_any_element()
        } else if browsable {
            IconButton::new("browse-models", IconName::CaretDown)
                .size(IconButtonSize::Xs)
                .tooltip("Browse models")
                .disabled(ctx.busy)
                .on_click(cx.listener(|this: &mut T, _, _, cx| {
                    let opening = *this.open() != Some(Picker::Model);
                    *this.open() = opening.then_some(Picker::Model);
                    if opening && !matches!(this.ai().catalog, Catalog::Ready { .. }) {
                        browse(this, cx);
                    }
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let input = TextField::new(&self.model)
            .surface(Surface::Floating)
            .trailing(trailing);
        let mut control = div().relative().w_full().child(input);
        if is_open {
            control = control.child(overlay::dropdown(
                window,
                Align::Start,
                div().min_w(rems(24.)).child(self.model_menu(cx)),
                dismiss(cx),
            ));
        }
        let help: Option<SharedString> = match &self.catalog {
            Catalog::Ready {
                overridden: true, ..
            } => {
                Some("An environment override chooses the model, so this choice is ignored.".into())
            }
            Catalog::Failed(error) if !is_open => Some(error.clone()),
            _ if kind == Some(HarnessKind::BuzzAgent) => {
                Some("Enter the model ID your provider uses.".into())
            }
            _ => None,
        };
        field(
            if self.for_defaults {
                "Default model"
            } else {
                "Model"
            },
            help,
            control,
            &c,
        )
    }

    fn model_menu<T: Form>(&self, cx: &mut Context<T>) -> Options {
        let typed = self.model.read(cx).value().trim().to_lowercase();
        let mut menu = Options::new("options");
        match &self.catalog {
            Catalog::Idle | Catalog::Loading => {
                menu = menu.child(
                    MenuItem::new("models-loading", "Loading models…")
                        .icon(IconName::CircleNotch)
                        .disabled(true),
                );
            }
            Catalog::Failed(error) => {
                menu = menu
                    .child(MenuItem::new("models-error", error.clone()).disabled(true))
                    .child(
                        MenuItem::new("models-retry", "Retry models")
                            .icon(IconName::ArrowsClockwise)
                            .on_click(cx.listener(|this: &mut T, _, _, cx| {
                                browse(this, cx);
                                cx.notify();
                            })),
                    );
            }
            Catalog::Ready { models, pi, .. } => {
                let pi = *pi;
                // A chosen model shows the whole list; typing filters it.
                let exact = models.iter().any(|model| model.id.to_lowercase() == typed);
                let shown: Vec<&ModelOption> = models
                    .iter()
                    .filter(|model| {
                        exact
                            || typed.is_empty()
                            || model.id.to_lowercase().contains(&typed)
                            || model.name.to_lowercase().contains(&typed)
                    })
                    .take(200)
                    .collect();
                if shown.is_empty() {
                    menu = menu.child(
                        MenuItem::new("models-empty", "Type a model ID to use a custom model.")
                            .disabled(true),
                    );
                }
                for (index, model) in shown.into_iter().enumerate() {
                    let id = model.id.clone();
                    let mut item = MenuItem::new(("model", index), model.name.clone())
                        .checked(model.id.to_lowercase() == typed);
                    if model.name != model.id {
                        item = item.trailing(div().text_caption().child(model.id.clone()));
                    }
                    menu = menu.child(item.on_click(pick(cx, move |this: &mut T, window, cx| {
                        let (provider, model) = match id.split_once('/') {
                            Some((provider, model)) if pi => {
                                (Some(provider.to_string()), model.to_string())
                            }
                            _ => (None, id.clone()),
                        };
                        let ai = this.ai();
                        if let Some(provider) = provider {
                            ai.provider = ProviderChoice::Id(provider);
                        }
                        ai.model
                            .update(cx, |state, cx| state.set_value(model, window, cx));
                    })));
                }
            }
        }
        menu
    }

    fn effort_field<T: Form>(
        &self,
        ctx: &Scope,
        kind: HarnessKind,
        window: &Window,
        cx: &mut Context<T>,
    ) -> Div {
        let c = *cx.colors();
        let unset = match self.inherited(kind, ctx) {
            Some(defaults) if !defaults.effort.is_empty() => {
                format!("Use agent defaults ({})", effort_label(&defaults.effort))
            }
            _ => "Not set (use harness default)".to_string(),
        };
        let mut menu = Options::new("options").child(
            MenuItem::new("effort-unset", unset.clone())
                .checked(self.effort.is_none())
                .on_click(pick(cx, |this: &mut T, _, _| this.ai().effort = None)),
        );
        for (index, effort) in efforts(kind).iter().enumerate() {
            let value = effort.to_string();
            menu = menu.child(
                MenuItem::new(("effort", index), effort_label(effort))
                    .checked(self.effort.as_deref() == Some(*effort))
                    .on_click(pick(cx, move |this: &mut T, _, _| {
                        this.ai().effort = Some(value.clone())
                    })),
            );
        }
        let value = self.effort.as_deref().map_or(unset, effort_label);
        field(
            if self.for_defaults {
                "Default effort"
            } else {
                "Effort"
            },
            None,
            select(
                ctx.open,
                ctx.busy,
                Picker::Effort,
                Select::new("effort-select").value(value),
                menu,
                window,
                cx,
            ),
            &c,
        )
    }

    fn session_field<T: Form>(&self, ctx: &Scope, window: &Window, cx: &mut Context<T>) -> Div {
        let c = *cx.colors();
        let label = |policy: SessionPolicy| match policy {
            SessionPolicy::Channel => "Entire channel",
            SessionPolicy::Thread => "Each thread",
        };
        let inherited = ctx
            .defaults
            .and_then(|defaults| defaults.session_policy)
            .unwrap_or(SessionPolicy::Thread);
        let inherit = format!("Use agent defaults ({})", label(inherited));
        let mut menu = Options::new("options");
        if !self.for_defaults {
            menu = menu.child(
                MenuItem::new("session-inherit", inherit.clone())
                    .checked(self.session.is_none())
                    .on_click(pick(cx, |this: &mut T, _, _| this.ai().session = None)),
            );
        }
        for policy in [SessionPolicy::Channel, SessionPolicy::Thread] {
            menu = menu.child(
                MenuItem::new(SharedString::from(label(policy)), label(policy))
                    .checked(self.session == Some(policy))
                    .on_click(pick(cx, move |this: &mut T, _, _| {
                        this.ai().session = Some(policy)
                    })),
            );
        }
        let value = self.session.map_or(inherit, |policy| label(policy).into());
        field(
            "Conversation context",
            Some(
                "Entire channel shares one conversation across threads. Each thread keeps a \
                 separate conversation; direct messages remain shared."
                    .into(),
            ),
            select(
                ctx.open,
                ctx.busy,
                Picker::Session,
                Select::new("session-select").value(value),
                menu,
                window,
                cx,
            ),
            &c,
        )
    }
}

/// Fetches the model catalog for the form's current draft. A newer fetch,
/// or a harness or provider change, discards an older result.
pub fn browse<T: Form>(this: &mut T, cx: &mut Context<T>) {
    let (host, request) = match this.models_request(cx) {
        Ok(request) => request,
        Err(error) => {
            this.ai().catalog = Catalog::Failed(error.into());
            return;
        }
    };
    let pi = matches!(&request, Request::Models { edit, .. }
        if harness::kind_of(&edit.harness.command, &[]) == Some(HarnessKind::Pi));
    let ai = this.ai();
    ai.generation += 1;
    let generation = ai.generation;
    ai.catalog = Catalog::Loading;
    let response = host.update(cx, |host, cx| host.request(request, cx));
    cx.spawn(async move |this, cx| {
        let response = response.await;
        let _ = this.update(cx, |this, cx| {
            let ai = this.ai();
            if ai.generation != generation {
                return;
            }
            ai.catalog = match response {
                Ok(Response::Models(catalog)) => Catalog::Ready {
                    models: catalog.models,
                    overridden: catalog.model_overridden,
                    pi,
                },
                Ok(_) => Catalog::Failed("The host sent an unexpected reply.".into()),
                Err(error) => Catalog::Failed(error.to_string().into()),
            };
            cx.notify();
        });
    })
    .detach();
}

/// "Test connection": one short prompt through the draft's harness.
fn test<T: Form>(this: &mut T, cx: &mut Context<T>) {
    let (host, request) = match this.models_request(cx) {
        Ok((host, Request::Models { id, revision, edit })) => {
            (host, Request::TestConnection { id, revision, edit })
        }
        Ok(_) => return,
        Err(error) => {
            this.ai().test = Test::Failed(error.into());
            cx.notify();
            return;
        }
    };
    *this.open() = None;
    this.ai().test = Test::Testing;
    let response = host.update(cx, |host, cx| host.request(request, cx));
    cx.spawn(async move |this, cx| {
        let response = response.await;
        let _ = this.update(cx, |this, cx| {
            let ai = this.ai();
            // A harness or provider change meanwhile already cleared it.
            if !matches!(ai.test, Test::Testing) {
                return;
            }
            ai.test = match response {
                Ok(Response::Tested { model }) => Test::Passed(model),
                Ok(_) => Test::Failed("The host sent an unexpected reply.".into()),
                Err(error) => Test::Failed(error.to_string().into()),
            };
            cx.notify();
        });
    })
    .detach();
    cx.notify();
}

/// "Disconnect": removes the host's cached Databricks sign-in for the
/// shown workspace, or for the one agent defaults set.
fn disconnect<T: Form>(this: &mut T, cx: &mut Context<T>) {
    let host = match this.models_request(cx) {
        Ok((host, _)) => host,
        Err(error) => {
            this.ai().disconnect = Some(Err(error.into()));
            cx.notify();
            return;
        }
    };
    let ai = this.ai();
    let workspace = ai.databricks_host.read(cx).value().trim().to_string();
    ai.disconnecting = true;
    ai.disconnect = Some(Ok(
        "Removing this host’s credentials for this workspace…".into()
    ));
    let response = host.update(cx, |host, cx| {
        host.request(Request::DisconnectDatabricks { workspace }, cx)
    });
    cx.spawn(async move |this, cx| {
        let response = response.await;
        let _ = this.update(cx, |this, cx| {
            let ai = this.ai();
            ai.disconnecting = false;
            ai.disconnect = Some(match response {
                Ok(_) => Ok("Disconnected from this workspace on this host.".into()),
                Err(error) => Err(error.to_string().into()),
            });
            cx.notify();
        });
    })
    .detach();
    cx.notify();
}

/// One environment variable in the form.
pub struct EnvRow {
    pub key: String,
    value: Entity<InputState>,
    /// Saved on the host; its value is unknown here.
    saved: bool,
    removed: bool,
    _subscription: Subscription,
}

/// The environment overrides editor. Saved values are never shown: a row
/// for a saved key starts empty and only a typed value replaces it.
pub struct EnvFields {
    pub rows: Vec<EnvRow>,
    name: Entity<InputState>,
    error: Option<SharedString>,
    _subscription: Subscription,
}

/// The worker count is a number, not a secret.
const PLAIN_KEYS: &[&str] = &["BUZZ_ACP_AGENTS"];

impl EnvFields {
    pub fn new<T: Form>(window: &mut Window, cx: &mut Context<T>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("VARIABLE_NAME"));
        let subscription = cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify());
        Self {
            rows: Vec::new(),
            name,
            error: None,
            _subscription: subscription,
        }
    }

    fn row<T: Form>(
        key: String,
        value: Option<String>,
        saved: bool,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> EnvRow {
        let masked = !PLAIN_KEYS.contains(&key.as_str());
        let placeholder = if saved {
            "Saved value unchanged"
        } else {
            "New value"
        };
        let state = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .masked(masked)
                .placeholder(placeholder);
            if let Some(value) = value {
                state.set_value(value, window, cx);
            }
            state
        });
        let subscription = cx.subscribe(&state, |_, _, _: &InputEvent, cx| cx.notify());
        EnvRow {
            key,
            value: state,
            saved,
            removed: false,
            _subscription: subscription,
        }
    }

    /// Lists the saved keys, and new variables with their starting values.
    pub fn load<T: Form>(
        &mut self,
        saved: &[String],
        new: &[(&str, &str)],
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        self.rows = saved
            .iter()
            .map(|key| Self::row(key.clone(), None, true, window, cx))
            .collect();
        for (key, value) in new {
            if !saved.iter().any(|saved| saved == key) {
                let row = Self::row(key.to_string(), Some(value.to_string()), false, window, cx);
                self.rows.push(row);
            }
        }
        self.error = None;
    }

    /// `None` removes a saved key; values replace. Untouched saved keys
    /// are left out, which keeps them.
    pub fn patch(&self, cx: &App) -> BTreeMap<String, Option<String>> {
        self.rows
            .iter()
            .filter_map(|row| {
                let value = row.value.read(cx).value().to_string();
                match (row.saved, row.removed) {
                    (true, true) => Some((row.key.clone(), None)),
                    (true, false) if value.is_empty() => None,
                    (_, false) => Some((row.key.clone(), Some(value))),
                    (false, true) => None,
                }
            })
            .collect()
    }

    fn add<T: Form>(&mut self, window: &mut Window, cx: &mut Context<T>) {
        let key = self.name.read(cx).value().trim().to_string();
        if !valid_env_key(&key) {
            self.error = Some(
                "Use letters, numbers and underscores; start with a letter or underscore.".into(),
            );
            return;
        }
        let value = if let Some(row) = self.rows.iter_mut().find(|row| row.key == key) {
            if !row.removed {
                self.error = Some("That variable is already listed.".into());
                return;
            }
            row.removed = false;
            row.value.update(cx, |state, cx| {
                state.set_placeholder("Saved value unchanged", window, cx)
            });
            row.value.clone()
        } else {
            let row = Self::row(key, None, false, window, cx);
            let value = row.value.clone();
            self.rows.push(row);
            value
        };
        self.error = None;
        self.name
            .update(cx, |state, cx| state.set_value("", window, cx));
        value.update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn render<T: Form>(&self, title: &'static str, busy: bool, cx: &mut Context<T>) -> Div {
        let c = *cx.colors();
        let mut rows = div().flex().flex_col().gap(rems(0.75));
        for (index, row) in self.rows.iter().enumerate() {
            let value = row.value.read(cx).value().to_string();
            let state = match (row.saved, row.removed) {
                (_, true) => Some("Will be removed on save"),
                (true, false) if !value.is_empty() => Some("Will replace on save"),
                (false, false) if value.is_empty() => Some("Will save an empty value"),
                _ => None,
            };
            let key = row.key.clone();
            rows = rows.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.25))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(rems(0.5))
                            .child(
                                div()
                                    .w(rems(14.))
                                    .min_w_0()
                                    .truncate()
                                    .text_mono()
                                    .child(row.key.clone()),
                            )
                            .child(
                                div().flex_1().min_w(rems(10.)).child(
                                    TextField::new(&row.value)
                                        .small()
                                        .surface(Surface::Floating)
                                        .disabled(busy || row.removed),
                                ),
                            )
                            .child(
                                div().flex().justify_end().w(rems(5.5)).child(
                                    Button::new(
                                        ("env-remove", index),
                                        if row.removed { "Undo" } else { "Remove" },
                                    )
                                    .size(ButtonSize::Sm)
                                    .surface(Surface::Floating)
                                    .disabled(busy)
                                    .on_click(cx.listener(
                                        move |this: &mut T, _, window, cx| {
                                            let env = this.env();
                                            let Some(at) =
                                                env.rows.iter().position(|row| row.key == key)
                                            else {
                                                return;
                                            };
                                            let row = &mut env.rows[at];
                                            if !row.saved {
                                                env.rows.remove(at);
                                            } else {
                                                row.removed = !row.removed;
                                                let placeholder = if row.removed {
                                                    "Will be removed on save"
                                                } else {
                                                    "Saved value unchanged"
                                                };
                                                row.value.update(cx, |state, cx| {
                                                    state.set_value("", window, cx);
                                                    state.set_placeholder(placeholder, window, cx);
                                                });
                                            }
                                            cx.notify();
                                        },
                                    )),
                                ),
                            ),
                    )
                    .children(state.map(|state| note(state, &c))),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(div().text_label().child(title))
            .child(note(
                "Saved values stay on the host. Leave them unchanged to keep them; \
                 replacements are write-only. Remove takes effect only when you save. Do not \
                 put secrets in arguments or prompts.",
                &c,
            ))
            .when(!self.rows.is_empty(), |this| this.child(rows))
            .child(field(
                "Variable name",
                None,
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .child(
                        div().flex_1().child(
                            TextField::new(&self.name)
                                .surface(Surface::Floating)
                                .invalid(self.error.is_some())
                                .disabled(busy),
                        ),
                    )
                    .child(
                        Button::new("env-add", "Add variable")
                            .surface(Surface::Floating)
                            .disabled(busy)
                            .on_click(cx.listener(|this: &mut T, _, window, cx| {
                                this.env().add(window, cx);
                                cx.notify();
                            })),
                    ),
                &c,
            ))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::valid_env_key;

    #[test]
    fn environment_names_follow_the_controller() {
        for good in ["OPENAI_API_KEY", "_PRIVATE", "a1"] {
            assert!(valid_env_key(good), "{good}");
        }
        for bad in ["", "1ABC", "WITH-DASH", "SPACE KEY", "ÉTÉ"] {
            assert!(!valid_env_key(bad), "{bad}");
        }
    }
}
