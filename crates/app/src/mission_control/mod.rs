//! Mission control: one live picture of every agent on every host this
//! app manages, with the agents that need their owner first.
//!
//! [`Fleet`] joins each host's agent snapshot with the activity the
//! owner's agents report in every open community, once a second and
//! whenever a host changes, into a [`model::Board`] the page draws. It
//! lives for the app, not the page, so an agent that fails, gets stuck or
//! waits on its owner raises a desktop notification wherever the window
//! is. Hosts send when an agent entered its status; for older hosts the
//! time this app first saw it stands in.

pub mod model;

use crate::agent_activity::observer::{self, now_ms};
use crate::community::{AppState, IdentityState};
use crate::hosts::{HostConnection, Hosts};
use crate::notifications::categories::{self, CategorySpec, Submission};
use crate::ui::navigation::{Place, Target};
use buzz_host::api::ProcessStatus;
use buzz_host_rpc::ConnectionState;
use buzz_relay_client::archive::community_key;
use buzz_relay_client::usage::{SavedMetrics, Scan, scan};
use gpui_kit::*;
use model::{Board, Entry, HostSummary, Key, Link, Live, Need, Tracker, Usage};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The notification category, with its own switch in Settings.
pub const CATEGORY: &str = "buzz.mission-control/attention";
/// After launch, evidence still arrives this long; what it shows is the
/// baseline, not news.
const QUIET: Duration = Duration::from_secs(20);
/// Saved turn metrics are read again at most this often.
const USAGE_EVERY: Duration = Duration::from_secs(300);

struct FleetHandle(Entity<Fleet>);

impl Global for FleetHandle {}

/// Starts following the fleet. Call after the hosts and notifications.
pub fn init(cx: &mut App) {
    categories::register(
        CategorySpec {
            key: CATEGORY,
            label: "Agents needing attention",
        },
        cx,
    );
    let fleet = cx.new(Fleet::new);
    cx.set_global(FleetHandle(fleet));
}

/// One community's evidence, kept while its activity is unchanged.
struct Seen {
    /// Held, so a new snapshot can never reuse its address.
    snapshot: Arc<crate::agent_activity::Snapshot>,
    lives: HashMap<String, Live>,
}

pub struct Fleet {
    board: Arc<Board>,
    tracker: Tracker,
    /// When this app first saw each agent in its status, for hosts that
    /// do not say.
    statuses: HashMap<Key, (ProcessStatus, u64)>,
    communities: HashMap<EntityId, Seen>,
    usage: HashMap<String, Usage>,
    usage_read: Option<Instant>,
    _usage: Option<Task<()>>,
    started: Instant,
    alerts: u64,
    observed: Vec<EntityId>,
    _hosts: Vec<Subscription>,
    _tick: Task<()>,
}

impl Fleet {
    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<FleetHandle>()
            .map(|handle| handle.0.clone())
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |this, cx| this.rebuild(cx)).is_err() {
                    return;
                }
            }
        });
        Self {
            board: Arc::default(),
            tracker: Tracker::default(),
            statuses: HashMap::new(),
            communities: HashMap::new(),
            usage: HashMap::new(),
            usage_read: None,
            _usage: None,
            started: Instant::now(),
            alerts: 0,
            observed: Vec::new(),
            _hosts: Vec::new(),
            _tick: tick,
        }
    }

    pub fn board(&self) -> Arc<Board> {
        self.board.clone()
    }

    /// Follows host changes as they happen rather than on the next tick.
    fn observe_hosts(&mut self, hosts: &[Entity<HostConnection>], cx: &mut Context<Self>) {
        let ids: Vec<EntityId> = hosts.iter().map(|host| host.entity_id()).collect();
        if ids == self.observed {
            return;
        }
        for gone in self.observed.iter().filter(|id| !ids.contains(id)) {
            self.tracker.forget_host(gone.as_u64());
        }
        self._hosts = hosts
            .iter()
            .map(|host| cx.observe(host, |this, _, cx| this.rebuild(cx)))
            .collect();
        self.observed = ids;
    }

    /// Each community's live evidence per agent, recomputed only for
    /// communities whose activity changed.
    fn lives(&mut self, now: u64, cx: &App) -> HashMap<String, Live> {
        let attached = observer::all(cx);
        self.communities
            .retain(|id, _| attached.iter().any(|(c, _)| c.entity_id() == *id));
        for (community, activity) in &attached {
            let snapshot = activity.read(cx).snapshot.clone();
            let fresh = self
                .communities
                .get(&community.entity_id())
                .is_some_and(|seen| Arc::ptr_eq(&seen.snapshot, &snapshot));
            if fresh {
                continue;
            }
            let origin = community.read(cx).origin.to_string();
            let mut agents: Vec<&str> = snapshot.turns.iter().map(|t| t.agent.as_str()).collect();
            agents.sort();
            agents.dedup();
            let lives = agents
                .into_iter()
                .filter_map(|agent| {
                    Some((
                        agent.to_string(),
                        model::live_in(&snapshot, &origin, agent, now)?,
                    ))
                })
                .collect();
            self.communities
                .insert(community.entity_id(), Seen { snapshot, lives });
        }
        let mut by_agent: HashMap<String, Vec<Live>> = HashMap::new();
        for seen in self.communities.values() {
            for (agent, live) in &seen.lives {
                by_agent
                    .entry(agent.clone())
                    .or_default()
                    .push(live.clone());
            }
        }
        by_agent
            .into_iter()
            .filter_map(|(agent, lives)| Some((agent, model::merge(lives)?)))
            .collect()
    }

    /// Recomputes the board; views hear of it only when it changed.
    pub fn rebuild(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let hosts = cx
            .try_global::<Hosts>()
            .map(|hosts| hosts.hosts.clone())
            .unwrap_or_default();
        self.observe_hosts(&hosts, cx);
        let lives = self.lives(now, cx);
        let mut entries = Vec::new();
        let mut summaries = Vec::new();
        for host in &hosts {
            let state = host.read(cx);
            let key = host.entity_id().as_u64();
            let (link, detail) = match &state.state {
                ConnectionState::Connected { .. } => (Link::Connected, None),
                ConnectionState::Connecting { .. } => (Link::Connecting, None),
                ConnectionState::Disconnected { error, .. } => (Link::Offline, Some(error.clone())),
                ConnectionState::Rejected { reason } => (Link::Refused, Some(reason.clone())),
            };
            let first = entries.len();
            for agent in &state.agents.agents {
                let key = Key {
                    host: key,
                    id: agent.id.clone(),
                };
                let seen = self
                    .statuses
                    .entry(key.clone())
                    .or_insert((agent.status, now));
                if seen.0 != agent.status {
                    *seen = (agent.status, now);
                }
                // When this app first saw a running agent is not when it
                // started, so it cannot tell a restart after a turn; it
                // stands in only for how long a transition has taken.
                let since = agent
                    .status_since
                    .or((agent.status != ProcessStatus::Running).then_some(seen.1));
                let live = lives.get(&agent.pubkey);
                let model = agent
                    .launch_model
                    .clone()
                    .or_else(|| Some(agent.harness.model.clone()))
                    .filter(|model| !model.is_empty());
                entries.push(Entry {
                    key,
                    host_label: state.label.to_string(),
                    remote: state.remote.is_some(),
                    pubkey: agent.pubkey.clone(),
                    name: agent.name.clone(),
                    picture: agent.picture.clone().filter(|p| !p.is_empty()),
                    model,
                    status: agent.status,
                    link,
                    attention: model::attention(agent, since, live, link, now),
                    doing: model::doing(agent, since, live, link, now),
                    usage: model::usage_line(self.usage.get(&agent.pubkey), live),
                    place: live.and_then(|live| live.place.clone()),
                });
            }
            let mine = &entries[first..];
            summaries.push(HostSummary {
                key,
                label: state.label.to_string(),
                address: state.remote.as_ref().map(|remote| remote.address.clone()),
                link,
                detail,
                total: mine.len(),
                running: mine
                    .iter()
                    .filter(|e| e.status == ProcessStatus::Running)
                    .count(),
                attention: mine.iter().filter(|e| e.attention.is_some()).count(),
            });
        }
        self.statuses
            .retain(|key, _| entries.iter().any(|entry| entry.key == *key));
        model::sort(&mut entries);
        let quiet = self.started.elapsed() < QUIET;
        for alert in self.tracker.observe(&entries, quiet) {
            self.notify(alert, cx);
        }
        if self
            .usage_read
            .is_none_or(|read| read.elapsed() >= USAGE_EVERY)
        {
            self.read_usage(cx);
        }
        let board = Board {
            hosts: summaries,
            entries,
        };
        if board != *self.board {
            // The sidebar's count draws from its last frame otherwise.
            let recount = board.attention() != self.board.attention();
            self.board = Arc::new(board);
            cx.notify();
            if recount {
                cx.refresh_windows();
            }
        }
    }

    /// A desktop notification through the plugin categories, so the
    /// viewer's alert and sound choices apply.
    fn notify(&mut self, alert: model::Alert, cx: &mut Context<Self>) {
        self.alerts += 1;
        let title = match alert.attention.need {
            Need::Failed => format!("{} failed", alert.name),
            Need::Waiting => format!("{} needs you", alert.name),
            Need::Stuck => format!("{} looks stuck", alert.name),
        };
        let community = AppState::current(cx).map(|c| c.read(cx).origin.to_string());
        categories::submit(
            CATEGORY,
            Submission {
                source_key: format!(
                    "{}:{}:{:?}:{}",
                    alert.key.host, alert.key.id, alert.attention.need, self.alerts
                ),
                title,
                body: format!("{} · {}", alert.attention.reason, alert.host_label),
                target: Target {
                    community,
                    place: Place::Page(crate::ui::mission_control::PAGE),
                },
            },
            cx,
        );
    }

    /// Reads the viewer's saved turn metrics in the selected community off
    /// the UI thread, for each agent's usage over the last day.
    pub fn read_usage(&mut self, cx: &mut Context<Self>) {
        let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
            return;
        };
        let (Some(archive), Some(community)) = (
            crate::ui::settings::agent_archive(cx),
            AppState::current(cx),
        ) else {
            return;
        };
        let source = SavedMetrics {
            archive,
            identity: identity.clone(),
            community: community_key(&community.read(cx).origin),
        };
        self.usage_read = Some(Instant::now());
        let since = now_ms().saturating_sub(model::USAGE_WINDOW_MS) as i64;
        let read = cx.background_spawn(async move {
            match scan(&source, || true) {
                Scan::Ready { records, .. } => Some(model::usage_by_agent(&records, since)),
                _ => None,
            }
        });
        self._usage = Some(cx.spawn(async move |this, cx| {
            let Some(usage) = read.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.usage = usage;
                this.rebuild(cx);
            });
        }));
    }
}
