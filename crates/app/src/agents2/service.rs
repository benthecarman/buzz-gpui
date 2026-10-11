//! The Agents2 service (buzz-app's `features/agents2/service.ts`).
//!
//! The agent list is the union of every connected host's saved Agents2
//! identities; `records` only adds local settings to them. Every change is
//! made on the host first and then read back, so the list has one source.
//! The visible agents are those the signed-in owner made for the selected
//! community. Each saved agent has one runner for as long as its identity
//! exists: matching live events and due timers queue there and run one at
//! a time through its type's `run`. Every GUI on the agent's host, and
//! every copy of the app sharing it, hears the same events; only the one
//! the host lets hold the agent runs them.

use super::attention::{
    self, AttentionObject, AttentionValue, CHAT_KINDS, EventData, Predicate, TimerState,
    addressed_to, compile_filter, tags_agent, timer_spent, timer_state, watch_matches,
};
use super::registry::{self, RegisteredType};
use super::runtime::{
    AgentHandle, Bounded, CancelSource, Cancellation, Delivery, HostLink, Trigger, UNSUPPORTED,
};
use super::store::{
    self, AgentRecord, FileStorage, Storage, read_records, set_attention, write_records,
};
use super::{Agent, AgentChange};
use crate::community::{AppState, Community, IdentityState};
use crate::hosts::{HostConnection, Hosts};
use crate::runtime::Runtime;
use buzz_host::api::{AppAgentRequest, AppAgentResponse, AppAgentSummary};
use buzz_host_rpc::ConnectionState;
use buzz_identity::Identity;
use buzz_relay_client::{LiveBatch, RelayOrigin, Session, SessionStatus};
use gpui_kit::{App, AppContext, Context, Entity, EntityId, Global, Subscription, Task};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const QUEUE_LIMIT: usize = 32;
/// Runs per agent per minute; bounds two agents that answer each other.
pub(crate) const RATE_LIMIT: u32 = 60;
pub(crate) const RATE_WINDOW: Duration = Duration::from_secs(60);
const TIMER_TICK: Duration = Duration::from_secs(5);
/// Bounds one cleanup attempt.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(60);

/// Whether the agent list could be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// No agent host is available.
    Unavailable,
    Loading,
    Ready,
    Error(String),
}

/// One saved identity and the host that holds its key.
#[derive(Clone)]
pub(crate) struct Saved {
    pub summary: AppAgentSummary,
    pub host: Entity<HostConnection>,
}

#[derive(Clone, Debug, PartialEq)]
enum ListState {
    Loading,
    Ready,
    Error(String),
    /// A host from before Agents2.
    Unsupported,
}

struct HostList {
    host: Entity<HostConnection>,
    state: ListState,
    agents: Vec<AppAgentSummary>,
    /// Counts list requests, so only the latest one is applied.
    turn: u64,
    connected: bool,
    _observe: Subscription,
}

/// The selected community while its session is ready.
struct Binding {
    community: Entity<Community>,
    /// `https://host[:port]`, comparable with an identity's relay.
    origin: String,
    /// The relay URL new agents are made for.
    destination: String,
    viewer: String,
    identity: Arc<Identity>,
    session: Arc<Session>,
    live: bool,
    /// Ends work started for this binding when it is replaced.
    generation: u64,
    _events: Task<()>,
}

struct Job {
    trigger: Trigger,
    channel: Option<String>,
}

pub(crate) struct CompiledWatch {
    slug: String,
    watch: attention::EventWatch,
    interest: Option<attention::Interest>,
    classifier_not_run: bool,
    filter: Option<Predicate>,
}

/// The watches compiled for one attention set.
type Compiled = (
    Arc<BTreeMap<String, AttentionObject>>,
    Arc<Vec<CompiledWatch>>,
);

/// One agent's runtime, for as long as its identity exists. Edits do not
/// replace it: each job reads the agent as it is when the job starts.
struct Runner {
    /// The registration whose `run` it calls; a new one cancels the run in
    /// flight.
    kind: Option<RegisteredType>,
    /// Fires when the agent is deleted or its type replaced.
    lifetime: Arc<CancelSource>,
    lifetime_signal: Cancellation,
    queue: VecDeque<Job>,
    running: bool,
    window: Window,
    /// Events seen, and events it wrote (so replies to it are addressed).
    seen: Bounded,
    wrote: Arc<Mutex<Bounded>>,
    compiled: Option<Compiled>,
    /// Tells a replaced runner's drain loop to stop.
    serial: u64,
    /// Whether the host may hold the agent for this GUI; true at first, so
    /// an agent that starts out of view is let go of once.
    may_hold: bool,
}

/// The agent and runnable type a drain step saw, to tell whether either
/// changed while it waited on the host.
#[derive(Clone)]
struct Seen {
    agent: Option<Agent>,
    kind: Option<RegisteredType>,
}

impl PartialEq for Seen {
    fn eq(&self, other: &Self) -> bool {
        self.agent == other.agent
            && match (&self.kind, &other.kind) {
                (Some(a), Some(b)) => Arc::ptr_eq(&a.kind, &b.kind),
                (None, None) => true,
                _ => false,
            }
    }
}

/// What a runner does next.
enum Step {
    Idle,
    /// It cannot run here: let another GUI or copy of the app have it.
    Release(HostLink, Seen),
    /// Ask the host to hold it before its next job.
    Claim(HostLink, Seen),
}

/// A job started, or why none did.
enum Start {
    Idle,
    /// The agent or its type changed while the claim was out.
    Changed,
    Run(
        Task<Result<(), String>>,
        Duration,
        Cancellation,
        CancelSource,
    ),
}

/// Whether the host's answer to a claim lets this GUI run the agent. A
/// claim that fails, as on a host from before claims, runs it anyway:
/// answering twice beats not answering at all.
fn claimed(pubkey: &str, answer: Result<AppAgentResponse, String>) -> bool {
    match answer {
        Ok(AppAgentResponse::Claimed(claimed)) => claimed,
        Ok(_) => true,
        Err(error) => {
            if error != UNSUPPORTED {
                tracing::warn!(agent = %pubkey, %error, "agent was not claimed");
            }
            true
        }
    }
}

/// The per-minute admission window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Window {
    pub start: Option<Instant>,
    pub admitted: u32,
}

/// Whether one more job may queue, counting it if so: at most
/// [`QUEUE_LIMIT`] waiting and [`RATE_LIMIT`] admitted per minute.
pub(crate) fn admit(window: &mut Window, queued: usize, now: Instant) -> bool {
    if window
        .start
        .is_none_or(|start| now.duration_since(start) >= RATE_WINDOW)
    {
        window.start = Some(now);
        window.admitted = 0;
    }
    if queued >= QUEUE_LIMIT || window.admitted >= RATE_LIMIT {
        return false;
    }
    window.admitted += 1;
    true
}

/// What one event wakes an agent for. `same_owner` says the author is
/// another agent of this agent's owner: two of one owner's agents would
/// otherwise answer each other forever, so one hears another only when it
/// is named in something that does not reply to it. Addressed events never
/// reach a watch, and only conversation starts a mention run.
pub(crate) fn heard(
    agent: &str,
    event: &Arc<EventData>,
    same_owner: bool,
    wrote: &dyn Fn(&str) -> bool,
    watches: &[CompiledWatch],
) -> Vec<Trigger> {
    let mention = || {
        vec![Trigger::Mention {
            event: event.clone(),
        }]
    };
    if same_owner {
        let replies_to_it = event.tags.iter().any(|tag| {
            tag.first().map(String::as_str) == Some("e")
                && tag.get(1).is_some_and(|id| !id.is_empty() && wrote(id))
        });
        return if CHAT_KINDS.contains(&event.kind) && tags_agent(event, agent) && !replies_to_it {
            mention()
        } else {
            Vec::new()
        };
    }
    if addressed_to(event, agent, wrote) {
        return if CHAT_KINDS.contains(&event.kind) {
            mention()
        } else {
            Vec::new()
        };
    }
    watches
        .iter()
        .filter(|compiled| watch_matches(&compiled.watch, event, compiled.filter.as_ref()))
        .map(|compiled| Trigger::Watch {
            event: event.clone(),
            slug: compiled.slug.clone(),
            watch: compiled.watch.clone(),
            interest: compiled.interest.clone(),
            classifier_not_run: compiled.classifier_not_run,
        })
        .collect()
}

/// The agent's enabled event watches.
fn compile(attention: &BTreeMap<String, AttentionObject>) -> Vec<CompiledWatch> {
    attention
        .values()
        .filter_map(|object| {
            let AttentionValue::Event(watch) = &object.value else {
                return None;
            };
            if !watch.enabled {
                return None;
            }
            Some(CompiledWatch {
                slug: object.slug.clone(),
                interest: interest(attention, &watch.interest_id),
                // No classifier model runs here yet, so the event passes
                // unclassified.
                classifier_not_run: watch.classifier.is_some(),
                filter: watch
                    .filter
                    .as_deref()
                    .and_then(|source| compile_filter(source).ok()),
                watch: watch.clone(),
            })
        })
        .collect()
}

fn interest(
    attention: &BTreeMap<String, AttentionObject>,
    id: &str,
) -> Option<attention::Interest> {
    match attention
        .get(&attention::interest_slug(id))
        .map(|o| &o.value)
    {
        Some(AttentionValue::Interest(interest)) => Some(interest.clone()),
        _ => None,
    }
}

/// Spec schedule rules: occurrence k is due at armed_at + k × interval, at
/// most one runs when overdue, then the next is due interval after it ran.
/// A timer with no saved state counts what was already due as used. The
/// longest-waiting timer runs first, so one whose runs outlast its interval
/// cannot hold back the others. Returns the new timer states and the timer
/// to run, if any.
pub(crate) fn due_timer(
    attention: &BTreeMap<String, AttentionObject>,
    prior: &BTreeMap<String, TimerState>,
    now: u64,
) -> (
    BTreeMap<String, TimerState>,
    Option<(String, attention::TimerWatch)>,
) {
    let mut timers = prior.clone();
    let mut next: Option<(&str, &attention::TimerWatch, TimerState)> = None;
    for object in attention.values() {
        let AttentionValue::Timer(timer) = &object.value else {
            continue;
        };
        if !timer.enabled {
            continue;
        }
        let saved = prior.get(&object.slug);
        let state = timer_state(timer, saved, now);
        if saved != Some(&state) {
            timers.insert(object.slug.clone(), state);
        }
        if now < state.next_due || timer_spent(timer, &state, now) {
            continue;
        }
        if next
            .as_ref()
            .is_none_or(|(_, _, best)| state.next_due < best.next_due)
        {
            next = Some((&object.slug, timer, state));
        }
    }
    let Some((slug, timer, state)) = next else {
        return (timers, None);
    };
    timers.insert(
        slug.to_owned(),
        TimerState {
            used: state.used + 1,
            next_due: now + timer.interval_secs,
            ..state
        },
    );
    (timers, Some((slug.to_owned(), timer.clone())))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A new agent's settings: its type's defaults.
fn defaults(pubkey: &str, kind: &RegisteredType) -> Result<AgentRecord, String> {
    let defaults = kind.kind.defaults();
    let mut record = AgentRecord {
        config: defaults.config,
        ..AgentRecord::blank(pubkey)
    };
    let now = now_secs();
    for (slug, value) in &defaults.attention {
        record = set_attention(&record, slug, Some(&value.to_json()), now)?;
    }
    Ok(record)
}

/// The http origin of a saved `wss://` relay.
fn http_origin(relay: &str) -> Option<String> {
    RelayOrigin::parse(relay).ok().map(|origin| origin.http())
}

struct Agents2Global(Entity<Agents2>);

impl Global for Agents2Global {}

pub(super) fn init(cx: &mut App) {
    let storage: Arc<dyn Storage> = Arc::new(FileStorage(
        crate::community::data_dir().join(format!("{}.json", store::STORAGE_KEY)),
    ));
    let service = cx.new(|cx| Agents2::new(storage, cx));
    cx.set_global(Agents2Global(service));
}

pub struct Agents2 {
    storage: Arc<dyn Storage>,
    records: BTreeMap<String, AgentRecord>,
    lists: Vec<(EntityId, HostList)>,
    status: Status,
    agents: Vec<Agent>,
    binding: Option<Binding>,
    /// The selected community, observed.
    watched: Option<(EntityId, Subscription)>,
    generation: u64,
    runners: HashMap<String, Runner>,
    serial: u64,
    /// Deleted agents whose community cleanup is running, and whether it
    /// was asked for again meanwhile.
    cleaning: HashMap<String, bool>,
    _subscriptions: Vec<Subscription>,
    _tick: Task<()>,
}

impl Agents2 {
    fn new(storage: Arc<dyn Storage>, cx: &mut Context<Self>) -> Self {
        let records = read_records(storage.as_ref());
        let subscriptions = vec![
            cx.observe_global::<registry::AgentTypes>(|this, cx| {
                this.update(cx);
                cx.notify();
            }),
            cx.observe_global::<Hosts>(|this, cx| this.watch_hosts(cx)),
            cx.observe_global::<AppState>(|this, cx| this.bind(cx)),
        ];
        // Wakes each runner, which starts any timer that is due.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TIMER_TICK).await;
                let alive = this.update(cx, |this, cx| {
                    let pubkeys: Vec<String> = this.runners.keys().cloned().collect();
                    for pubkey in pubkeys {
                        this.drain(&pubkey, cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        let mut this = Self {
            storage,
            records,
            lists: Vec::new(),
            status: Status::Unavailable,
            agents: Vec::new(),
            binding: None,
            watched: None,
            generation: 0,
            runners: HashMap::new(),
            serial: 0,
            cleaning: HashMap::new(),
            _subscriptions: subscriptions,
            _tick: tick,
        };
        this.watch_hosts(cx);
        this.bind(cx);
        this
    }

    pub fn global(cx: &App) -> Option<Entity<Agents2>> {
        cx.try_global::<Agents2Global>()
            .map(|global| global.0.clone())
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The selected community's agents.
    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    /// The agent of this pubkey in the selected community, if it is an
    /// Agents2 agent.
    pub fn find(&self, pubkey: &str) -> Option<&Agent> {
        self.agents.iter().find(|agent| agent.pubkey == pubkey)
    }

    /// The host holding the agent's key.
    pub fn host_of(&self, pubkey: &str) -> Option<Entity<HostConnection>> {
        self.saved()
            .find(|saved| saved.summary.pubkey == pubkey)
            .map(|saved| saved.host)
    }

    /// Hosts that can hold a new agent's key.
    pub fn hosts(&self, cx: &App) -> Vec<Entity<HostConnection>> {
        self.lists
            .iter()
            .filter(|(_, list)| list.state == ListState::Ready)
            .map(|(_, list)| list.host.clone())
            .filter(|host| matches!(host.read(cx).state, ConnectionState::Connected { .. }))
            .collect()
    }

    /// Every saved identity, deleted ones included.
    fn saved(&self) -> impl Iterator<Item = Saved> + '_ {
        self.lists.iter().flat_map(|(_, list)| {
            list.agents.iter().map(|summary| Saved {
                summary: summary.clone(),
                host: list.host.clone(),
            })
        })
    }

    fn live(&self, pubkey: &str) -> Option<Saved> {
        self.saved()
            .find(|saved| saved.summary.pubkey == pubkey && !saved.summary.deleted)
    }

    // ----- hosts -----

    /// Follows the host list: new hosts are listed when they connect.
    fn watch_hosts(&mut self, cx: &mut Context<Self>) {
        let hosts: Vec<Entity<HostConnection>> = cx
            .try_global::<Hosts>()
            .map(|hosts| hosts.hosts.clone())
            .unwrap_or_default();
        let before = self.lists.len();
        self.lists
            .retain(|(id, _)| hosts.iter().any(|host| host.entity_id() == *id));
        let mut added = false;
        for host in hosts {
            if self.lists.iter().any(|(id, _)| *id == host.entity_id()) {
                continue;
            }
            let observe = cx.observe(&host, |this, host, cx| this.host_changed(host, cx));
            self.lists.push((
                host.entity_id(),
                HostList {
                    host: host.clone(),
                    state: ListState::Loading,
                    agents: Vec::new(),
                    turn: 0,
                    connected: false,
                    _observe: observe,
                },
            ));
            added = true;
            self.host_changed(host, cx);
        }
        if added || self.lists.len() != before {
            self.update(cx);
            self.recompute_status();
            cx.notify();
        }
    }

    /// Lists a host's agents whenever it connects.
    fn host_changed(&mut self, host: Entity<HostConnection>, cx: &mut Context<Self>) {
        let connected = matches!(host.read(cx).state, ConnectionState::Connected { .. });
        let Some((_, list)) = self
            .lists
            .iter_mut()
            .find(|(id, _)| *id == host.entity_id())
        else {
            return;
        };
        let was = list.connected;
        list.connected = connected;
        if connected && !was {
            self.refresh_host(host, cx).detach();
        } else if !connected && was {
            self.recompute_status();
            cx.notify();
        }
    }

    /// Reads the agent list from every host.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let tasks: Vec<Task<()>> = self
            .lists
            .iter()
            .filter(|(_, list)| list.connected)
            .map(|(_, list)| list.host.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|host| self.refresh_host(host, cx))
            .collect();
        cx.spawn(async move |_, _| {
            futures::future::join_all(tasks).await;
        })
    }

    fn refresh_host(&mut self, host: Entity<HostConnection>, cx: &mut Context<Self>) -> Task<()> {
        let id = host.entity_id();
        let Some((_, list)) = self.lists.iter_mut().find(|(key, _)| *key == id) else {
            return Task::ready(());
        };
        list.turn += 1;
        let turn = list.turn;
        let link = link(&host, cx);
        cx.spawn(async move |this, cx| {
            let listed = link.app_agent(AppAgentRequest::List).await;
            let _ = this.update(cx, |this, cx| {
                let Some((_, list)) = this.lists.iter_mut().find(|(key, _)| *key == id) else {
                    return;
                };
                // A newer list is on its way; this one may predate a change.
                if list.turn != turn {
                    return;
                }
                match listed {
                    Ok(AppAgentResponse::Agents(agents)) => {
                        list.agents = agents;
                        list.state = ListState::Ready;
                    }
                    Ok(_) => list.state = ListState::Error("Invalid agent list".into()),
                    Err(error) if error == super::runtime::UNSUPPORTED => {
                        list.agents.clear();
                        list.state = ListState::Unsupported;
                    }
                    Err(error) => list.state = ListState::Error(error),
                }
                this.prune();
                this.recompute_status();
                this.update(cx);
                cx.notify();
                this.sync(cx);
            });
        })
    }

    /// Settings whose agent is gone do nothing; they are dropped once every
    /// host has answered, so a host that is offline keeps its agents'.
    fn prune(&mut self) {
        let settled = self
            .lists
            .iter()
            .all(|(_, list)| matches!(list.state, ListState::Ready | ListState::Unsupported));
        if !settled {
            return;
        }
        let live: Vec<String> = self
            .saved()
            .filter(|saved| !saved.summary.deleted)
            .map(|saved| saved.summary.pubkey)
            .collect();
        if self.records.keys().any(|pubkey| !live.contains(pubkey)) {
            self.records.retain(|pubkey, _| live.contains(pubkey));
            if let Err(error) = write_records(self.storage.as_ref(), &self.records) {
                tracing::warn!(%error, "agent settings were not saved");
            }
        }
    }

    fn recompute_status(&mut self) {
        let states: Vec<&ListState> = self.lists.iter().map(|(_, list)| &list.state).collect();
        self.status = if states.is_empty() {
            Status::Unavailable
        } else if states.contains(&&ListState::Ready) {
            Status::Ready
        } else if states.contains(&&ListState::Loading) {
            Status::Loading
        } else {
            let error = states.iter().find_map(|state| match state {
                ListState::Error(error) => Some(error.clone()),
                _ => None,
            });
            Status::Error(error.unwrap_or_else(|| super::runtime::UNSUPPORTED.into()))
        };
    }

    // ----- the selected community -----

    fn bind(&mut self, cx: &mut Context<Self>) {
        // Follows the selected community, to bind once its session is ready.
        let current = AppState::current(cx);
        if self.watched.as_ref().map(|(id, _)| *id) != current.as_ref().map(Entity::entity_id) {
            self.watched = current.map(|community| {
                (
                    community.entity_id(),
                    cx.observe(&community, |this, _, cx| this.bind(cx)),
                )
            });
        }
        let next = AppState::current(cx).and_then(|community| {
            let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
                return None;
            };
            let read = community.read(cx);
            (read.snapshot.status == SessionStatus::Ready)
                .then(|| (community.clone(), identity.clone(), read.snapshot.live))
        });
        let same = match (&self.binding, &next) {
            (Some(binding), Some((community, ..))) => binding.community == *community,
            (None, None) => true,
            _ => false,
        };
        if same {
            // Retries what failed whenever the connection recovers.
            if let (Some(binding), Some((_, _, live))) = (&mut self.binding, &next) {
                let recovered = *live && !binding.live;
                binding.live = *live;
                if recovered {
                    self.sync(cx);
                }
            }
            return;
        }
        self.generation += 1;
        self.binding = next.map(|(community, identity, live)| {
            let read = community.read(cx);
            let session = read.session.clone();
            let origin = read.origin.http();
            let destination = read.origin.websocket().to_owned();
            let events = listen(&session, cx);
            Binding {
                origin,
                destination,
                viewer: session.viewer().to_owned(),
                identity,
                session,
                live,
                generation: self.generation,
                community,
                _events: events,
            }
        });
        self.update(cx);
        cx.notify();
        if self.binding.is_some() {
            self.sync(cx);
        }
    }

    fn in_scope(&self, summary: &AppAgentSummary) -> bool {
        self.binding.as_ref().is_some_and(|binding| {
            summary.owner == binding.viewer
                && http_origin(&summary.relay).as_deref() == Some(binding.origin.as_str())
        })
    }

    /// Brings the relay in line with the saved identities: publishes each
    /// name its community lacks, and finishes Delete for the open
    /// community. Each step is safe to repeat; what fails is tried again on
    /// the next refresh or reconnect.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let saved: Vec<Saved> = self.saved().collect();
        for identity in saved {
            if !identity.summary.deleted {
                let link = link(&identity.host, cx);
                let name = identity.summary.name.clone();
                let pubkey = identity.summary.pubkey.clone();
                cx.background_spawn(async move {
                    if let Err(error) = link
                        .app_agent(AppAgentRequest::PublishProfile { pubkey })
                        .await
                    {
                        tracing::warn!(agent = %name, %error, "agent profile was not published");
                    }
                })
                .detach();
            } else if self.in_scope(&identity.summary) {
                self.cleanup(identity, cx);
            }
        }
    }

    /// As harness Delete does: leave every channel, then archive the
    /// identity so it drops out of member lists and mention suggestions.
    /// The owner signs both, so neither needs the deleted key.
    fn cleanup(&mut self, identity: Saved, cx: &mut Context<Self>) {
        let pubkey = identity.summary.pubkey.clone();
        if let Some(again) = self.cleaning.get_mut(&pubkey) {
            *again = true;
            return;
        }
        let Some(binding) = &self.binding else {
            return;
        };
        self.cleaning.insert(pubkey.clone(), false);
        let generation = binding.generation;
        let session = binding.session.clone();
        let viewer = binding.identity.clone();
        let link = link(&identity.host, cx);
        let runtime = Runtime::handle(cx);
        cx.spawn(async move |this, cx| {
            let target = pubkey.clone();
            let work = runtime.spawn(async move {
                tokio::time::timeout(
                    CLEANUP_TIMEOUT,
                    super::cleanup::clean(&session, &viewer, &target),
                )
                .await
                .unwrap_or_else(|_| Err("Deleted agent cleanup timed out".into()))
            });
            let mut outcome = work
                .await
                .unwrap_or_else(|_| Err("Deleted agent cleanup was cancelled".into()));
            if outcome.is_ok() {
                outcome = link
                    .app_agent(AppAgentRequest::Forget {
                        pubkey: pubkey.clone(),
                    })
                    .await
                    .map(drop);
            }
            let _ = this.update(cx, |this, cx| {
                let again = this.cleaning.remove(&pubkey).unwrap_or(false);
                let current = this
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.generation == generation);
                match outcome {
                    Ok(()) => {
                        this.refresh(cx).detach();
                    }
                    Err(error) if current => {
                        tracing::warn!(%error, "deleted agent cleanup will be retried");
                        if again {
                            this.cleanup(identity, cx);
                        }
                    }
                    Err(_) => {}
                }
            });
        })
        .detach();
    }

    // ----- the visible agents and their runners -----

    /// Its saved settings or, without any, its type's defaults, kept in
    /// memory until a save stores them. Without its type there are no
    /// defaults yet.
    fn record_of(&mut self, summary: &AppAgentSummary, cx: &App) -> AgentRecord {
        if !self.records.contains_key(&summary.pubkey)
            && let Some(kind) = registry::type_of(&summary.agent_type, cx)
        {
            match defaults(&summary.pubkey, &kind) {
                Ok(record) => {
                    self.records.insert(summary.pubkey.clone(), record);
                }
                Err(error) => {
                    tracing::warn!(kind = %summary.agent_type, %error, "agent type has invalid defaults");
                }
            }
        }
        self.records
            .get(&summary.pubkey)
            .cloned()
            .unwrap_or_else(|| AgentRecord::blank(&summary.pubkey))
    }

    fn view(summary: &AppAgentSummary, record: &AgentRecord) -> Agent {
        Agent {
            pubkey: summary.pubkey.clone(),
            name: summary.name.clone(),
            agent_type: summary.agent_type.clone(),
            owner: summary.owner.clone(),
            relay: summary.relay.clone(),
            attention: record.attention.clone(),
            skipped: record.skipped.clone(),
            timers: record.timers.clone(),
            config: record.config.clone(),
        }
    }

    /// Joins identities, records and the binding into the visible agents,
    /// and keeps one runner per saved agent. Runners outlive edits and
    /// reconnects; one is retired only when its agent is deleted, and its
    /// run is cancelled when the agent's type is replaced.
    fn update(&mut self, cx: &mut Context<Self>) {
        let was_shown: Vec<String> = self.agents.iter().map(|a| a.pubkey.clone()).collect();
        let live: Vec<AppAgentSummary> = self
            .saved()
            .filter(|saved| !saved.summary.deleted)
            .map(|saved| saved.summary)
            .collect();
        let mut agents = Vec::new();
        for summary in &live {
            if !self.in_scope(summary) {
                continue;
            }
            let record = self.record_of(summary, cx);
            agents.push(Self::view(summary, &record));
        }
        self.runners
            .retain(|pubkey, _| live.iter().any(|summary| &summary.pubkey == pubkey));
        let mut drains = Vec::new();
        for summary in &live {
            let kind = registry::type_of(&summary.agent_type, cx);
            match self.runners.get_mut(&summary.pubkey) {
                None => {
                    self.serial += 1;
                    let (lifetime, lifetime_signal) = CancelSource::new();
                    self.runners.insert(
                        summary.pubkey.clone(),
                        Runner {
                            kind,
                            lifetime: Arc::new(lifetime),
                            lifetime_signal,
                            queue: VecDeque::new(),
                            running: false,
                            window: Window {
                                start: None,
                                admitted: 0,
                            },
                            seen: Bounded::default(),
                            wrote: Arc::default(),
                            compiled: None,
                            serial: self.serial,
                            may_hold: true,
                        },
                    );
                }
                Some(runner) => {
                    let replaced = match (&runner.kind, &kind) {
                        (Some(a), Some(b)) => !Arc::ptr_eq(&a.kind, &b.kind),
                        (None, None) => false,
                        _ => true,
                    };
                    if replaced {
                        runner.lifetime.cancel();
                        let (lifetime, signal) = CancelSource::new();
                        runner.lifetime = Arc::new(lifetime);
                        runner.lifetime_signal = signal;
                        runner.kind = kind;
                        drains.push(summary.pubkey.clone());
                    }
                }
            }
        }
        if agents != self.agents {
            self.agents = agents;
            cx.notify();
        }
        // Resume runners whose queue paused while their agent was out of
        // view, and claim or release each agent as it comes into or leaves
        // view, so another GUI takes over without waiting for an event or
        // tick.
        for (pubkey, runner) in &self.runners {
            let shown = self.agents.iter().any(|agent| &agent.pubkey == pubkey);
            if !runner.queue.is_empty() || shown != was_shown.contains(pubkey) {
                drains.push(pubkey.clone());
            }
        }
        for pubkey in drains {
            self.drain(&pubkey, cx);
        }
    }

    fn dispatch(&mut self, batch: LiveBatch, cx: &mut Context<Self>) {
        let events: Vec<Arc<EventData>> = batch
            .events
            .iter()
            .map(|event| Arc::new(EventData::from(event.as_ref())))
            .collect();
        let owners: HashMap<String, String> = self
            .saved()
            .map(|saved| (saved.summary.pubkey, saved.summary.owner))
            .collect();
        let mut drains = Vec::new();
        let now = Instant::now();
        for agent in &self.agents {
            let Some(runner) = self.runners.get_mut(&agent.pubkey) else {
                continue;
            };
            if !runner.kind.as_ref().is_some_and(|kind| kind.kind.runs()) {
                continue;
            }
            let watches = match &runner.compiled {
                Some((attention, watches)) if Arc::ptr_eq(attention, &agent.attention) => {
                    watches.clone()
                }
                _ => {
                    let watches = Arc::new(compile(&agent.attention));
                    runner.compiled = Some((agent.attention.clone(), watches.clone()));
                    watches
                }
            };
            for event in &events {
                if !runner.seen.insert(&event.id) {
                    continue;
                }
                // An agent never hears itself; its own events make replies
                // addressed.
                if event.pubkey == agent.pubkey {
                    runner
                        .wrote
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(&event.id);
                    continue;
                }
                let same_owner = owners.get(&event.pubkey) == Some(&agent.owner);
                let wrote = runner.wrote.clone();
                let wrote =
                    move |id: &str| wrote.lock().unwrap_or_else(|p| p.into_inner()).contains(id);
                for trigger in heard(&agent.pubkey, event, same_owner, &wrote, &watches) {
                    if admit(&mut runner.window, runner.queue.len(), now) {
                        runner.queue.push_back(Job {
                            trigger,
                            channel: batch.channel.clone(),
                        });
                    } else {
                        tracing::warn!(agent = %agent.name, "agent dropped an event");
                    }
                }
            }
            if !runner.queue.is_empty() {
                drains.push(agent.pubkey.clone());
            }
        }
        for pubkey in drains {
            self.drain(&pubkey, cx);
        }
    }

    /// The due timer of a visible agent, recording its run.
    fn due(&mut self, agent: &Agent) -> Option<Job> {
        let record = self
            .records
            .get(&agent.pubkey)
            .cloned()
            .unwrap_or_else(|| AgentRecord::blank(&agent.pubkey));
        let (timers, next) = due_timer(&agent.attention, &agent.timers, now_secs());
        if timers != *record.timers {
            let record = AgentRecord {
                timers: Arc::new(timers),
                ..record
            };
            // Unsaved, it would run again at once; it waits for storage
            // instead.
            if let Err(error) = self.write(record) {
                tracing::warn!(agent = %agent.name, %error, "agent timer state was not saved");
                return None;
            }
        }
        let (slug, timer) = next?;
        let interest = interest(&agent.attention, &timer.interest_id);
        Some(Job {
            trigger: Trigger::Timer {
                slug,
                timer,
                interest,
            },
            channel: None,
        })
    }

    /// One run at a time per agent: events in arrival order, then a due
    /// timer. While the agent is out of view the queue pauses; a type that
    /// cannot run drops what is queued. Either way the host lets another
    /// GUI have the agent. Before each run, on every wake, the host is
    /// asked whether this GUI holds the agent; one that does not drops
    /// what is queued. A run that ignores its deadline stops holding the
    /// queue when the deadline passes.
    fn drain(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let Some(runner) = self.runners.get_mut(pubkey) else {
            return;
        };
        if runner.running {
            return;
        }
        runner.running = true;
        let serial = runner.serial;
        let pubkey = pubkey.to_owned();
        cx.spawn(async move |this, cx| {
            loop {
                let (link, seen, claim) =
                    match this.update(cx, |this, cx| this.step(&pubkey, serial, cx)) {
                        Ok(Step::Claim(link, seen)) => (link, seen, true),
                        Ok(Step::Release(link, seen)) => (link, seen, false),
                        Ok(Step::Idle) | Err(_) => break,
                    };
                let request = match claim {
                    true => AppAgentRequest::Claim {
                        pubkey: pubkey.clone(),
                    },
                    false => AppAgentRequest::Release {
                        pubkey: pubkey.clone(),
                    },
                };
                let answer = link.app_agent(request).await;
                if !claim {
                    if let Err(error) = answer
                        && error != UNSUPPORTED
                    {
                        tracing::warn!(agent = %pubkey, %error, "agent was not released");
                    }
                    // It may have come back into view, or gained its type,
                    // meanwhile.
                    match this.update(cx, |this, _| this.seen(&pubkey) != seen) {
                        Ok(true) => continue,
                        _ => break,
                    }
                }
                let claimed = claimed(&pubkey, answer);
                let next = this.update(cx, |this, cx| {
                    this.start(&pubkey, serial, claimed, &seen, cx)
                });
                let (run, timeout, lifetime, source) = match next {
                    Ok(Start::Run(run, timeout, lifetime, source)) => {
                        (run, timeout, lifetime, source)
                    }
                    Ok(Start::Changed) => continue,
                    Ok(Start::Idle) | Err(_) => break,
                };
                let deadline = cx.background_executor().timer(timeout);
                let ended = futures::future::select(
                    run,
                    futures::future::select(
                        Box::pin(deadline),
                        Box::pin(async move { lifetime.cancelled().await }),
                    ),
                )
                .await;
                source.cancel();
                match ended {
                    futures::future::Either::Left((Err(error), _)) => {
                        tracing::error!(agent = %pubkey, %error, "agent run failed");
                    }
                    futures::future::Either::Left((Ok(()), _)) => {}
                    // The run keeps going unfenced; the queue moves on.
                    futures::future::Either::Right((_, run)) => run.detach(),
                }
            }
            let _ = this.update(cx, |this, _| {
                if let Some(runner) = this.runners.get_mut(&pubkey)
                    && runner.serial == serial
                {
                    runner.running = false;
                }
            });
        })
        .detach();
    }

    /// The agent in view and its runnable type.
    fn seen(&self, pubkey: &str) -> Seen {
        Seen {
            agent: self.find(pubkey).cloned(),
            kind: self
                .runners
                .get(pubkey)
                .and_then(|runner| runner.kind.clone())
                .filter(|kind| kind.kind.runs()),
        }
    }

    /// Whether the runner asks the host to hold its agent or lets it go.
    /// An agent out of view, or of a type that cannot run here, is left to
    /// another GUI; what it queued in view is dropped.
    fn step(&mut self, pubkey: &str, serial: u64, cx: &mut Context<Self>) -> Step {
        let seen = self.seen(pubkey);
        let host = self.host_of(pubkey);
        let Some(runner) = self.runners.get_mut(pubkey).filter(|r| r.serial == serial) else {
            return Step::Idle;
        };
        if seen.agent.is_none() || seen.kind.is_none() {
            if seen.agent.is_some() {
                runner.queue.clear();
            }
            if !std::mem::replace(&mut runner.may_hold, false) {
                return Step::Idle;
            }
            return match host {
                Some(host) => Step::Release(link(&host, cx), seen),
                None => Step::Idle,
            };
        }
        match host {
            Some(host) => Step::Claim(link(&host, cx), seen),
            None => Step::Idle,
        }
    }

    /// Starts the runner's next job, if this GUI holds the agent and it has
    /// one to run now.
    fn start(
        &mut self,
        pubkey: &str,
        serial: u64,
        claimed: bool,
        seen: &Seen,
        cx: &mut Context<Self>,
    ) -> Start {
        let current = self.seen(pubkey);
        let Some(runner) = self.runners.get_mut(pubkey).filter(|r| r.serial == serial) else {
            return Start::Idle;
        };
        runner.may_hold = claimed;
        // Another GUI or copy of the app holds it and runs these.
        if !claimed {
            runner.queue.clear();
            return Start::Idle;
        }
        if current != *seen {
            return Start::Changed;
        }
        let (Some(agent), Some(kind)) = (current.agent, current.kind) else {
            return Start::Idle;
        };
        let lifetime = runner.lifetime_signal.clone();
        let job = match runner.queue.pop_front() {
            Some(job) => Some(job),
            None => self.due(&agent),
        };
        let Some(job) = job else {
            return Start::Idle;
        };
        let Some(host) = self.host_of(pubkey) else {
            return Start::Idle;
        };
        let Some(runner) = self.runners.get(pubkey) else {
            return Start::Idle;
        };
        let (source, cancel) = CancelSource::new();
        let handle = AgentHandle {
            pubkey: agent.pubkey.clone(),
            name: agent.name.clone(),
            owner: agent.owner.clone(),
            host: link(&host, cx),
            wrote: runner.wrote.clone(),
        };
        let delivery = Delivery {
            trigger: job.trigger,
            channel: job.channel,
            agent: handle,
            config: agent.config.clone(),
            cancel,
        };
        let run = kind.kind.run(delivery, cx);
        Start::Run(run, kind.kind.timeout(), lifetime, source)
    }

    // ----- changes -----

    /// Saves one record; memory changes only once storage has taken it.
    fn write(&mut self, record: AgentRecord) -> Result<(), String> {
        let mut records = self.records.clone();
        records.insert(record.pubkey.clone(), record);
        write_records(self.storage.as_ref(), &records)?;
        self.records = records;
        Ok(())
    }

    /// Makes an agent of `type_key` named `name` for the selected
    /// community, with its key on `host` (the embedded host by default).
    pub fn create(
        &mut self,
        type_key: &str,
        name: &str,
        host: Option<Entity<HostConnection>>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Agent, String>> {
        let Some(binding) = &self.binding else {
            return Task::ready(Err("Connect to a community first".into()));
        };
        let Some(kind) = registry::type_of(type_key, cx) else {
            return Task::ready(Err("That agent type is not available".into()));
        };
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Task::ready(Err("Name the agent".into()));
        }
        let host = host.or_else(|| {
            cx.try_global::<Hosts>()?
                .hosts
                .iter()
                .find(|host| host.read(cx).remote.is_none())
                .cloned()
        });
        let Some(host) = host else {
            return Task::ready(Err("Agents run only in the desktop app".into()));
        };
        let settings = match defaults("", &kind) {
            Ok(settings) => settings,
            Err(error) => return Task::ready(Err(error)),
        };
        let link = link(&host, cx);
        let identity = binding.identity.clone();
        let destination = binding.destination.clone();
        let owner = binding.viewer.clone();
        let type_key = type_key.to_owned();
        cx.spawn(async move |this, cx| {
            let request_id = uuid::Uuid::new_v4().to_string();
            let AppAgentResponse::Prepared { pubkey } = link
                .app_agent(AppAgentRequest::CreatePrepare {
                    request_id: request_id.clone(),
                    destination,
                    owner,
                    agent_type: type_key,
                    name,
                })
                .await?
            else {
                return Err("The host answered with something else".into());
            };
            let auth = identity
                .attest_agent(&pubkey)
                .map_err(|error| error.to_string())?;
            let AppAgentResponse::Created(summary) = link
                .app_agent(AppAgentRequest::CreateCommit { request_id, auth })
                .await?
            else {
                return Err("The host answered with something else".into());
            };
            let refresh = this.update(cx, |this, cx| {
                let record = AgentRecord {
                    pubkey: summary.pubkey.clone(),
                    ..settings
                };
                // The agent exists either way; unsaved settings start from
                // the defaults.
                if let Err(error) = this.write(record) {
                    tracing::warn!(agent = %summary.name, %error, "agent settings were not saved");
                }
                this.refresh(cx)
            });
            if let Ok(refresh) = refresh {
                refresh.await;
            }
            this.update(cx, |this, cx| {
                let record = this.record_of(&summary, cx);
                Self::view(&summary, &record)
            })
            .map_err(|_| "Agents2 stopped".to_string())
        })
    }

    /// Changes an agent's name, config or attention.
    pub fn save(
        &mut self,
        pubkey: &str,
        change: AgentChange,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(identity) = self.live(pubkey) else {
            return Task::ready(Err("No such agent on this device".into()));
        };
        let name = change.name.as_deref().map(str::trim).map(str::to_owned);
        if name.as_deref() == Some("") {
            return Task::ready(Err("Name the agent".into()));
        }
        let mut record = self.record_of(&identity.summary, cx);
        if let Some(config) = change.config.clone() {
            record.config = config;
        }
        let now = now_secs();
        for (slug, value) in &change.attention {
            record = match set_attention(&record, slug, value.as_ref(), now) {
                Ok(record) => record,
                Err(error) => return Task::ready(Err(error)),
            };
        }
        // As the spec's writer: an Interest stays while anything uses it.
        for (slug, value) in &change.attention {
            let Some((attention::Space::Interest, id)) = attention::parse_slug(slug) else {
                continue;
            };
            if value.is_none()
                && record
                    .attention
                    .values()
                    .any(|object| object.value.interest_id() == Some(id))
            {
                return Task::ready(Err(
                    "Remove or move the watches and timers that use this Interest first".into(),
                ));
            }
        }
        if (change.config.is_some() || !change.attention.is_empty())
            && let Err(error) = self.write(record)
        {
            return Task::ready(Err(error));
        }
        self.update(cx);
        cx.notify();
        let Some(name) = name.filter(|name| *name != identity.summary.name) else {
            return Task::ready(Ok(()));
        };
        let link = link(&identity.host, cx);
        let pubkey = pubkey.to_owned();
        cx.spawn(async move |this, cx| {
            link.app_agent(AppAgentRequest::Rename { pubkey, name })
                .await?;
            // Shows the new name and publishes it as the agent's profile.
            if let Ok(refresh) = this.update(cx, |this, cx| this.refresh(cx)) {
                refresh.await;
            }
            Ok(())
        })
    }

    /// Deletes its key at once. Leaving its channels and archiving it
    /// follow in the background whenever its community is open.
    pub fn remove(&mut self, pubkey: &str, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let Some(identity) = self.live(pubkey) else {
            return Task::ready(Err("No such agent on this device".into()));
        };
        let link = link(&identity.host, cx);
        let pubkey = pubkey.to_owned();
        cx.spawn(async move |this, cx| {
            link.app_agent(AppAgentRequest::Delete { pubkey }).await?;
            // Drops its settings, and starts its cleanup if its community
            // is open.
            if let Ok(refresh) = this.update(cx, |this, cx| this.refresh(cx)) {
                refresh.await;
            }
            Ok(())
        })
    }

    /// For [`super::AgentView::save`].
    pub(crate) fn save_config(
        pubkey: &str,
        config: Value,
        cx: &mut App,
    ) -> Task<Result<(), String>> {
        let Some(service) = Self::global(cx) else {
            return Task::ready(Err("Agents2 is not running".into()));
        };
        service.update(cx, |this, cx| {
            this.save(
                pubkey,
                AgentChange {
                    config: Some(config),
                    ..Default::default()
                },
                cx,
            )
        })
    }

    /// Channels a watch can be scoped to, by name, from the viewer's joined
    /// roster.
    pub fn channel_choices(&self, cx: &App) -> Vec<attention::ChannelChoice> {
        let Some(binding) = &self.binding else {
            return Vec::new();
        };
        let mut choices: Vec<_> = binding
            .community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .filter(|channel| !channel.read_only)
            .map(|channel| attention::ChannelChoice {
                id: channel.id.clone(),
                name: channel.name.clone(),
            })
            .collect();
        choices.sort_by_key(|choice| choice.name.to_lowercase());
        choices
    }
}

fn link(host: &Entity<HostConnection>, cx: &App) -> HostLink {
    HostLink {
        client: host.read(cx).client(),
        runtime: Runtime::handle(cx),
    }
}

/// Delivers the session's live events to the runners.
fn listen(session: &Arc<Session>, cx: &mut Context<Agents2>) -> Task<()> {
    let mut events = session.live_events();
    cx.spawn(async move |this, cx| {
        while let Some(batch) = events.recv().await {
            if this
                .update(cx, |this, cx| this.dispatch(batch, cx))
                .is_err()
            {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests;
