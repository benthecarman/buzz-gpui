//! Agent defaults (ui.md §5a.11, buzz-app's `AgentDefaultsCard`): what a
//! host's agents use for choices they leave empty.
//!
//! buzz-app has one device, so its defaults are a card on Settings → Agents.
//! This computer's defaults are that card here too ([`DefaultsCard`]). Each
//! remote host has its own defaults, which open in a dialog from the host's
//! row ([`DefaultsDialog`]), as Settings → Agents shows one host's harnesses.
//!
//! Environment values are write-only: the host returns keys, never values.

use super::editor::{Dismissed, Saved, connected, saved_message};
use super::form::{Options, field};
use crate::hosts::HostConnection;
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_host::api::{
    AgentDefaults, AgentEdit, BuildDefaults, DefaultsEdit, Harness, HarnessEdit, HarnessKind,
    ModelOption, Request, Response, SessionPolicy,
};
use buzz_host::harness::{Provider, defaults_name, efforts, from_defaults_name, providers};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, Colors, Dialog, DialogSize, Icon, IconButton, IconButtonSize,
    IconName, MenuItem, Select, Surface, TextField, TypeStyled, tokens::radius,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::rc::Rc;

const HARNESSES: [(HarnessKind, &str); 3] = [
    (HarnessKind::BuzzAgent, "Buzz Agent"),
    (HarnessKind::Goose, "Goose"),
    (HarnessKind::Pi, "Pi"),
];
const VISIBLE_MODEL_LIMIT: usize = 10;
const DATABRICKS: [&str; 3] = ["databricks_v2", "databricks-v2", "databricks"];

/// An environment patch: `None` removes a saved variable.
pub type Environment = BTreeMap<String, Option<String>>;

/// buzz-app's `AgentDefaultsEdit`: the card's unsaved choices.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub harness: HarnessKind,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub session_policy: SessionPolicy,
    pub environment: Environment,
}

impl Draft {
    pub fn from_saved(saved: &AgentDefaults) -> Self {
        Self {
            harness: from_defaults_name(&saved.harness).unwrap_or(HarnessKind::BuzzAgent),
            provider: saved.provider.clone(),
            model: saved.model.clone(),
            effort: saved.effort.clone(),
            session_policy: saved.session_policy.unwrap_or(SessionPolicy::Thread),
            environment: Environment::new(),
        }
    }

    fn edit(&self) -> DefaultsEdit {
        DefaultsEdit {
            harness: defaults_name(self.harness).unwrap_or("buzz-agent").into(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            session_policy: Some(self.session_policy),
            environment: self.environment.clone(),
        }
    }
}

/// A variable is set when the draft gives it a value, or it is saved and
/// the draft leaves it alone.
pub fn environment_set(environment: &Environment, saved: &[String], key: &str) -> bool {
    match environment.get(key) {
        Some(value) => value.is_some(),
        None => saved.iter().any(|saved| saved == key),
    }
}

/// buzz-app's `effectiveGooseProvider`: the provider Goose will use, or
/// `None` when a saved `GOOSE_PROVIDER` decides it and its value is hidden.
pub fn effective_goose_provider(
    provider: &str,
    environment: &Environment,
    saved: &[String],
) -> Option<String> {
    match environment.get("GOOSE_PROVIDER") {
        Some(Some(value)) => Some(value.clone()),
        None if saved.iter().any(|key| key == "GOOSE_PROVIDER") => None,
        _ => Some(provider.to_string()),
    }
}

/// buzz-app's `defaultsApiKey`: the provider whose key the card asks for.
pub fn api_key(draft: &Draft, saved: &[String]) -> Option<Provider> {
    let id = match draft.harness {
        HarnessKind::Pi => draft.provider.clone(),
        HarnessKind::Goose => effective_goose_provider(&draft.provider, &draft.environment, saved)?,
        _ => return None,
    };
    providers(draft.harness)
        .iter()
        .find(|provider| provider.id == id && provider.key.is_some())
        .copied()
}

/// buzz-app's `defaultLabel`: Buzz Agent names the build's value.
pub fn default_label(kind: HarnessKind, value: &str) -> String {
    if !value.is_empty() && kind == HarnessKind::BuzzAgent {
        format!("Use build default ({value})")
    } else {
        "Not set (use harness default)".into()
    }
}

/// The variable that overrides the provider choice.
fn provider_key(kind: HarnessKind) -> Option<&'static str> {
    match kind {
        HarnessKind::BuzzAgent => Some("BUZZ_AGENT_PROVIDER"),
        HarnessKind::Goose => Some("GOOSE_PROVIDER"),
        _ => None,
    }
}

/// The variable that overrides the model choice.
fn model_key(kind: HarnessKind) -> Option<&'static str> {
    match kind {
        HarnessKind::BuzzAgent => Some("BUZZ_AGENT_MODEL"),
        HarnessKind::Goose => Some("GOOSE_MODEL"),
        _ => None,
    }
}

/// `(value, label)` choices, the unset one first.
pub type Choices = Vec<(String, String)>;

/// The provider choices. Pi lists the providers its catalog reported
/// first, then those a key can sign in to.
pub fn provider_choices(
    kind: HarnessKind,
    models: &[ModelOption],
    build: &BuildDefaults,
) -> Choices {
    let label = |id: &str| {
        providers(kind)
            .iter()
            .find(|provider| provider.id == id)
            .map_or_else(|| id.to_string(), |provider| provider.label.to_string())
    };
    let mut list: Choices = Vec::new();
    if kind == HarnessKind::Pi {
        let mut discovered: Vec<String> = Vec::new();
        for model in models {
            let provider = model.id.split('/').next().unwrap_or("");
            if !provider.is_empty() && !discovered.iter().any(|seen| seen == provider) {
                discovered.push(provider.to_string());
            }
        }
        for id in &discovered {
            list.push((id.clone(), format!("{} (available in Pi)", label(id))));
        }
        for provider in providers(kind) {
            if !discovered.iter().any(|id| id == provider.id) {
                list.push((
                    provider.id.to_string(),
                    format!("{} (API key may be needed)", provider.label),
                ));
            }
        }
    } else {
        list.extend(
            providers(kind)
                .iter()
                .map(|provider| (provider.id.to_string(), provider.label.to_string())),
        );
    }
    let built_in = list
        .iter()
        .find(|(value, _)| *value == build.provider)
        .map_or_else(|| build.provider.clone(), |(_, label)| label.clone());
    let mut choices = vec![(String::new(), default_label(kind, &built_in))];
    choices.extend(list);
    choices
}

/// The effort choices, capitalized as buzz-app shows them.
pub fn effort_choices(kind: HarnessKind) -> Choices {
    let mut choices = vec![(String::new(), "Not set (use harness default)".to_string())];
    choices.extend(efforts(kind).iter().map(|effort| {
        let label = match *effort {
            "xhigh" => "Extra high".to_string(),
            other => {
                let mut chars = other.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            }
        };
        (effort.to_string(), label)
    }));
    choices
}

/// Whether the build's Databricks model fills in an unset Buzz Agent model.
pub fn build_model_applies(draft: &Draft, saved: &[String], build: &BuildDefaults) -> bool {
    let env = &draft.environment;
    let provider = match env.get("BUZZ_AGENT_PROVIDER") {
        Some(Some(value)) => Some(value.clone()),
        _ if environment_set(env, saved, "BUZZ_AGENT_PROVIDER") => None,
        _ if draft.provider.is_empty() => Some(build.provider.clone()),
        _ => Some(draft.provider.clone()),
    };
    draft.harness == HarnessKind::BuzzAgent
        && provider.is_some_and(|provider| DATABRICKS.contains(&provider.as_str()))
        && !environment_set(env, saved, "BUZZ_AGENT_MODEL")
        && !environment_set(env, saved, "DATABRICKS_MODEL")
}

/// Up to ten models matching `query` by name or ID, plus the selected one
/// so filtering never turns it into a custom ID.
pub fn matching<'a>(
    entries: &[&'a ModelOption],
    query: &str,
    selected: &str,
) -> Vec<&'a ModelOption> {
    let query = query.to_lowercase();
    let mut shown: Vec<&ModelOption> = entries
        .iter()
        .filter(|model| {
            format!("{} {}", model.name, model.id)
                .to_lowercase()
                .contains(&query)
        })
        .take(VISIBLE_MODEL_LIMIT)
        .copied()
        .collect();
    if let Some(chosen) = entries.iter().find(|model| model.id == selected)
        && !shown.iter().any(|model| model.id == chosen.id)
    {
        shown.insert(0, chosen);
    }
    shown
}

fn model_label(model: &ModelOption) -> String {
    if model.name == model.id {
        model.id.clone()
    } else {
        format!("{} · {}", model.name, model.id)
    }
}

/// A change keeps a typed API key only while the card still asks for it.
pub fn retire_key(current: &Draft, mut next: Draft, saved: &[String]) -> Draft {
    if let Some(previous) = api_key(current, saved).and_then(|provider| provider.key)
        && (next.harness != current.harness
            || api_key(&next, saved).and_then(|provider| provider.key) != Some(previous))
        && matches!(next.environment.get(previous), Some(Some(_)))
    {
        next.environment.remove(previous);
    }
    next
}

/// The environment rows: every saved or edited key, its state, and whether
/// it is removed on save.
pub fn environment_rows(
    saved: &[String],
    environment: &Environment,
) -> Vec<(String, &'static str, bool)> {
    let mut keys: Vec<String> = saved
        .iter()
        .cloned()
        .chain(environment.keys().cloned())
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|key| {
            let (state, removed) = match environment.get(&key) {
                Some(None) => ("Removed on save", true),
                Some(Some(_)) => ("Set on save", false),
                None => ("Set", false),
            };
            (key, state, removed)
        })
        .collect()
}

/// Remove marks a saved key for removal; Keep (or removing a new one)
/// drops the edit.
pub fn toggle_variable(environment: &mut Environment, saved: &[String], key: &str) {
    let removed = matches!(environment.get(key), Some(None));
    if removed || !saved.iter().any(|saved| saved == key) {
        environment.remove(key);
    } else {
        environment.insert(key.to_string(), None);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Harness,
    Provider,
    Model,
    Effort,
    Session,
}

/// Selects in "Custom ID" mode, until the harness or edit session changes.
#[derive(Default)]
struct Custom {
    provider: bool,
    model: bool,
    effort: bool,
}

/// The model lookup for the draft's lookup context (`key`).
#[derive(Default)]
struct Lookup {
    key: String,
    models: Option<Vec<ModelOption>>,
    status: Option<SharedString>,
    busy: bool,
    attempted: bool,
    generation: u64,
}

/// Input placeholders and masking as last set, to set them only on change.
#[derive(Default, PartialEq)]
struct Shown {
    key_placeholder: SharedString,
    masked: bool,
}

pub struct DefaultsCard {
    host: Entity<HostConnection>,
    /// `None` until something changes: Save stays disabled.
    draft: Option<Draft>,
    /// Bumped by Save and Discard; it resets Custom ID modes.
    session: u64,
    custom: Custom,
    custom_for: (HarnessKind, u64),
    open: Option<Menu>,
    provider_input: Entity<InputState>,
    model_input: Entity<InputState>,
    effort_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    search_input: Entity<InputState>,
    name_input: Entity<InputState>,
    value_input: Entity<InputState>,
    revealed: bool,
    shown: Shown,
    add_open: bool,
    lookup: Lookup,
    saving: bool,
    /// Save or Discard emptied the new variable's fields.
    clear_new: bool,
    notice: Option<SharedString>,
    error: Option<SharedString>,
    /// Shown in [`DefaultsDialog`]: a clean save closes it.
    in_dialog: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for DefaultsCard {}
impl EventEmitter<Saved> for DefaultsCard {}

impl DefaultsCard {
    pub fn new(
        host: Entity<HostConnection>,
        in_dialog: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        host.update(cx, |host, cx| host.load_defaults(cx));
        let input =
            |placeholder: &str, masked: bool, window: &mut Window, cx: &mut Context<Self>| {
                let placeholder = placeholder.to_string();
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .masked(masked)
                        .placeholder(placeholder)
                })
            };
        let provider_input = input("", false, window, cx);
        let model_input = input("", false, window, cx);
        let effort_input = input("", false, window, cx);
        let api_key_input = input("", true, window, cx);
        let search_input = input("", false, window, cx);
        let name_input = input("", false, window, cx);
        let value_input = input("", true, window, cx);
        let subscriptions = vec![
            cx.observe(&host, |_, _, cx| cx.notify()),
            cx.subscribe(&provider_input, Self::on_input),
            cx.subscribe(&model_input, Self::on_input),
            cx.subscribe(&effort_input, Self::on_input),
            cx.subscribe(&api_key_input, Self::on_input),
            cx.subscribe(&search_input, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&name_input, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&value_input, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        Self {
            host,
            draft: None,
            session: 0,
            custom: Custom::default(),
            custom_for: (HarnessKind::BuzzAgent, 0),
            open: None,
            provider_input,
            model_input,
            effort_input,
            api_key_input,
            search_input,
            name_input,
            value_input,
            revealed: false,
            shown: Shown::default(),
            add_open: false,
            lookup: Lookup::default(),
            saving: false,
            clear_new: false,
            notice: None,
            error: None,
            in_dialog,
            _subscriptions: subscriptions,
        }
    }

    fn saved<'a>(&self, cx: &'a App) -> Option<&'a AgentDefaults> {
        self.host.read(cx).defaults.as_ref()
    }

    fn saved_keys(&self, cx: &App) -> Vec<String> {
        self.saved(cx)
            .map(|saved| saved.environment_keys.clone())
            .unwrap_or_default()
    }

    fn current(&self, cx: &App) -> Option<Draft> {
        self.draft
            .clone()
            .or_else(|| self.saved(cx).map(Draft::from_saved))
    }

    fn harness(&self, kind: HarnessKind, cx: &App) -> Option<Harness> {
        self.host
            .read(cx)
            .harnesses
            .as_ref()?
            .iter()
            .find(|harness| harness.kind == kind)
            .cloned()
    }

    fn disabled(&self, cx: &App) -> bool {
        self.saving || !connected(&self.host, cx) || self.saved(cx).is_none()
    }

    /// buzz-app's `change`: any edit starts (or updates) the draft.
    fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Draft)) {
        let Some(current) = self.current(cx) else {
            return;
        };
        let mut next = current.clone();
        edit(&mut next);
        let saved = self.saved_keys(cx);
        self.draft = Some(retire_key(&current, next, &saved));
        self.notice = None;
        cx.notify();
    }

    fn change_provider(&mut self, provider: String, cx: &mut Context<Self>) {
        self.change(cx, |draft| {
            if draft.provider != provider {
                draft.model.clear();
            }
            draft.provider = provider;
        });
    }

    /// Text typed into one of the draft's inputs.
    fn on_input(&mut self, state: Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let value = state.read(cx).value().to_string();
        if state == self.provider_input {
            self.custom.provider = true;
            self.change_provider(value, cx);
        } else if state == self.model_input {
            self.custom.model = true;
            self.change(cx, |draft| draft.model = value);
        } else if state == self.effort_input {
            self.custom.effort = true;
            self.change(cx, |draft| draft.effort = value);
        } else if state == self.api_key_input {
            let saved = self.saved_keys(cx);
            let Some(env) = self
                .current(cx)
                .and_then(|draft| api_key(&draft, &saved))
                .and_then(|provider| provider.key)
            else {
                return;
            };
            if value.is_empty() {
                self.revealed = false;
            }
            self.change(cx, |draft| {
                if value.is_empty() {
                    draft.environment.remove(env);
                } else {
                    draft.environment.insert(env.to_string(), Some(value));
                }
            });
        }
    }

    /// A choice picked from a select's menu.
    fn pick(&mut self, menu: Menu, value: String, cx: &mut Context<Self>) {
        self.open = None;
        match menu {
            Menu::Provider => {
                self.custom.provider = false;
                self.change_provider(value, cx);
            }
            Menu::Model => {
                self.custom.model = false;
                let pi = self
                    .current(cx)
                    .is_some_and(|d| d.harness == HarnessKind::Pi);
                let fresh = self
                    .lookup
                    .models
                    .as_ref()
                    .is_some_and(|models| models.iter().any(|model| model.id == value));
                self.change(cx, |draft| {
                    if pi && value.is_empty() {
                        draft.provider.clear();
                        draft.model.clear();
                    } else if pi
                        && fresh
                        && let Some((provider, model)) = value.split_once('/')
                    {
                        draft.provider = provider.to_string();
                        draft.model = model.to_string();
                    } else {
                        draft.model = value;
                    }
                });
            }
            Menu::Effort => {
                self.custom.effort = false;
                self.change(cx, |draft| draft.effort = value);
            }
            Menu::Harness | Menu::Session => {}
        }
    }

    /// "Custom ID": shows the input with the current value, focused.
    fn pick_custom(&mut self, menu: Menu, window: &mut Window, cx: &mut Context<Self>) {
        self.open = None;
        let input = match menu {
            Menu::Provider => {
                self.custom.provider = true;
                &self.provider_input
            }
            Menu::Model => {
                self.custom.model = true;
                &self.model_input
            }
            Menu::Effort => {
                self.custom.effort = true;
                &self.effort_input
            }
            Menu::Harness | Menu::Session => return,
        };
        input.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.current(cx) else {
            return;
        };
        let Some(harness) = self.harness(draft.harness, cx) else {
            return;
        };
        let Some(command) = harness.command.clone() else {
            return;
        };
        if self.lookup.busy || draft.environment.values().any(Option::is_none) {
            return;
        }
        let build = self
            .saved(cx)
            .map(|saved| saved.build.clone())
            .unwrap_or_default();
        let provider = if draft.harness == HarnessKind::BuzzAgent && draft.provider.is_empty() {
            if build.provider.is_empty() {
                "databricks_v2".to_string()
            } else {
                build.provider.clone()
            }
        } else {
            draft.provider.clone()
        };
        let edit = AgentEdit {
            session_policy: Some(Some(draft.session_policy)),
            harness: HarnessEdit {
                command,
                provider,
                model: draft.model.clone(),
                ..Default::default()
            },
            environment: draft.environment.clone(),
            ..Default::default()
        };
        self.lookup.generation += 1;
        let generation = self.lookup.generation;
        self.lookup.busy = true;
        self.lookup.attempted = true;
        self.lookup.status = Some(format!("Loading {} models…", harness.label).into());
        let label = harness.label.clone();
        let response = self.host.update(cx, |host, cx| {
            host.request(
                Request::Models {
                    id: None,
                    revision: 0,
                    edit,
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let response = response.await;
            let _ = this.update(cx, |this, cx| {
                if this.lookup.generation != generation {
                    return;
                }
                this.lookup.busy = false;
                this.lookup.status = Some(match response {
                    Ok(Response::Models(catalog)) => {
                        let status = if catalog.model_overridden {
                            "A saved environment model override takes precedence over this \
                             selection."
                                .to_string()
                        } else if catalog.models.is_empty() {
                            format!("No {label} models found. Enter a custom model ID or retry.")
                        } else {
                            "Model choices loaded. Availability does not confirm inference access."
                                .to_string()
                        };
                        this.lookup.models = Some(catalog.models);
                        status.into()
                    }
                    Ok(_) => "The host sent an unexpected reply.".into(),
                    Err(error) => error.to_string().into(),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn cancel_lookup(&mut self, cx: &mut Context<Self>) {
        self.lookup.generation += 1;
        self.lookup.busy = false;
        self.lookup.status = Some("Cancelled. Retry when ready.".into());
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        if self.saving {
            return;
        }
        self.saving = true;
        self.error = None;
        self.open = None;
        let response = self.host.update(cx, |host, cx| {
            host.request(Request::SaveDefaults { edit: draft.edit() }, cx)
        });
        cx.spawn(async move |this, cx| {
            let response = response.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match response {
                    Ok(Response::Defaults(saved)) => {
                        let failures = saved.restart_failures.unwrap_or(0);
                        let message = saved_message(saved.restarted.unwrap_or(0), failures);
                        this.reset_session();
                        if this.in_dialog && failures == 0 {
                            cx.emit(Saved(message.into()));
                            cx.emit(Dismissed);
                        } else {
                            this.notice = Some(message.into());
                        }
                    }
                    Ok(_) => this.error = Some("The host sent an unexpected reply.".into()),
                    // A save may already have committed (a restart failed),
                    // so never claim nothing was saved.
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Save and Discard: back to what is saved, in a new edit session.
    fn reset_session(&mut self) {
        self.draft = None;
        self.clear_new = true;
        self.session += 1;
        self.revealed = false;
    }

    fn add_variable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.name_input.read(cx).value().trim().to_string();
        if key.is_empty() {
            return;
        }
        let value = self.value_input.read(cx).value().to_string();
        self.change(cx, |draft| {
            draft.environment.insert(key, Some(value));
        });
        self.name_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        self.value_input
            .update(cx, |state, cx| state.set_value("", window, cx));
    }

    /// Keeps inputs, placeholders and masking in step with the draft; the
    /// draft changes outside the inputs too (a select, Save, Discard).
    fn sync(
        &mut self,
        draft: &Draft,
        saved: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let set = |state: &Entity<InputState>,
                   value: &str,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            if state.read(cx).value().as_ref() != value {
                let value = value.to_string();
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        };
        set(&self.provider_input, &draft.provider, window, cx);
        set(&self.model_input, &draft.model, window, cx);
        set(&self.effort_input, &draft.effort, window, cx);
        let key = api_key(draft, saved).and_then(|provider| provider.key);
        let value = key
            .and_then(|key| draft.environment.get(key).cloned().flatten())
            .unwrap_or_default();
        set(&self.api_key_input, &value, window, cx);
        if self.clear_new {
            self.clear_new = false;
            set(&self.name_input, "", window, cx);
            set(&self.value_input, "", window, cx);
        }
        let placeholder: SharedString = match key.map(|key| draft.environment.get(key)) {
            Some(Some(None)) => "Will remove on save".into(),
            _ if key.is_some_and(|key| saved.iter().any(|saved| saved == key)) => {
                "Saved key unchanged".into()
            }
            _ if draft.harness == HarnessKind::Pi => {
                "Paste API key or use an existing Pi sign-in".into()
            }
            _ => "Paste API key or use existing Goose credentials".into(),
        };
        let shown = Shown {
            key_placeholder: placeholder.clone(),
            masked: !(self.revealed && !value.is_empty()),
        };
        if shown != self.shown {
            let masked = shown.masked;
            self.api_key_input.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx);
                state.set_masked(masked, window, cx);
            });
            self.shown = shown;
        }
    }

    /// A select and its menu while open.
    #[allow(clippy::too_many_arguments)]
    fn select(
        &self,
        id: &'static str,
        menu: Menu,
        value: SharedString,
        items: Vec<AnyElement>,
        disabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.open == Some(menu);
        let page = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| {
                this.open = None;
                cx.notify();
            });
        });
        let mut options = Options::new("options");
        for item in items {
            options = options.child(item);
        }
        div()
            .relative()
            .w_full()
            .child(
                Select::new(id)
                    .value(value)
                    .open(open)
                    .surface(self.surface())
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open = (this.open != Some(menu)).then_some(menu);
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(overlay::dropdown(
                    window,
                    Align::Start,
                    div().min_w(rems(20.)).child(options),
                    dismiss,
                ))
            })
    }

    fn surface(&self) -> Surface {
        if self.in_dialog {
            Surface::Floating
        } else {
            Surface::default()
        }
    }

    /// buzz-app's `DefaultsChoice`: a select whose "Custom ID" shows an
    /// input; a saved ID absent from the choices stays editable there.
    #[allow(clippy::too_many_arguments)]
    fn choice(
        &self,
        id: &'static str,
        menu: Menu,
        label: &'static str,
        value: &str,
        choices: &Choices,
        custom: bool,
        input: &Entity<InputState>,
        disabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let index = choices.iter().position(|(choice, _)| choice == value);
        let show_input = custom || index.is_none();
        let shown: SharedString = match index {
            Some(index) if !show_input => choices[index].1.clone().into(),
            _ => "Custom ID".into(),
        };
        let mut items: Vec<AnyElement> = choices
            .iter()
            .enumerate()
            .map(|(at, (choice, text))| {
                let choice = choice.clone();
                MenuItem::new((id, at), text.clone())
                    .checked(!show_input && index == Some(at))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.pick(menu, choice.clone(), cx)),
                    )
                    .into_any_element()
            })
            .collect();
        items.push(
            MenuItem::new((id, choices.len()), "Custom ID")
                .checked(show_input)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.pick_custom(menu, window, cx)),
                )
                .into_any_element(),
        );
        div()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .child(field(
                label,
                None,
                self.select(id, menu, shown, items, disabled, window, cx),
                &c,
            ))
            .when(show_input, |this| {
                this.child(field(
                    format!("Custom {} ID", label.to_lowercase()),
                    None,
                    TextField::new(input)
                        .surface(self.surface())
                        .disabled(disabled),
                    &c,
                ))
            })
    }

    fn warning(text: impl Into<SharedString>, c: &Colors) -> Div {
        div()
            .text_body_sm()
            .text_color(c.text_warning)
            .child(text.into())
    }

    fn secondary(text: impl Into<SharedString>, c: &Colors) -> Div {
        div()
            .text_body_sm()
            .text_color(c.text_subtle)
            .child(text.into())
    }

    /// The fields, from Default harness to Environment variables.
    fn fields(
        &self,
        draft: &Draft,
        saved: &AgentDefaults,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let disabled = self.disabled(cx);
        let keys = &saved.environment_keys;
        let kind = draft.harness;
        let pi = kind == HarnessKind::Pi;
        let harness = self.harness(kind, cx);
        let models = self.lookup.models.clone().unwrap_or_default();

        // Default harness.
        let harness_items = HARNESSES
            .iter()
            .enumerate()
            .map(|(at, (option, label))| {
                let option = *option;
                MenuItem::new(("defaults-harness", at), *label)
                    .checked(option == kind)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open = None;
                        // Keep the provider; clear what belongs to the old harness.
                        this.change(cx, |draft| {
                            if draft.harness != option {
                                draft.model.clear();
                                draft.effort.clear();
                            }
                            draft.harness = option;
                        });
                    }))
                    .into_any_element()
            })
            .collect();
        let harness_label = HARNESSES
            .iter()
            .find(|(option, _)| *option == kind)
            .map_or("Buzz Agent", |(_, label)| *label);
        let mut fields = div().flex().flex_col().gap(rems(1.)).child(field(
            "Default harness",
            None,
            self.select(
                "defaults-harness-select",
                Menu::Harness,
                harness_label.into(),
                harness_items,
                disabled,
                window,
                cx,
            ),
            &c,
        ));

        // Default provider, and the variable that overrides it.
        let choices = provider_choices(kind, &models, &saved.build);
        let override_key =
            provider_key(kind).filter(|key| environment_set(&draft.environment, keys, key));
        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .child(self.choice(
                    "defaults-provider",
                    Menu::Provider,
                    "Default provider",
                    &draft.provider,
                    &choices,
                    self.custom.provider,
                    &self.provider_input,
                    disabled,
                    window,
                    cx,
                ))
                .children(override_key.map(|key| {
                    Self::warning(
                        format!(
                            "{key} overrides this provider selection. Replace or remove it under \
                             Environment variables."
                        ),
                        &c,
                    )
                })),
        );
        if kind == HarnessKind::Goose
            && effective_goose_provider(&draft.provider, &draft.environment, keys).is_none()
        {
            fields = fields.child(Self::secondary(
                "A saved GOOSE_PROVIDER override has a hidden value. Replace or remove it under \
                 Environment variables to enter the matching API key here.",
                &c,
            ));
        }

        // The provider's API key.
        if let Some(provider) = api_key(draft, keys)
            && let Some(env) = provider.key
        {
            let typed = !self.api_key_input.read(cx).value().is_empty();
            let reveal = typed.then(|| {
                IconButton::new(
                    "defaults-reveal-key",
                    if self.revealed {
                        IconName::EyeSlash
                    } else {
                        IconName::Eye
                    },
                )
                .size(IconButtonSize::Sm)
                .tooltip(if self.revealed {
                    "Hide API key"
                } else {
                    "Show API key"
                })
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.revealed = !this.revealed;
                    cx.notify();
                }))
            });
            let sign_in = if pi {
                "Pi sign-in"
            } else {
                "Goose credentials"
            };
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(field(
                        format!("{} API key", provider.label),
                        None,
                        TextField::new(&self.api_key_input)
                            .surface(self.surface())
                            .disabled(disabled)
                            .when_some(reveal, |this, reveal| this.trailing(reveal)),
                        &c,
                    ))
                    .child(Self::secondary(
                        format!(
                            "{env} is used for model lookup and every local agent without its \
                             own value, including other harnesses. Leave blank to keep a saved \
                             key or use your {sign_in}. Saved keys remain after provider \
                             changes; remove them under Environment variables. Saved values \
                             are never shown again."
                        ),
                        &c,
                    )),
            );
        }

        fields =
            fields.child(self.model_choice(draft, saved, harness.as_ref(), disabled, window, cx));

        // Default effort.
        fields = fields.child(self.choice(
            "defaults-effort",
            Menu::Effort,
            "Default effort",
            &draft.effort,
            &effort_choices(kind),
            self.custom.effort,
            &self.effort_input,
            disabled,
            window,
            cx,
        ));

        // Conversation context.
        let session_label = |policy| match policy {
            SessionPolicy::Channel => "Entire channel",
            SessionPolicy::Thread => "Each thread",
        };
        let session_items = [SessionPolicy::Channel, SessionPolicy::Thread]
            .into_iter()
            .map(|policy| {
                MenuItem::new(
                    SharedString::from(session_label(policy)),
                    session_label(policy),
                )
                .checked(draft.session_policy == policy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open = None;
                    this.change(cx, |draft| draft.session_policy = policy);
                }))
                .into_any_element()
            })
            .collect();
        fields = fields.child(field(
            "Conversation context",
            Some(
                "Share one conversation across the channel, or keep threads separate. Direct \
                 messages stay shared."
                    .into(),
            ),
            self.select(
                "defaults-session",
                Menu::Session,
                session_label(draft.session_policy).into(),
                session_items,
                disabled,
                window,
                cx,
            ),
            &c,
        ));

        fields.child(self.environment(draft, keys, disabled, cx))
    }

    fn model_choice(
        &self,
        draft: &Draft,
        saved: &AgentDefaults,
        harness: Option<&Harness>,
        disabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let keys = &saved.environment_keys;
        let kind = draft.harness;
        let pi = kind == HarnessKind::Pi;
        let models = self.lookup.models.clone().unwrap_or_default();
        let entries: Vec<&ModelOption> = models
            .iter()
            .filter(|model| {
                !pi || draft.provider.is_empty()
                    || model.id.starts_with(&format!("{}/", draft.provider))
            })
            .collect();
        let selected = if pi && !draft.provider.is_empty() && !draft.model.is_empty() {
            format!("{}/{}", draft.provider, draft.model)
        } else {
            draft.model.clone()
        };
        let query = self.search_input.read(cx).value().to_string();
        let mut choices: Choices = vec![(
            String::new(),
            default_label(
                kind,
                if build_model_applies(draft, keys, &saved.build) {
                    &saved.build.model
                } else {
                    ""
                },
            ),
        )];
        choices.extend(
            matching(&entries, &query, &selected)
                .into_iter()
                .map(|model| (model.id.clone(), model_label(model))),
        );
        let override_key =
            model_key(kind).filter(|key| environment_set(&draft.environment, keys, key));
        let removing = draft.environment.values().any(Option::is_none);
        let unavailable = harness.is_some_and(|harness| harness.command.is_none());
        let no_goose_provider = kind == HarnessKind::Goose && draft.provider.is_empty();
        let lookup_button = if self.lookup.busy {
            Button::new("defaults-cancel-models", "Cancel model lookup")
                .disabled(disabled)
                .on_click(cx.listener(|this, _, _, cx| this.cancel_lookup(cx)))
        } else {
            Button::new(
                "defaults-browse-models",
                if self.lookup.attempted {
                    "Retry models"
                } else {
                    "Browse models"
                },
            )
            .disabled(disabled || harness.is_none() || unavailable || removing || no_goose_provider)
            .on_click(cx.listener(|this, _, _, cx| this.browse(cx)))
        };
        let custom = self.custom.model;
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .when(entries.len() > VISIBLE_MODEL_LIMIT, |this| {
                this.child(field(
                    "Search models",
                    Some(
                        format!(
                            "Search all {} models by name or ID. Up to {VISIBLE_MODEL_LIMIT} \
                             matches are shown, plus the selected model.",
                            entries.len()
                        )
                        .into(),
                    ),
                    TextField::new(&self.search_input)
                        .surface(self.surface())
                        .disabled(disabled),
                    &c,
                ))
            })
            .child(self.choice(
                "defaults-model",
                Menu::Model,
                "Default model",
                &selected,
                &choices,
                custom,
                &self.model_input,
                disabled,
                window,
                cx,
            ))
            .children(override_key.map(|key| {
                Self::warning(
                    format!(
                        "{key} overrides this model selection. Replace or remove it under \
                         Environment variables."
                    ),
                    &c,
                )
            }))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(0.5))
                    .child(lookup_button.surface(self.surface()))
                    .children(
                        self.lookup
                            .status
                            .clone()
                            .map(|status| Self::secondary(status, &c)),
                    ),
            )
            .when_some(harness.filter(|_| unavailable), |this, harness| {
                this.child(Self::secondary(
                    format!(
                        "Install {} to browse its models. Custom IDs remain editable.",
                        harness.label
                    ),
                    &c,
                ))
            })
            .when(no_goose_provider, |this| {
                this.child(Self::secondary(
                    "Choose a Goose provider to browse its models.",
                    &c,
                ))
            })
            .when(
                pi && !draft.provider.is_empty() && draft.model.is_empty(),
                |this| {
                    this.child(Self::warning(
                    "Choose a model for this Pi provider, or clear Provider to use Pi defaults.",
                    &c,
                ))
                },
            )
            .when(removing, |this| {
                this.child(Self::secondary(
                    "Save environment removals before browsing models so lookup uses the \
                     updated settings.",
                    &c,
                ))
            })
    }

    fn environment(
        &self,
        draft: &Draft,
        keys: &[String],
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let rows = environment_rows(keys, &draft.environment);
        let count = rows.len();
        let list = div()
            .flex()
            .flex_col()
            .children(
                rows.into_iter()
                    .enumerate()
                    .map(|(index, (key, state, removed))| {
                        let toggle = key.clone();
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap(rems(0.5))
                            .py(rems(0.5))
                            .text_body_sm()
                            .when(index > 0, |this| {
                                this.border_t_1().border_color(c.border_standard)
                            })
                            .child(div().text_mono().child(key.clone()))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(rems(0.5))
                                    .child(div().text_color(c.text_subtle).child(state))
                                    .child(
                                        Button::new(
                                            SharedString::from(format!("defaults-env-{key}")),
                                            if removed { "Keep" } else { "Remove" },
                                        )
                                        .size(ButtonSize::Sm)
                                        .surface(self.surface())
                                        .disabled(disabled)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let saved = this.saved_keys(cx);
                                                let key = toggle.clone();
                                                this.change(cx, |draft| {
                                                    toggle_variable(
                                                        &mut draft.environment,
                                                        &saved,
                                                        &key,
                                                    )
                                                });
                                            }),
                                        ),
                                    ),
                            )
                    }),
            );
        let open = self.add_open;
        let new_key = self.name_input.read(cx).value().trim().to_string();
        let accordion = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("defaults-add-variable")
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(rems(0.75))
                    .p(rems(0.5))
                    .rounded(radius::ROW)
                    .cursor_pointer()
                    .text_body()
                    .hover(|this| this.bg(c.affordance_selected))
                    .child("Add environment variable")
                    .child(
                        Icon::new(if open {
                            IconName::CaretUp
                        } else {
                            IconName::CaretDown
                        })
                        .size(14.),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_open = !this.add_open;
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.75))
                        .p(rems(0.5))
                        .child(field(
                            "Name",
                            None,
                            TextField::new(&self.name_input)
                                .surface(self.surface())
                                .disabled(disabled),
                            &c,
                        ))
                        .child(field(
                            "Value",
                            None,
                            TextField::new(&self.value_input)
                                .surface(self.surface())
                                .disabled(disabled),
                            &c,
                        ))
                        .child(
                            div().flex().child(
                                Button::new("defaults-add-variable-button", "Add variable")
                                    .surface(self.surface())
                                    .disabled(disabled || new_key.is_empty())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_variable(window, cx)
                                    })),
                            ),
                        ),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .child(
                div()
                    .mb(rems(0.5))
                    .text_label_sm()
                    .child("Environment variables"),
            )
            .child(Self::secondary(
                "Added to every agent; an agent’s own key wins. Saved values stay on this device \
                 and are never shown again.",
                &c,
            ))
            .when(count > 0, |this| this.child(list))
            .child(accordion)
    }

    fn dirty(&self, cx: &App) -> bool {
        self.draft.is_some()
            || !self.name_input.read(cx).value().trim().is_empty()
            || !self.value_input.read(cx).value().is_empty()
    }

    fn discard(&mut self, cx: &mut Context<Self>) {
        self.reset_session();
        self.error = None;
        cx.notify();
    }

    /// Discard (once something changed) and Save defaults.
    fn actions(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let disabled = self.disabled(cx);
        let mut actions = Vec::new();
        if self.dirty(cx) {
            actions.push(
                Button::new("discard-defaults", "Discard")
                    .surface(self.surface())
                    .disabled(disabled)
                    .on_click(cx.listener(|this, _, _, cx| this.discard(cx)))
                    .into_any_element(),
            );
        }
        actions.push(
            Button::new("save-defaults", "Save defaults")
                .prominent()
                .loading(self.saving)
                .disabled(disabled || self.draft.is_none())
                .on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                .into_any_element(),
        );
        actions
    }

    /// Resets lookup state and Custom ID modes whose context changed.
    fn follow(
        &mut self,
        draft: &Draft,
        keys: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.custom_for != (draft.harness, self.session) {
            self.custom_for = (draft.harness, self.session);
            self.custom = Custom::default();
        }
        let command = self
            .harness(draft.harness, cx)
            .and_then(|harness| harness.command);
        let key = format!(
            "{:?}",
            (
                draft.harness,
                (draft.harness != HarnessKind::Pi).then_some(&draft.provider),
                command,
                &draft.environment,
                keys,
            )
        );
        if key != self.lookup.key {
            self.lookup = Lookup {
                key,
                generation: self.lookup.generation + 1,
                ..Lookup::default()
            };
            self.search_input
                .update(cx, |state, cx| state.set_value("", window, cx));
        }
    }

    fn content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(Div, Vec<AnyElement>)> {
        let saved = self.saved(cx)?.clone();
        let draft = self.current(cx)?;
        self.follow(&draft, &saved.environment_keys, window, cx);
        self.sync(&draft, &saved.environment_keys, window, cx);
        let fields = self.fields(&draft, &saved, window, cx);
        Some((fields, self.actions(cx)))
    }

    /// "Saved." and what restarted, or why the save failed.
    fn messages(&self, cx: &App) -> Vec<Div> {
        let c = cx.colors();
        self.notice
            .clone()
            .map(|notice| div().text_body_sm().child(notice))
            .into_iter()
            .chain(
                self.error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .collect()
    }
}

impl Render for DefaultsCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((fields, actions)) = self.content(window, cx) else {
            // buzz-app shows nothing until the defaults load.
            return div();
        };
        let fields = if self.in_dialog {
            fields
        } else {
            fields.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap(rems(0.75))
                    .pt(rems(0.5))
                    .children(actions),
            )
        };
        div().child(fields.children(self.messages(cx)))
    }
}

/// A remote host's agent defaults, in a dialog from its row.
pub struct DefaultsDialog {
    host: Entity<HostConnection>,
    card: Entity<DefaultsCard>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<Dismissed> for DefaultsDialog {}
impl EventEmitter<Saved> for DefaultsDialog {}

impl DefaultsDialog {
    pub fn new(host: Entity<HostConnection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let card = cx.new(|cx| DefaultsCard::new(host.clone(), true, window, cx));
        let subscriptions = [
            cx.subscribe(&card, |_, _, _: &Dismissed, cx| cx.emit(Dismissed)),
            cx.subscribe(&card, |_, _, saved: &Saved, cx| {
                cx.emit(Saved(saved.0.clone()))
            }),
        ];
        Self {
            host,
            card,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for DefaultsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.host.read(cx).label.clone();
        let loaded = self.host.read(cx).defaults.is_some();
        let actions = self.card.update(cx, |card, cx| {
            let saving = card.saving;
            (card.actions(cx), saving)
        });
        let (actions, saving) = actions;
        let body: AnyElement = if loaded {
            self.card.clone().into_any_element()
        } else {
            div()
                .text_body_sm()
                .child(if connected(&self.host, cx) {
                    "Loading agent defaults…".to_string()
                } else {
                    format!("{label} is not connected.")
                })
                .into_any_element()
        };
        // Escape closes an open select before the dialog.
        let card = self.card.downgrade();
        let escape = move |_: &buzz_ui::Cancel, _: &mut Window, cx: &mut App| {
            let closed = card
                .update(cx, |card, cx| {
                    let open = card.open.take().is_some();
                    cx.notify();
                    open
                })
                .unwrap_or(false);
            if closed {
                cx.stop_propagation();
            }
        };
        let mut dialog = Dialog::new("agent-defaults", "Agent defaults")
            .size(DialogSize::Px(640.))
            .heading_title()
            .description(format!(
                "Used when an agent on {label} starts without its own choices. Changes apply on \
                 its next start."
            ))
            .on_close(cx.listener(move |_, _, _, cx| {
                if !saving {
                    cx.emit(Dismissed)
                }
            }))
            .child(body);
        if loaded {
            for action in actions {
                dialog = dialog.footer(action);
            }
        }
        div().capture_action(escape).child(dialog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn draft(kind: HarnessKind, provider: &str) -> Draft {
        Draft {
            harness: kind,
            provider: provider.into(),
            model: String::new(),
            effort: String::new(),
            session_policy: SessionPolicy::Thread,
            environment: Environment::new(),
        }
    }

    fn model(id: &str) -> ModelOption {
        ModelOption {
            id: id.into(),
            name: id.into(),
        }
    }

    #[test]
    fn unset_labels_name_the_build_default_for_buzz_agent_only() {
        assert_eq!(
            default_label(HarnessKind::BuzzAgent, "Databricks v2"),
            "Use build default (Databricks v2)"
        );
        assert_eq!(
            default_label(HarnessKind::Goose, "anthropic"),
            "Not set (use harness default)"
        );
        assert_eq!(
            default_label(HarnessKind::BuzzAgent, ""),
            "Not set (use harness default)"
        );
        let build = BuildDefaults {
            provider: "databricks_v2".into(),
            model: "claude".into(),
            ..Default::default()
        };
        let choices = provider_choices(HarnessKind::BuzzAgent, &[], &build);
        assert_eq!(choices[0].1, "Use build default (Databricks v2)");
        let mut buzz = draft(HarnessKind::BuzzAgent, "");
        assert!(build_model_applies(&buzz, &[], &build));
        buzz.provider = "openai".into();
        assert!(!build_model_applies(&buzz, &[], &build));
        buzz.provider.clear();
        assert!(!build_model_applies(
            &buzz,
            &["DATABRICKS_MODEL".into()],
            &build
        ));
    }

    #[test]
    fn pi_lists_signed_in_providers_first() {
        let models = [model("anthropic/claude"), model("mistral/large")];
        let choices = provider_choices(HarnessKind::Pi, &models, &BuildDefaults::default());
        let labels: Vec<&str> = choices.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Not set (use harness default)",
                "Anthropic (available in Pi)",
                "mistral (available in Pi)",
                "OpenAI (API key may be needed)",
                "Google Gemini (API key may be needed)",
                "OpenRouter (API key may be needed)",
            ]
        );
    }

    #[test]
    fn efforts_are_capitalized() {
        let labels: Vec<String> = effort_choices(HarnessKind::Pi)
            .into_iter()
            .map(|(_, label)| label)
            .collect();
        assert_eq!(
            labels,
            [
                "Not set (use harness default)",
                "Off",
                "Minimal",
                "Low",
                "Medium",
                "High",
                "Extra high"
            ]
        );
    }

    #[test]
    fn saved_overrides_count_until_removed() {
        let saved = vec!["GOOSE_PROVIDER".to_string()];
        let mut env = Environment::new();
        assert!(environment_set(&env, &saved, "GOOSE_PROVIDER"));
        assert_eq!(effective_goose_provider("openai", &env, &saved), None);
        env.insert("GOOSE_PROVIDER".into(), Some("anthropic".into()));
        assert_eq!(
            effective_goose_provider("openai", &env, &saved).as_deref(),
            Some("anthropic")
        );
        env.insert("GOOSE_PROVIDER".into(), None);
        assert!(!environment_set(&env, &saved, "GOOSE_PROVIDER"));
        assert_eq!(
            effective_goose_provider("openai", &env, &saved).as_deref(),
            Some("openai")
        );
    }

    #[test]
    fn api_keys_follow_the_effective_provider() {
        let goose = draft(HarnessKind::Goose, "openai");
        assert_eq!(
            api_key(&goose, &[]).and_then(|p| p.key),
            Some("OPENAI_API_KEY")
        );
        assert!(api_key(&goose, &["GOOSE_PROVIDER".into()]).is_none());
        assert_eq!(
            api_key(&draft(HarnessKind::Pi, "google"), &[]).and_then(|p| p.key),
            Some("GEMINI_API_KEY")
        );
        assert!(api_key(&draft(HarnessKind::BuzzAgent, "openai"), &[]).is_none());
        assert!(api_key(&draft(HarnessKind::Goose, "ollama"), &[]).is_none());
    }

    #[test]
    fn a_typed_key_is_dropped_when_another_provider_takes_over() {
        let mut current = draft(HarnessKind::Goose, "openai");
        current
            .environment
            .insert("OPENAI_API_KEY".into(), Some("sk".into()));
        let next = Draft {
            provider: "anthropic".into(),
            ..current.clone()
        };
        assert!(retire_key(&current, next, &[]).environment.is_empty());
        let next = Draft {
            effort: "high".into(),
            ..current.clone()
        };
        assert_eq!(retire_key(&current, next, &[]).environment.len(), 1);
    }

    #[test]
    fn search_shows_ten_matches_and_keeps_the_selected_model() {
        let models: Vec<ModelOption> = (0..30).map(|i| model(&format!("m{i:02}"))).collect();
        let entries: Vec<&ModelOption> = models.iter().collect();
        let shown = matching(&entries, "", "m25");
        assert_eq!(shown.len(), 11);
        assert_eq!(shown[0].id, "m25");
        let shown = matching(&entries, "M1", "");
        assert_eq!(shown.len(), 10);
        assert!(shown.iter().all(|model| model.id.starts_with("m1")));
        assert_eq!(
            model_label(&ModelOption {
                id: "a".into(),
                name: "A".into()
            }),
            "A · a"
        );
    }

    #[test]
    fn variable_rows_show_what_save_does() {
        let saved = vec!["B_KEY".to_string(), "A_KEY".to_string()];
        let mut env = Environment::new();
        env.insert("C_KEY".into(), Some("v".into()));
        toggle_variable(&mut env, &saved, "A_KEY");
        let rows = environment_rows(&saved, &env);
        assert_eq!(
            rows,
            [
                ("A_KEY".to_string(), "Removed on save", true),
                ("B_KEY".to_string(), "Set", false),
                ("C_KEY".to_string(), "Set on save", false),
            ]
        );
        // Keep undoes a removal; Remove drops a new variable.
        toggle_variable(&mut env, &saved, "A_KEY");
        toggle_variable(&mut env, &saved, "C_KEY");
        assert!(env.is_empty());
    }
}
