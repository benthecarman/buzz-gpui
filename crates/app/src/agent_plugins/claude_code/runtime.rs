//! Turns Agents2 deliveries into Claude turns, a port of buzz-app's
//! `runtime.ts`. Each agent of this type has its own sessions; a delivery
//! becomes a prompt in the harness format, sent to the session for its
//! conversation. The run returns once the turn is handed over, so one agent
//! works in several conversations at once. Claude acts in Buzz with the
//! in-process Buzz tools, signed by the agent's host-held key; the runtime
//! only reports a turn that fails.

use super::claude::{Launch, Settled, ToolServer, TurnPick};
use super::prompt::{
    CONTEXT_LIMIT, Channel, Scope, Thread, steer_prompt, system_prompt, timer_prompt, turn_prompt,
};
use super::sessions::{
    AgentSessions, DeliverOptions, FileSessions, SessionStore, SessionView, SessionsOptions, Spare,
    Where,
};
use crate::agent_plugins::owner_relay::OwnerRelay;
use crate::agent_plugins::process::Spawner;
use crate::agents2::{Effort, Model, Models};
use crate::buzz_mcp::app_client::{self, AgentActions, AppClient};
use crate::buzz_mcp::{BuzzClient, BuzzEvent, Context, Memory, Template, rpc, tools::hex};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The type's config, as buzz-app saves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// A Claude model or alias; empty uses Claude's own default.
    pub model: String,
    pub instructions: String,
    /// The agent's working directory, absolute or `~/…`.
    pub workspace: String,
    /// `thread` or `channel`: whether each thread is its own session, or
    /// the whole channel is one.
    pub scope: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: String::new(),
            instructions: String::new(),
            workspace: "~/.buzz".into(),
            scope: "thread".into(),
        }
    }
}

impl Config {
    /// The saved config over the defaults, field by field.
    pub fn read(value: &Value) -> Self {
        let mut config = Self::default();
        let field = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_string);
        if let Some(model) = field("model") {
            config.model = model;
        }
        if let Some(instructions) = field("instructions") {
            config.instructions = instructions;
        }
        if let Some(workspace) = field("workspace") {
            config.workspace = workspace;
        }
        if let Some(scope) = field("scope") {
            config.scope = scope;
        }
        config
    }

    pub fn value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    pub fn scope(&self) -> Scope {
        if self.scope == "channel" {
            Scope::Channel
        } else {
            Scope::Thread
        }
    }

    /// What a running process fixed when it started.
    fn fingerprint(&self) -> String {
        json!([self.model, self.workspace, self.instructions, self.scope]).to_string()
    }

    /// Where an agent's processes run, on `spawn`'s host, and with which
    /// model.
    fn at(&self, spawn: &Spawner) -> Where {
        let model = self.model.trim();
        Where {
            spawn: spawn.clone(),
            cwd: self.workspace.clone(),
            model: (!model.is_empty()).then(|| model.to_string()),
        }
    }
}

/// How long a read of the agent's core memory is reused.
const MEMORY_TTL: Duration = Duration::from_secs(5 * 60);
/// How long a failed read stands before sessions try again: long enough
/// that a relay that is down doesn't slow every start by the timeout.
const FAILED_MEMORY_TTL: Duration = Duration::from_secs(30);
const MEMORY_TIMEOUT: Duration = Duration::from_secs(5);
const NAMES_TIMEOUT: Duration = Duration::from_secs(2);
const CHAT: [u32; 2] = [9, 40002];

/// What a session can switch to between turns. Claude runs `max` as
/// `xhigh` in a stream-json session, so it is not offered.
const EFFORTS: [(&str, &str); 4] = [
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
];

fn efforts(supported: Option<&[String]>) -> Vec<Effort> {
    EFFORTS
        .iter()
        .filter(|(id, _)| supported.is_none_or(|supported| supported.iter().any(|s| s == id)))
        .map(|(id, label)| Effort {
            id: (*id).into(),
            label: (*label).into(),
        })
        .collect()
}

/// `claude-opus-5-5` reads as `Opus 5.5`, and an alias such as `opus` as
/// `Opus`. Anything else is shown as it is.
pub fn model_name(model: &str) -> String {
    static ID: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?(?:-\d{8})?(?:\[.*\])?$")
            .expect("model id pattern")
    });
    let capitalized = |family: &str| {
        let mut chars = family.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect::<String>())
            .unwrap_or_default()
    };
    if let Some(id) = ID.captures(model) {
        let minor = id
            .get(3)
            .map_or(String::new(), |minor| format!(".{}", minor.as_str()));
        return format!("{} {}{minor}", capitalized(&id[1]), &id[2]);
    }
    if !model.is_empty() && model.chars().all(|c| c.is_ascii_lowercase()) {
        return capitalized(model);
    }
    model.to_string()
}

/// Why an agent woke, as this type needs it.
#[derive(Clone, Debug)]
pub enum Wake {
    Mention {
        event: BuzzEvent,
    },
    Watch {
        event: BuzzEvent,
        /// The matched watch's Interest instructions.
        interest: Option<String>,
    },
    Timer {
        slug: String,
        prompt: String,
        interest: Option<String>,
    },
}

/// One delivery.
pub struct Run {
    pub wake: Wake,
    /// The live route's channel.
    pub channel: Option<String>,
    pub agent: Arc<dyn AgentActions>,
    pub config: Value,
    /// Where the agent's processes run: the host that holds it.
    pub spawn: Spawner,
    /// The model and effort the owner picked for this turn.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Told when the turn this run hands over settles.
    pub working: crate::agent_plugins::HoldUntil,
}

/// One agent, as `sync` describes it.
pub struct Synced {
    pub pubkey: String,
    /// Its name, for its prompt; a rename restarts idle sessions.
    pub name: String,
    pub config: Value,
    pub spawn: Spawner,
}

/// Where an agent's saved sessions are kept, by agent pubkey.
pub type StoreFn = Arc<dyn Fn(&str) -> Arc<dyn SessionStore> + Send + Sync>;

type MemoryRead = Shared<BoxFuture<'static, Result<Vec<Memory>, String>>>;

struct EntryState {
    config: Config,
    /// The agent's name, for its prompt; a rename restarts idle sessions.
    name: Option<String>,
    /// The core memory and when it was read: `None` when it could not be.
    memory: Option<(Option<Option<String>>, Instant)>,
    /// The agent's handle from its latest delivery, for the tools to act
    /// with.
    handle: Option<Arc<dyn AgentActions>>,
    /// Where each conversation's tools default to: its latest turn's
    /// thread.
    contexts: HashMap<String, Context>,
}

struct Entry {
    sessions: AgentSessions,
    state: Arc<Mutex<EntryState>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    agents: Mutex<HashMap<String, Arc<Entry>>>,
    reading: Mutex<HashMap<String, MemoryRead>>,
    /// The owner's connection to each agent's own community, by agent
    /// pubkey.
    relays: Mutex<HashMap<String, Arc<dyn OwnerRelay>>>,
    /// One process started ahead of need, for any agent's next new
    /// conversation.
    spare: Spare,
    changes: watch::Sender<u64>,
    /// Where each agent's saved sessions are kept.
    store: StoreFn,
}

/// Every Claude agent's sessions. Cheap to clone.
#[derive(Clone)]
pub struct ClaudeRuntime {
    inner: Arc<Inner>,
}

impl ClaudeRuntime {
    /// Saved sessions go to `path`, a JSON file shared by all agents.
    pub fn new(path: Option<PathBuf>) -> Self {
        Self::with_store(Arc::new(move |agent: &str| -> Arc<dyn SessionStore> {
            let path = path
                .clone()
                .or_else(FileSessions::default_path)
                .unwrap_or_else(|| PathBuf::from("claude-code-sessions.json"));
            Arc::new(FileSessions::new(path, agent))
        }))
    }

    pub fn with_store(store: StoreFn) -> Self {
        Self {
            inner: Arc::new(Inner {
                agents: Mutex::default(),
                reading: Mutex::default(),
                relays: Mutex::default(),
                spare: Spare::new(Arc::new(|_, message: Value| {
                    async move { Ok(rpc::respond(None, &Context::default(), &message).await) }
                        .boxed()
                })),
                changes: watch::Sender::new(0),
                store,
            }),
        }
    }

    /// Each agent's own community, as its owner reads it, by agent pubkey.
    pub fn set_relays(&self, relays: HashMap<String, Arc<dyn OwnerRelay>>) {
        *locked(&self.inner.relays) = relays;
    }
    fn relay(&self, pubkey: &str) -> Option<Arc<dyn OwnerRelay>> {
        locked(&self.inner.relays).get(pubkey).cloned()
    }

    /// Bumps whenever a session starts, stops or settles.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }
    fn notify(&self) {
        self.inner.changes.send_modify(|count| *count += 1);
    }

    /// Brings the runtime in line with this type's agents: changed settings
    /// restart idle sessions, removed agents stop, and a spare waits while
    /// any agent is here. Must run inside tokio.
    pub fn sync(&self, agents: Vec<Synced>) {
        let current: HashSet<&str> = agents.iter().map(|agent| agent.pubkey.as_str()).collect();
        // Deleted, signed out or its community disconnected: either way its
        // processes stop. Saved sessions stay so it resumes when it is back.
        let gone: Vec<Arc<Entry>> = {
            let mut entries = locked(&self.inner.agents);
            let gone = entries
                .iter()
                .filter(|(pubkey, _)| !current.contains(pubkey.as_str()))
                .map(|(_, entry)| entry.clone())
                .collect();
            entries.retain(|pubkey, _| current.contains(pubkey.as_str()));
            gone
        };
        for entry in gone {
            entry.sessions.dispose();
        }
        let mut wheres = Vec::new();
        for agent in agents {
            let next = Config::read(&agent.config);
            wheres.push(next.at(&agent.spawn));
            let existing = locked(&self.inner.agents).get(&agent.pubkey).cloned();
            match existing {
                None => {
                    let entry = self.entry(&agent.pubkey, next, agent.spawn);
                    locked(&entry.state).name = Some(agent.name);
                }
                Some(entry) => {
                    let changed = {
                        let mut state = locked(&entry.state);
                        let changed =
                            state.config != next || state.name.as_ref() != Some(&agent.name);
                        state.config = next;
                        state.name = Some(agent.name);
                        changed
                    };
                    if changed {
                        entry.sessions.reconfigure();
                    }
                }
            }
        }
        self.inner.spare.warm(wheres);
        self.notify();
    }

    /// The models the agent's next turn can run on, with their effort
    /// levels. Without a pick it uses its own: the configured one, or else
    /// the one its sessions report. `default` duplicates a listed model, so
    /// it is not offered.
    pub async fn models(&self, pubkey: &str) -> Result<Models, String> {
        let entry = locked(&self.inner.agents).get(pubkey).cloned();
        let configured = entry
            .as_ref()
            .map(|entry| locked(&entry.state).config.model.trim().to_string())
            .unwrap_or_default();
        let metadata = match &entry {
            Some(entry) => entry.sessions.metadata().await?,
            None => Default::default(),
        };
        // A turn without a pick keeps the session's effort on any model.
        let resting = |efforts: &[Effort]| {
            metadata
                .effort
                .clone()
                .filter(|effort| efforts.iter().any(|level| level.id == *effort))
        };
        let own = if configured.is_empty() {
            metadata.model.clone().unwrap_or_default()
        } else {
            configured
        };
        let offered: Vec<_> = metadata
            .models
            .iter()
            .filter(|model| model.value != "default")
            .collect();
        let models: Vec<Model> = offered
            .iter()
            .map(|model| {
                let efforts = if model.supports_effort == Some(false) {
                    Vec::new()
                } else {
                    efforts(model.supported_effort_levels.as_deref())
                };
                Model {
                    id: model.value.clone(),
                    label: model_name(model.resolved_model.as_deref().unwrap_or(&model.value)),
                    effort: resting(&efforts),
                    efforts,
                }
            })
            .collect();
        let current = offered
            .iter()
            .find(|model| own == model.value || Some(&own) == model.resolved_model.as_ref())
            .map_or(own.clone(), |model| model.value.clone());
        // An own model Claude does not list is still the one it runs.
        if models.iter().any(|model| model.id == current) {
            return Ok(Models { current, models });
        }
        let all = efforts(None);
        let mut listed = vec![Model {
            id: current.clone(),
            label: if current.is_empty() {
                "Claude".into()
            } else {
                model_name(&current)
            },
            effort: resting(&all),
            efforts: all,
        }];
        listed.extend(models);
        Ok(Models {
            current,
            models: listed,
        })
    }

    /// Conversations each agent has running, for its status view.
    pub fn sessions(&self, pubkey: &str) -> Vec<SessionView> {
        locked(&self.inner.agents)
            .get(pubkey)
            .map(|entry| entry.sessions.snapshot())
            .unwrap_or_default()
    }

    /// [`Self::dispose`], resolving once the processes it stops have
    /// exited.
    pub fn dispose_and_wait(&self) -> impl std::future::Future<Output = ()> + Send + use<> {
        let spare = self.inner.spare.process();
        let processes: Vec<_> = locked(&self.inner.agents)
            .values()
            .flat_map(|entry| entry.sessions.processes())
            .collect();
        self.dispose();
        async move {
            let spare = async {
                if let Some(spare) = spare
                    && let Some(process) = spare.await
                {
                    process.exited().await;
                }
            };
            let sessions = futures::future::join_all(processes.iter().map(|p| p.exited()));
            futures::join!(spare, sessions);
        }
    }

    pub fn dispose(&self) {
        self.inner.spare.dispose();
        let entries: Vec<Arc<Entry>> = locked(&self.inner.agents)
            .drain()
            .map(|(_, entry)| entry)
            .collect();
        for entry in entries {
            entry.sessions.dispose();
        }
    }

    /// Hands one delivery to its conversation's session. Resolves once it
    /// is handed over, not when Claude has answered.
    pub async fn run(&self, run: Run) {
        let settings = Config::read(&run.config);
        let pubkey = run.agent.pubkey();
        // Claude runs commands as you; mentions need owner or delegated
        // authority.
        let owner = run.agent.owner();
        let trusted = match &run.wake {
            Wake::Mention { event } => {
                let relay = self.relay(&pubkey);
                crate::agent_plugins::from_owner_or_workflow(event, &owner, &pubkey, relay.as_ref())
                    .await
            }
            Wake::Watch { event, .. } => crate::agent_plugins::from_owner(event, &owner),
            Wake::Timer { .. } => true,
        };
        if !trusted {
            return;
        }
        let entry = self.entry(&pubkey, settings.clone(), run.spawn.clone());
        locked(&entry.state).handle = Some(run.agent.clone());
        // Read while the turn's prompt is built, not after: a new session's
        // prompt needs it, and a spare is only faster if nothing else is
        // awaited.
        {
            let (this, pubkey, state) = (self.clone(), pubkey.clone(), entry.state.clone());
            tokio::spawn(async move { this.memory(&pubkey, &state).await });
        }
        let (event, label, interest) = match run.wake {
            Wake::Timer {
                slug,
                prompt,
                interest,
            } => {
                let done = entry
                    .sessions
                    .deliver(
                        format!("timer/{slug}"),
                        timer_prompt(&slug, &prompt, interest.as_deref()),
                        0,
                        DeliverOptions::default(),
                    )
                    .shared();
                (run.working)(done.clone().map(drop).boxed());
                self.watch(done.boxed(), None);
                return;
            }
            Wake::Mention { event } => (event, "@mention", None),
            Wake::Watch { event, interest } => (event, "watch", interest),
        };
        let Some(channel) = run.channel.or_else(|| tag(&event, "h")) else {
            return;
        };
        let (key, text) = self
            .prompt(
                &entry,
                &pubkey,
                &event,
                &channel,
                false,
                label,
                interest.as_deref(),
            )
            .await;
        // A conversation's tools default to its thread; a session shared by
        // several threads is told which to reply in instead.
        let root = thread_reference(&event).map_or(event.id.clone(), |thread| thread.root_id);
        locked(&entry.state).contexts.insert(
            key.clone(),
            Context {
                channel: Some(channel.clone()),
                root: (key != channel).then_some(root),
            },
        );
        let fresh = {
            let (this, entry, event, channel) =
                (self.clone(), entry.clone(), event.clone(), channel.clone());
            let interest = interest.clone();
            async move {
                this.prompt(
                    &entry,
                    &pubkey,
                    &event,
                    &channel,
                    true,
                    label,
                    interest.as_deref(),
                )
                .await
                .1
            }
            .boxed()
        };
        let done = entry.sessions.deliver(
            key,
            text.clone(),
            event.created_at,
            DeliverOptions {
                steer: Some(steer_prompt(&text)),
                fresh: Some(fresh),
                pick: TurnPick {
                    model: run.model,
                    effort: run.effort,
                },
            },
        );
        // The turn outlives `run`, which the host is told so it shows it as
        // working.
        let done = done.shared();
        (run.working)(done.clone().map(drop).boxed());
        self.notify();
        self.watch(done.boxed(), Some((run.agent, channel, event)));
    }

    /// Notes when a turn settles, and says in the thread when it failed.
    fn watch(
        &self,
        done: BoxFuture<'static, Settled>,
        report: Option<(Arc<dyn AgentActions>, String, BuzzEvent)>,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            let settled = done.await;
            if let (false, Some((agent, channel, event))) = (settled.ok, report) {
                let error = settled.error.unwrap_or_default();
                if let Err(failure) = Self::report(agent.as_ref(), &channel, &event, &error).await {
                    tracing::warn!(%failure, "Claude could not report a failed turn");
                }
            }
            this.notify();
        });
    }

    /// Says in the thread that a turn failed, so nobody waits on it. Tells
    /// the owner even when a sibling asked.
    async fn report(
        agent: &dyn AgentActions,
        channel: &str,
        event: &BuzzEvent,
        error: &str,
    ) -> Result<(), String> {
        let tags = crate::agent_plugins::reply_tags(channel, event, &agent.owner());
        let error: String = error.chars().take(500).collect();
        agent
            .publish(Template {
                kind: 9,
                content: format!("⚠️ I couldn't finish that: {error}"),
                tags,
            })
            .await
            .map(drop)
    }

    /// Where the event's turn goes, and its prompt. `fresh` is for a
    /// session that has been shown nothing yet.
    #[allow(clippy::too_many_arguments)]
    async fn prompt(
        &self,
        entry: &Entry,
        own: &str,
        event: &BuzzEvent,
        channel_id: &str,
        fresh: bool,
        label: &str,
        interest: Option<&str>,
    ) -> (String, String) {
        let relay = self.relay(own);
        let summary = relay.as_ref().and_then(|relay| relay.channel(channel_id));
        let dm = summary.as_ref().is_some_and(|summary| summary.dm);
        let thread = thread_reference(event);
        let root_id = thread
            .as_ref()
            .map_or(event.id.clone(), |thread| thread.root_id.clone());
        let scope = locked(&entry.state).config.scope();
        let key = if dm || scope == Scope::Channel {
            channel_id.to_string()
        } else {
            format!("{channel_id}/{root_id}")
        };
        let seen = if fresh { 0 } else { entry.sessions.seen(&key) };
        let mut earlier: Vec<BuzzEvent> = Vec::new();
        if let Some(relay) = &relay
            && (thread.is_some() || dm)
        {
            let filters = if thread.is_some() {
                vec![
                    json!({ "ids": [root_id], "limit": 1 }),
                    json!({ "kinds": CHAT, "#h": [channel_id], "#e": [root_id], "limit": 200 }),
                ]
            } else {
                vec![json!({ "kinds": CHAT, "#h": [channel_id], "limit": CONTEXT_LIMIT + 1 })]
            };
            match tokio::time::timeout(NAMES_TIMEOUT * 2, relay.read(filters)).await {
                Ok(Ok(events)) => {
                    let mut ids = HashSet::new();
                    earlier = events
                        .into_iter()
                        .filter(|item| {
                            CHAT.contains(&item.kind)
                                && item.id != event.id
                                && item.created_at <= event.created_at
                                && ids.insert(item.id.clone())
                        })
                        .collect();
                    earlier.sort_by_key(|item| item.created_at);
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "Claude could not read the conversation")
                }
                Err(_) => tracing::warn!("Claude timed out reading the conversation"),
            }
        }
        // A session already has its own replies; a new one is shown them too.
        let unseen: Vec<BuzzEvent> = earlier
            .iter()
            .filter(|item| item.created_at > seen && (seen == 0 || item.pubkey != own))
            .cloned()
            .collect();
        let mut pubkeys: Vec<String> = Vec::new();
        for key in std::iter::once(event.pubkey.clone())
            .chain(unseen.iter().map(|item| item.pubkey.clone()))
            .chain(
                event
                    .tags
                    .iter()
                    .filter(|tag| tag.first().map(String::as_str) == Some("p"))
                    .filter_map(|tag| tag.get(1).cloned()),
            )
        {
            if !key.is_empty() && !pubkeys.contains(&key) {
                pubkeys.push(key);
            }
        }
        let members = summary
            .as_ref()
            .map(|summary| summary.members.clone())
            .unwrap_or_default();
        let names = match &relay {
            Some(relay) => tokio::time::timeout(NAMES_TIMEOUT, relay.names(pubkeys, members))
                .await
                .unwrap_or_default(),
            None => HashMap::new(),
        };
        let channel = Channel {
            id: channel_id.to_string(),
            name: summary
                .and_then(|summary| summary.name)
                .filter(|name| !name.is_empty()),
            dm,
        };
        let name = |pubkey: &str| names.get(pubkey).cloned();
        let text = turn_prompt(&super::prompt::TurnInput {
            event,
            channel: &channel,
            scope,
            thread: thread.as_ref(),
            context: &unseen,
            total: earlier.len() + 1,
            label,
            interest,
            name: &name,
        });
        (key, text)
    }

    fn entry(&self, pubkey: &str, initial: Config, spawn: Spawner) -> Arc<Entry> {
        let mut agents = locked(&self.inner.agents);
        if let Some(existing) = agents.get(pubkey) {
            return existing.clone();
        }
        let state = Arc::new(Mutex::new(EntryState {
            config: initial,
            name: None,
            memory: None,
            handle: None,
            contexts: HashMap::new(),
        }));
        let launch = {
            let (this, state, pubkey, spawn) = (
                self.clone(),
                state.clone(),
                pubkey.to_string(),
                spawn.clone(),
            );
            Arc::new(move || {
                let (this, state, pubkey, spawn) =
                    (this.clone(), state.clone(), pubkey.clone(), spawn.clone());
                async move {
                    let (config, name) = {
                        let state = locked(&state);
                        (state.config.clone(), state.name.clone())
                    };
                    let memory = this.memory(&pubkey, &state).await;
                    let Where { cwd, model, .. } = config.at(&spawn);
                    Ok(Launch {
                        cwd,
                        tools: Some(this.tools(&pubkey, &state, &spawn, &config.workspace)),
                        model,
                        system_prompt: system_prompt(
                            config.scope(),
                            &config.workspace,
                            name.as_deref().map(|name| (name, pubkey.as_str())),
                            &config.instructions,
                            memory.as_ref().map(|memory| memory.as_deref()),
                        ),
                        ..Default::default()
                    })
                }
                .boxed()
            })
        };
        let fingerprint = {
            let state = state.clone();
            Arc::new(move || {
                let state = locked(&state);
                json!([state.config.fingerprint(), state.name]).to_string()
            })
        };
        let mut options =
            SessionsOptions::new(spawn, (self.inner.store)(pubkey), launch, fingerprint);
        options.spare = Some(self.inner.spare.clone());
        let changes = self.clone();
        options.on_change = Arc::new(move || changes.notify());
        let entry = Arc::new(Entry {
            sessions: AgentSessions::new(options),
            state,
        });
        agents.insert(pubkey.to_string(), entry.clone());
        entry
    }

    /// The tool server for processes started in `cwd`, which read files
    /// where they were started until they are replaced.
    fn tools(
        &self,
        pubkey: &str,
        state: &Arc<Mutex<EntryState>>,
        spawn: &Spawner,
        cwd: &str,
    ) -> ToolServer {
        let (this, state, pubkey, spawn, cwd) = (
            self.clone(),
            state.clone(),
            pubkey.to_string(),
            spawn.clone(),
            cwd.to_string(),
        );
        Arc::new(move |conversation: Option<String>, message: Value| {
            let (handle, context) = {
                let state = locked(&state);
                let context = conversation
                    .as_ref()
                    .and_then(|conversation| state.contexts.get(conversation).cloned())
                    .unwrap_or_default();
                (state.handle.clone(), context)
            };
            let client: Option<Arc<dyn BuzzClient>> = handle.map(|agent| {
                let memories = {
                    let (this, pubkey) = (this.clone(), pubkey.clone());
                    Arc::new(move || this.memories(&pubkey).boxed()) as app_client::Memories
                };
                let remembered = {
                    let state = state.clone();
                    Arc::new(move |slug: &str| {
                        if slug == "core" {
                            locked(&state).memory = None;
                        }
                    }) as app_client::Remembered
                };
                Arc::new(AppClient {
                    agent,
                    spawn: spawn.clone(),
                    cwd: cwd.clone(),
                    memories,
                    remembered,
                    check: None,
                    signal: None,
                }) as Arc<dyn BuzzClient>
            });
            async move { Ok(rpc::respond(client, &context, &message).await) }.boxed()
        })
    }

    /// The agent's `core` memory, as the harness injects it into a new
    /// session: its text, `Some(None)` when it has none, or `None` when it
    /// could not be read.
    async fn memory(&self, pubkey: &str, state: &Arc<Mutex<EntryState>>) -> Option<Option<String>> {
        if let Some((value, at)) = &locked(state).memory {
            let ttl = match value {
                Some(_) => MEMORY_TTL,
                None => FAILED_MEMORY_TTL,
            };
            if at.elapsed() < ttl {
                return value.clone();
            }
        }
        let value = match self.memories(pubkey).await {
            Ok(memories) => Some(
                memories
                    .into_iter()
                    .find(|memory| memory.slug == "core")
                    .map(|memory| memory.body),
            ),
            Err(error) => {
                tracing::warn!(%error, "Claude could not read the agent's memory");
                None
            }
        };
        locked(state).memory = Some((value.clone(), Instant::now()));
        value
    }

    /// The agent's memory, read back by the owner in the agent's own
    /// community. Calls at once share one read.
    fn memories(
        &self,
        pubkey: &str,
    ) -> impl std::future::Future<Output = Result<Vec<Memory>, String>> + Send + 'static {
        locked(&self.inner.reading)
            .entry(pubkey.to_string())
            .or_insert_with(|| {
                let (this, pubkey) = (self.clone(), pubkey.to_string());
                async move {
                    let result = match this.relay(&pubkey) {
                        Some(relay) => {
                            tokio::time::timeout(MEMORY_TIMEOUT, relay.memories(&pubkey))
                                .await
                                .unwrap_or_else(|_| Err("Memory read timed out".into()))
                        }
                        None => Err("Buzz is not connected".into()),
                    };
                    locked(&this.inner.reading).remove(&pubkey);
                    result
                }
                .boxed()
                .shared()
            })
            .clone()
    }
}

/// NIP-10 as relay ingest reads it: the last valid marked root and reply
/// win; a lone root is not a reply.
pub fn thread_reference(event: &BuzzEvent) -> Option<Thread> {
    let (mut root, mut reply) = (None, None);
    for tag in &event.tags {
        let (Some(name), Some(value)) = (tag.first(), tag.get(1)) else {
            continue;
        };
        let value = value.to_lowercase();
        if name != "e" || !hex(&value) {
            continue;
        }
        match tag.get(3).map(String::as_str) {
            Some("root") => root = Some(value),
            Some("reply") => reply = Some(value),
            _ => {}
        }
    }
    let reply = reply?;
    Some(Thread {
        root_id: root.unwrap_or_else(|| reply.clone()),
        parent_id: reply,
    })
}

fn tag(event: &BuzzEvent, name: &str) -> Option<String> {
    crate::buzz_mcp::tools::tag(event, name)
}

#[cfg(test)]
mod tests;
