//! Claude Code agents, buzz-app's `src/agent-plugins/claude-code`: an
//! Agents2 type whose agents are Claude Code sessions on the host that holds
//! them, so they run on remote buzzd hosts as well as this computer. Each
//! conversation the agent is in is one continuing session; Claude answers
//! with the in-process Buzz tools. Compiled in: there is no plugin loading.

pub mod claude;
#[cfg(debug_assertions)]
mod dev;
#[cfg(test)]
mod e2e;
pub mod prompt;
pub mod runtime;
pub mod sessions;
pub mod setup;
pub mod tabs;

use crate::agent_plugins::owner_relay;
use crate::agent_plugins::process::Spawner;
use crate::agents2::{
    Agent, AgentTab, AgentType, Agents2, Delivery, Status, Trigger, TypeDefaults, register_type,
};
use crate::buzz_mcp::BuzzEvent;
use crate::hosts::HostConnection;
use crate::runtime::Runtime;
use gpui_kit::*;
use runtime::{ClaudeRuntime, Config, Run, Synced, Wake};
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
        let mut subscriptions = Vec::new();
        if let Some(agents) = Agents2::global(cx) {
            subscriptions.push(cx.observe(&agents, |this: &mut ClaudeCode, _, cx| this.sync(cx)));
        }
        subscriptions.push(
            cx.observe_global::<crate::plugins::Plugins>(|this: &mut ClaudeCode, cx| this.sync(cx)),
        );
        // Quitting stops every session's process, as buzz-app does on exit,
        // and waits for them as long as GPUI lets quit handlers run.
        subscriptions.push(cx.on_app_quit(|this: &mut ClaudeCode, cx| {
            let _runtime = Runtime::handle(cx).enter();
            this.runtime.dispose_and_wait()
        }));
        let mut this = ClaudeCode {
            runtime: ClaudeRuntime::new(None),
            setups: HashMap::new(),
            _subscriptions: subscriptions,
        };
        this.sync(cx);
        this
    });
    cx.set_global(ClaudeCodeGlobal(state));
    #[cfg(debug_assertions)]
    dev::init(cx);
}

fn key() -> String {
    format!("{PLUGIN}/{TYPE_ID}")
}

impl ClaudeCode {
    fn global(cx: &App) -> Option<Entity<ClaudeCode>> {
        cx.try_global::<ClaudeCodeGlobal>()
            .map(|global| global.0.clone())
    }

    /// One spare session waits for whichever running agent needs it next.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(agents) = Agents2::global(cx) else {
            return;
        };
        // Turning the plugin off stops every session, as unloading it does
        // in buzz-app.
        if !crate::plugins::enabled(PLUGIN) {
            let _runtime = Runtime::handle(cx).enter();
            self.runtime.dispose();
            return;
        }
        let agents = agents.read(cx);
        // Agents that stop running, the app signing out included, stop
        // their processes; switching communities does not.
        if *agents.status() == Status::Loading {
            return;
        }
        let key = key();
        let synced: Vec<Synced> = agents
            .running()
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
        let relays = owner_relay::running(agents, cx)
            .into_iter()
            .map(|(pubkey, (_, relay))| (pubkey, relay))
            .collect();
        let _runtime = Runtime::handle(cx).enter();
        self.runtime.set_relays(relays);
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
        tracing::debug!(agent = %delivery.agent.pubkey, "Claude Code delivery");
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
