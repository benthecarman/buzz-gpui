//! Claude Code agents, buzz-app's `src/agent-plugins/claude-code`: an
//! Agents2 type whose agents are Claude Code sessions on the host that holds
//! them, so they run on remote buzzd hosts as well as this computer. Each
//! conversation the agent is in is one continuing session; Claude answers
//! with the in-process Buzz tools. Compiled in: there is no plugin loading.

pub mod claude;
pub mod client;
#[cfg(test)]
mod e2e;
pub mod prompt;
pub mod runtime;
pub mod sessions;
pub mod setup;
pub mod tabs;

use crate::agent_plugins::process::Spawner;
use crate::agents2::{
    Agent, AgentTab, AgentType, Agents2, Delivery, Status, Trigger, TypeDefaults, register_type,
};
use crate::buzz_mcp::{BuzzEvent, Memory};
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::runtime::Runtime;
use buzz_identity::Identity;
use buzz_relay_client::session::SessionSnapshot;
use buzz_relay_client::{ChannelType, RelayHttp};
use futures::future::BoxFuture;
use gpui_kit::*;
use runtime::{ChannelInfo, ClaudeRuntime, Config, OwnerRelay, Run, Synced, Wake};
use serde_json::Value;
use setup::ClaudeSetup;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Registered as `buzz.claude-code/claude-code`, buzz-app's key.
pub const PLUGIN: &str = "buzz.claude-code";
pub const TYPE_ID: &str = "claude-code";
/// The agent type whose declared processes hosts run.
pub const PROCESS_OWNER: &str = "claude-code";
/// A turn's run returns once Claude has it, so this bounds only the
/// hand-over.
const HANDOVER: Duration = Duration::from_secs(2 * 60);

/// The runtime and each host's setup view, kept for the app's lifetime.
pub struct ClaudeCode {
    runtime: ClaudeRuntime,
    /// Claude Code's setup on each host, by host entity.
    setups: HashMap<EntityId, ClaudeSetup>,
    _subscriptions: Vec<Subscription>,
}

struct ClaudeCodeGlobal(Entity<ClaudeCode>);

impl Global for ClaudeCodeGlobal {}

/// Registers the type and starts its runtime. Call after `agents2::init`.
pub fn init(cx: &mut App) {
    if let Err(error) = register_type(PLUGIN, Arc::new(ClaudeCodeType), cx) {
        tracing::error!(%error, "could not register the Claude Code agent type");
        return;
    }
    let state = cx.new(|cx| {
        let mut subscriptions =
            vec![cx.observe_global::<AppState>(|this: &mut ClaudeCode, cx| this.bind(cx))];
        if let Some(agents) = Agents2::global(cx) {
            subscriptions.push(cx.observe(&agents, |this, _, cx| this.sync(cx)));
        }
        let mut this = ClaudeCode {
            runtime: ClaudeRuntime::new(None),
            setups: HashMap::new(),
            _subscriptions: subscriptions,
        };
        this.bind(cx);
        this.sync(cx);
        this
    });
    cx.set_global(ClaudeCodeGlobal(state));
}

fn key() -> String {
    format!("{PLUGIN}/{TYPE_ID}")
}

impl ClaudeCode {
    fn global(cx: &App) -> Option<Entity<ClaudeCode>> {
        cx.try_global::<ClaudeCodeGlobal>()
            .map(|global| global.0.clone())
    }

    /// Reads the selected community as its owner.
    fn bind(&mut self, cx: &mut Context<Self>) {
        let relay = AppState::current(cx).and_then(|community| {
            let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
                return None;
            };
            let community = community.read(cx);
            let _runtime = Runtime::handle(cx).enter();
            Some(Arc::new(AppRelay {
                http: RelayHttp::new(community.origin.clone(), identity.clone()),
                identity: identity.clone(),
                snapshot: community.session.snapshot(),
            }) as Arc<dyn OwnerRelay>)
        });
        self.runtime.set_relay(relay);
    }

    /// Agents come and go with the community; a spare session is warmed
    /// for each.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(agents) = Agents2::global(cx) else {
            return;
        };
        let agents = agents.read(cx);
        // Agents leaving view, the app signing out included, stop their
        // processes.
        if *agents.status() == Status::Loading {
            return;
        }
        let key = key();
        let synced: Vec<Synced> = agents
            .agents()
            .iter()
            .filter(|agent| agent.agent_type == key)
            .filter_map(|agent| {
                let host = agents.host_of(&agent.pubkey)?;
                Some(Synced {
                    pubkey: agent.pubkey.clone(),
                    config: agent.config.clone(),
                    spawn: host.read(cx).spawner(PROCESS_OWNER),
                })
            })
            .collect();
        let _runtime = Runtime::handle(cx).enter();
        self.runtime.sync(synced);
    }

    /// Claude Code's setup on the host that holds `pubkey`.
    fn setup_for(&mut self, pubkey: &str, cx: &mut Context<Self>) -> ClaudeSetup {
        let host: Option<Entity<HostConnection>> =
            Agents2::global(cx).and_then(|agents| agents.read(cx).host_of(pubkey));
        let Some(host) = host else {
            return ClaudeSetup::new(None);
        };
        self.setups
            .entry(host.entity_id())
            .or_insert_with(|| ClaudeSetup::new(Some(host.read(cx).spawner(PROCESS_OWNER))))
            .clone()
    }
}

struct ClaudeCodeType;

/// The Claude tab before the runtime started, which `init` makes first.
struct Unavailable;

impl Render for Unavailable {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child("Claude Code agents run only in the desktop app")
    }
}

impl AgentType for ClaudeCodeType {
    fn id(&self) -> &'static str {
        TYPE_ID
    }

    fn title(&self) -> SharedString {
        "Claude Code".into()
    }

    fn description(&self) -> Option<SharedString> {
        Some(
            "Claude Code on this computer. Each thread it is mentioned in is one continuing session."
                .into(),
        )
    }

    fn defaults(&self) -> TypeDefaults {
        TypeDefaults {
            config: Config::default().value(),
            attention: Default::default(),
        }
    }

    fn summary(&self, agent: &Agent) -> Option<String> {
        let model = Config::read(&agent.config).model;
        Some(if model.is_empty() {
            "Claude Code".into()
        } else {
            model
        })
    }

    fn tabs(&self) -> Vec<AgentTab> {
        vec![
            AgentTab {
                id: "claude".into(),
                title: "Claude".into(),
                build: Arc::new(|view, window, cx| {
                    let Some(state) = ClaudeCode::global(cx) else {
                        return cx.new(|_| Unavailable).into();
                    };
                    let pubkey = view.agent.pubkey.clone();
                    let (setup, runtime) = state.update(cx, |state, cx| {
                        (state.setup_for(&pubkey, cx), state.runtime.clone())
                    });
                    cx.new(|cx| tabs::ClaudeTab::new(pubkey, setup, runtime, window, cx))
                        .into()
                }),
            },
            AgentTab {
                id: "settings".into(),
                title: "Settings".into(),
                build: Arc::new(|view, window, cx| {
                    cx.new(|cx| tabs::SettingsTab::new(view, window, cx)).into()
                }),
            },
        ]
    }

    fn runs(&self) -> bool {
        true
    }

    fn run(&self, delivery: Delivery, cx: &mut App) -> Task<Result<(), String>> {
        let Some(state) = ClaudeCode::global(cx) else {
            return Task::ready(Err("Claude Code is not running".into()));
        };
        let host =
            Agents2::global(cx).and_then(|agents| agents.read(cx).host_of(&delivery.agent.pubkey));
        let Some(host) = host else {
            return Task::ready(Err("The agent's host is not connected".into()));
        };
        let spawn: Spawner = host.read(cx).spawner(PROCESS_OWNER);
        let wake = match delivery.trigger {
            Trigger::Mention { event } => Wake::Mention {
                event: event_of(&event),
            },
            Trigger::Watch {
                event, interest, ..
            } => Wake::Watch {
                event: event_of(&event),
                interest: interest.map(|interest| interest.instructions),
            },
            Trigger::Timer {
                slug,
                timer,
                interest,
            } => Wake::Timer {
                slug,
                prompt: timer.prompt,
                interest: interest.map(|interest| interest.instructions),
            },
        };
        let run = Run {
            wake,
            channel: delivery.channel,
            agent: Arc::new(delivery.agent),
            config: delivery.config,
            spawn,
        };
        let runtime = state.read(cx).runtime.clone();
        let handed = Runtime::handle(cx).spawn(async move { runtime.run(run).await });
        cx.background_spawn(
            async move { handed.await.map_err(|_| "Claude Code stopped".to_string()) },
        )
    }

    fn timeout(&self) -> Duration {
        HANDOVER
    }
}

fn event_of(event: &crate::agents2::attention::EventData) -> BuzzEvent {
    BuzzEvent {
        id: event.id.clone(),
        pubkey: event.pubkey.clone(),
        created_at: event.created_at,
        kind: u32::from(event.kind),
        tags: event.tags.clone(),
        content: event.content.clone(),
        sig: String::new(),
    }
}

/// The selected community, read with the owner's key: conversations for
/// context, names, and the agent's memory, which is encrypted to the owner.
struct AppRelay {
    http: RelayHttp,
    identity: Arc<Identity>,
    snapshot: tokio::sync::watch::Receiver<Arc<SessionSnapshot>>,
}

fn filters(values: Vec<Value>) -> Vec<buzz_relay_client::socket::Filter> {
    values
        .into_iter()
        .filter_map(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .collect()
}

fn profile_name(content: &str) -> Option<String> {
    let profile: Value = serde_json::from_str(content).ok()?;
    ["display_name", "name"]
        .iter()
        .filter_map(|key| profile.get(*key).and_then(Value::as_str))
        .find(|name| !name.trim().is_empty())
        .map(str::to_string)
}

impl OwnerRelay for AppRelay {
    fn channel(&self, id: &str) -> Option<ChannelInfo> {
        let snapshot = self.snapshot.borrow().clone();
        snapshot
            .channels
            .iter()
            .find(|channel| channel.id == id)
            .map(|channel| ChannelInfo {
                name: Some(channel.name.clone()),
                dm: channel.channel_type == Some(ChannelType::Dm),
                members: channel.members.clone(),
            })
    }

    fn read(&self, values: Vec<Value>) -> BoxFuture<'static, Result<Vec<BuzzEvent>, String>> {
        let http = self.http.clone();
        Box::pin(async move {
            let events = http
                .query(&filters(values))
                .await
                .map_err(|error| error.to_string())?;
            Ok(events
                .iter()
                .filter_map(|event| {
                    serde_json::to_value(event)
                        .ok()
                        .and_then(|value| serde_json::from_value(value).ok())
                })
                .collect())
        })
    }

    fn names(
        &self,
        pubkeys: Vec<String>,
        _members: Vec<String>,
    ) -> BoxFuture<'static, HashMap<String, String>> {
        let profiles = self.snapshot.borrow().profiles.clone();
        let mut names = HashMap::new();
        let mut missing = Vec::new();
        for pubkey in pubkeys {
            match profiles.get(&pubkey) {
                Some(profile) if !profile.name_is_fallback => {
                    names.insert(pubkey, profile.name.clone());
                }
                _ => missing.push(pubkey),
            }
        }
        let http = self.http.clone();
        Box::pin(async move {
            if missing.is_empty() {
                return names;
            }
            let filter =
                serde_json::json!({ "kinds": [0], "authors": missing, "limit": missing.len() });
            if let Ok(events) = http.query(&filters(vec![filter])).await {
                let mut newest: HashMap<String, (u64, String)> = HashMap::new();
                for event in events {
                    let Some(name) = profile_name(&event.content) else {
                        continue;
                    };
                    let (pubkey, at) = (event.pubkey.to_hex(), event.created_at.as_secs());
                    if newest.get(&pubkey).is_none_or(|(seen, _)| *seen < at) {
                        newest.insert(pubkey, (at, name));
                    }
                }
                names.extend(newest.into_iter().map(|(pubkey, (_, name))| (pubkey, name)));
            }
            names
        })
    }

    fn memories(&self, agent: &str) -> BoxFuture<'static, Result<Vec<Memory>, String>> {
        let (http, identity, agent) = (self.http.clone(), self.identity.clone(), agent.to_string());
        Box::pin(async move {
            let query = buzz_desktop::relay::agent::memory_query(&identity.pubkey_hex(), &agent)?;
            let values: Vec<Value> =
                serde_json::from_str(&query).map_err(|_| "Invalid memory target")?;
            let records = http
                .query_raw(&filters(values))
                .await
                .map_err(|error| error.to_string())?;
            let records: Vec<Option<Value>> = records.into_iter().map(Some).collect();
            let listing = identity.decode_agent_memories(&agent, &records)?;
            Ok(listing
                .get("entries")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            Some(Memory {
                                slug: entry.get("slug")?.as_str()?.to_string(),
                                body: entry.get("body")?.as_str()?.to_string(),
                                created_at: entry.get("createdAt")?.as_u64()?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default())
        })
    }
}
