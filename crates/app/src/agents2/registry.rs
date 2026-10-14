//! The agent-type registry (buzz-app's `agents2.register`).
//!
//! An agent type is compiled in: it implements [`AgentType`] and registers
//! itself once at startup, from its module's `init`, with
//! [`register_type`]:
//!
//! ```ignore
//! pub fn init(cx: &mut App) {
//!     agents2::register_type("buzz.claude-code", Arc::new(ClaudeCode), cx)
//!         .expect("a valid agent type");
//! }
//! ```
//!
//! Its key, `<plugin>/<id>`, is saved with every agent made from it and
//! never changes. The app owns each agent's name, key and attention; the
//! type owns its config (one JSON value) and decides what a run does.

// The type API: agent types compiled in elsewhere use what the app does not.
#![allow(dead_code)]

use super::Agent;
use super::attention::AttentionValue;
use super::runtime::Delivery;
use gpui_kit::{AnyElement, AnyView, App, Global, SharedString, Task, Window};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// The longest a run may take.
pub const TIMEOUT_LIMIT: Duration = Duration::from_secs(30 * 60);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A new agent's starting settings.
#[derive(Clone, Debug, Default)]
pub struct TypeDefaults {
    pub config: Value,
    /// Attention objects by slug, such as an Interest and its watches.
    pub attention: BTreeMap<String, AttentionValue>,
}

/// What a type's settings tab is built from. A type writes only its own
/// config; the app owns the name and attention.
#[derive(Clone)]
pub struct AgentView {
    pub agent: Agent,
}

impl AgentView {
    /// Replaces the type's whole config. Change one field by editing a copy
    /// of `agent.config`, or tabs that each save a part erase each other's
    /// fields.
    pub fn save(&self, config: Value, cx: &mut App) -> Task<Result<(), String>> {
        super::Agents2::save_config(&self.agent.pubkey, config, cx)
    }
}

/// Builds a tab's view. Called when the tab is shown; the view may read
/// the current agent from [`super::Agents2`] to follow later changes.
pub type BuildTab = Arc<dyn Fn(AgentView, &mut Window, &mut App) -> AnyView>;

/// One of a type's settings tabs, shown before the app's Attention tab.
#[derive(Clone)]
pub struct AgentTab {
    pub id: SharedString,
    pub title: SharedString,
    pub build: BuildTab,
}

/// An effort level a model offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effort {
    pub id: String,
    pub label: String,
}

/// A model with its effort levels, lowest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    pub label: String,
    pub efforts: Vec<Effort>,
    /// The effort a run on it uses without a pick, when known.
    pub effort: Option<String>,
}

/// The models an owner can pick from, and the id of the one used without a
/// pick.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Models {
    pub current: String,
    pub models: Vec<Model>,
}

/// A kind of Agents2 agent. Everything but `id`, `title` and `defaults`
/// has a default.
pub trait AgentType: 'static {
    /// Lowercase letters, digits, `.`, `_` and `-`, starting with a letter
    /// or digit. Unique within its plugin.
    fn id(&self) -> &'static str;
    fn title(&self) -> SharedString;
    /// Shown under the title in the New agent form.
    fn description(&self) -> Option<SharedString> {
        None
    }
    /// A new agent's starting config and attention objects.
    fn defaults(&self) -> TypeDefaults;
    /// One short line under the agent's name, such as its model. `None` or
    /// blank shows the title.
    fn summary(&self, _agent: &Agent) -> Option<String> {
        None
    }
    /// A read-only glance shown when the agent is selected in the grid.
    fn peek(&self, _agent: &Agent, _window: &mut Window, _cx: &mut App) -> Option<AnyElement> {
        None
    }
    /// The type's settings tabs, in order. The app adds Attention after
    /// them.
    fn tabs(&self) -> Vec<AgentTab> {
        Vec::new()
    }
    /// Whether [`Self::run`] does anything. Agents of a type that does not
    /// run drop what wakes them.
    fn runs(&self) -> bool {
        false
    }
    /// Models and effort levels the owner can pick for the next run while
    /// mentioning the agent. Discovery may take a while but must not start
    /// a turn. The picks reach that run as [`Delivery::model`] and
    /// [`Delivery::effort`]. `None` offers no choice.
    fn models(&self, _agent: &Agent, _cx: &mut App) -> Option<Task<Result<Models, String>>> {
        None
    }
    /// Handles one wake. Runs of one agent are serialized: the next starts
    /// when this task ends or its deadline passes, whichever is first.
    fn run(&self, _delivery: Delivery, _cx: &mut App) -> Task<Result<(), String>> {
        Task::ready(Ok(()))
    }
    /// The per-run deadline, from 1 ms to 30 minutes.
    fn timeout(&self) -> Duration {
        DEFAULT_TIMEOUT
    }
    /// Whether [`Self::adopt`] gives a brought-over agent anything it kept
    /// in an earlier agent system.
    fn adopts(&self) -> bool {
        false
    }
    /// A brought-over agent's config: `config`, the type's defaults, with
    /// what of `carry` the type has a place for.
    fn adopt(&self, _carry: &buzz_host::api::Carry, config: Value) -> Value {
        config
    }
}

/// A registered type and its key.
#[derive(Clone)]
pub struct RegisteredType {
    /// `<plugin>/<id>`, saved with each agent.
    pub key: SharedString,
    pub kind: Arc<dyn AgentType>,
}

impl RegisteredType {
    /// The type's summary of `agent`; its title when it has none.
    pub fn summary_of(&self, agent: &Agent) -> String {
        self.kind
            .summary(agent)
            .map(|summary| summary.trim().to_owned())
            .filter(|summary| !summary.is_empty())
            .unwrap_or_else(|| self.kind.title().to_string())
    }
}

/// The registered types, in registration order.
#[derive(Default)]
pub struct AgentTypes(pub Vec<RegisteredType>);

impl Global for AgentTypes {}

fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// Checks a type before it is registered.
pub fn validate(plugin: &str, kind: &dyn AgentType) -> Result<(), String> {
    if !valid_name(plugin) || !valid_name(kind.id()) || kind.title().is_empty() {
        return Err("Agent types need an id, a title and defaults".into());
    }
    let timeout = kind.timeout();
    if timeout < Duration::from_millis(1) || timeout > TIMEOUT_LIMIT {
        return Err(format!(
            "Agent type timeoutMs must be a whole number from 1 to {}",
            TIMEOUT_LIMIT.as_millis()
        ));
    }
    Ok(())
}

/// Registers `kind` as `<plugin>/<id>`, replacing a type registered under
/// the same key. Agents of a replaced type abort their run in flight and
/// continue with the new one.
pub fn register_type(plugin: &str, kind: Arc<dyn AgentType>, cx: &mut App) -> Result<(), String> {
    validate(plugin, kind.as_ref())?;
    let key: SharedString = format!("{plugin}/{}", kind.id()).into();
    let types = &mut cx.default_global::<AgentTypes>().0;
    types.retain(|existing| existing.key != key);
    types.push(RegisteredType { key, kind });
    Ok(())
}

/// Every registered type whose plugin is on. A plugin turned off takes its
/// types with it, as unloading it does in buzz-app: its agents show their
/// type as unavailable and stop running until it is back.
pub fn types(cx: &App) -> Vec<RegisteredType> {
    cx.try_global::<AgentTypes>()
        .map(|types| {
            types
                .0
                .iter()
                .filter(|registered| crate::plugins::enabled_key(&registered.key))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The registered type with this key, while its plugin is on.
pub fn type_of(key: &str, cx: &App) -> Option<RegisteredType> {
    cx.try_global::<AgentTypes>()?
        .0
        .iter()
        .find(|registered| registered.key == key)
        .filter(|registered| crate::plugins::enabled_key(&registered.key))
        .cloned()
}
