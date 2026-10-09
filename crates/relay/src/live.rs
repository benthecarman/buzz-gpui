//! Live traffic for one community: which subscriptions exist and how they
//! recover (`live.ts`, spec §3.3–3.6).
//!
//! A *route* is a logical interest: the global `profiles` and `membership`
//! feeds, presence, a single channel, or a batch of up to ten joined
//! channels. A *wire* is the REQ currently carrying a route; replacing a
//! wire keeps the old one open until the new one reaches EOSE, so no events
//! are missed while a batch changes shape.
//!
//! After a reconnect every route is rebuilt with a fresh five-minute
//! look-back rather than resumed; the session follows up with HTTP
//! catch-up reads for anything older.

use crate::event::{Verified, Verifier, kinds};
use crate::origin::RelayOrigin;
use crate::socket::{Filter, RelaySocket, SocketOptions, SocketState, SubscriptionEvent};
use buzz_identity::Identity;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_stream::StreamMap;

#[derive(Clone, Debug)]
pub struct LiveOptions {
    pub batch_size: usize,
    pub channel_capacity: usize,
    pub replay_limit: u64,
    pub lookback: Duration,
    pub setup_concurrency: usize,
    pub setup_deadline: Duration,
    pub recovery_interval: Duration,
    pub max_quota_retries: u32,
    pub max_presence_authors: usize,
}

impl Default for LiveOptions {
    fn default() -> Self {
        Self {
            batch_size: 10,
            channel_capacity: 1022,
            replay_limit: 500,
            lookback: Duration::from_secs(300),
            setup_concurrency: 4,
            setup_deadline: Duration::from_secs(10),
            recovery_interval: Duration::from_secs(60),
            max_quota_retries: 3,
            max_presence_authors: 256,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RouteKey {
    Profiles,
    Membership,
    Presence,
    Channel(String),
    Batch(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Stored events sent before EOSE.
    Replay,
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteStatus {
    Pending,
    SettingUp,
    Live,
    Error(String),
    /// Over the channel capacity; finite reads still work.
    Limited,
}

#[derive(Debug)]
pub enum LiveUpdate {
    /// Verified events, with the channels the carrying route covers.
    Events {
        events: Vec<Verified>,
        phase: Phase,
        scope: Arc<[String]>,
    },
    Presence(Vec<Verified>),
    /// A route reached EOSE. For channel routes the session catches up
    /// those channels over HTTP; for globals it refreshes global state.
    Established {
        route: RouteKey,
        channels: Vec<String>,
    },
    /// `restricted: not a channel member`: access to these channels is gone.
    Denied {
        channels: Vec<String>,
        reason: String,
    },
    /// `restricted: channel access revoked`: a hint to re-read the roster,
    /// not proof of removal.
    AccessHint {
        channels: Vec<String>,
    },
    Connected,
    /// The socket dropped; everything live is stale until `Connected`.
    Disconnected,
    /// Route statuses changed; read them with [`Live::statuses`].
    Status,
}

struct Route {
    key: RouteKey,
    channels: Vec<String>,
    since: u64,
    status: RouteStatus,
    wire: Option<u64>,
    /// The wire being replaced, kept until `wire` reaches EOSE.
    previous: Option<u64>,
    setup_deadline: Option<Instant>,
    quota_retries: u32,
    replayed: u64,
    /// Replacement wires ask only for new events.
    live_only: bool,
}

struct Wire {
    route: RouteKey,
    scope: Arc<[String]>,
    eose: bool,
}

pub struct Live {
    socket: RelaySocket,
    socket_state: watch::Receiver<SocketState>,
    viewer: String,
    options: LiveOptions,
    connected: bool,
    routes: BTreeMap<RouteKey, Route>,
    wires: HashMap<u64, Wire>,
    streams: StreamMap<u64, crate::socket::Subscription>,
    next_wire: u64,
    next_batch: u64,
    /// Inputs, kept so routes can be rebuilt after a reconnect.
    interests: BTreeSet<String>,
    joined: BTreeSet<String>,
    prioritized: Vec<String>,
    presence_authors: Vec<String>,
    cooldown: Option<Instant>,
    recovery: Instant,
    outbox: VecDeque<LiveUpdate>,
}

impl Live {
    pub fn connect(
        runtime: &tokio::runtime::Handle,
        origin: RelayOrigin,
        identity: Arc<Identity>,
        verifier: Arc<Verifier>,
        options: LiveOptions,
    ) -> Self {
        let viewer = identity.pubkey_hex();
        let socket = RelaySocket::connect(
            runtime,
            origin,
            identity,
            verifier,
            SocketOptions {
                resubscribe: false,
                ..Default::default()
            },
        );
        Self {
            socket_state: socket.state(),
            socket,
            viewer,
            recovery: Instant::now() + options.recovery_interval,
            options,
            connected: false,
            routes: BTreeMap::new(),
            wires: HashMap::new(),
            streams: StreamMap::new(),
            next_wire: 1,
            next_batch: 1,
            interests: BTreeSet::new(),
            joined: BTreeSet::new(),
            prioritized: Vec::new(),
            presence_authors: Vec::new(),
            cooldown: None,
            outbox: VecDeque::new(),
        }
    }

    pub fn socket(&self) -> &RelaySocket {
        &self.socket
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn statuses(&self) -> Vec<(RouteKey, RouteStatus)> {
        self.routes
            .values()
            .map(|route| (route.key.clone(), route.status.clone()))
            .collect()
    }

    /// Sets which channels need live traffic. `joined` channels may share
    /// batched routes; `prioritized` ones (open or previewed, at most 64)
    /// get their own routes and are set up first.
    pub fn update(
        &mut self,
        interests: impl IntoIterator<Item = String>,
        joined: impl IntoIterator<Item = String>,
        prioritized: Vec<String>,
    ) {
        self.interests = interests.into_iter().collect();
        self.joined = joined.into_iter().collect();
        self.prioritized = prioritized.into_iter().take(64).collect();
        if self.connected {
            self.sync();
        }
    }

    /// Authors whose presence to follow live (at most 256).
    pub fn watch_presence(&mut self, mut authors: Vec<String>) {
        authors.sort();
        authors.dedup();
        authors.truncate(self.options.max_presence_authors);
        if authors == self.presence_authors {
            return;
        }
        self.presence_authors = authors;
        if self.connected {
            self.remove_route(&RouteKey::Presence);
            if !self.presence_authors.is_empty() {
                self.add_route(RouteKey::Presence, Vec::new());
            }
            self.pump();
        }
    }

    /// The user's "Retry live updates": routes in error get another go.
    pub fn retry(&mut self) {
        self.cooldown = None;
        for route in self.routes.values_mut() {
            if matches!(route.status, RouteStatus::Error(_)) {
                route.status = RouteStatus::Pending;
                route.quota_retries = 0;
            }
        }
        if self.connected {
            self.pump();
        } else {
            self.socket.reconnect();
        }
    }

    /// Waits for the next update. Cancel-safe: call it in a `select!`.
    pub async fn next(&mut self) -> LiveUpdate {
        loop {
            if let Some(update) = self.outbox.pop_front() {
                return update;
            }
            let setup = self
                .routes
                .values()
                .filter_map(|route| route.setup_deadline)
                .min();
            let cooldown = self.cooldown;
            tokio::select! {
                changed = self.socket_state.changed() => {
                    if changed.is_err() {
                        return LiveUpdate::Disconnected;
                    }
                    let ready = *self.socket_state.borrow_and_update() == SocketState::Ready;
                    self.connection_changed(ready);
                }
                Some((wire, event)) = self.streams.next(), if !self.streams.is_empty() => {
                    self.wire_event(wire, event);
                }
                _ = sleep_until(setup) => self.expire_setups(),
                _ = sleep_until(cooldown) => {
                    self.cooldown = None;
                    self.pump();
                }
                _ = tokio::time::sleep_until(self.recovery) => self.recover(),
            }
        }
    }

    fn connection_changed(&mut self, ready: bool) {
        if ready == self.connected {
            return;
        }
        self.connected = ready;
        // Rebuilt from inputs either way: a fresh look-back after a
        // reconnect, nothing while disconnected.
        self.routes.clear();
        self.wires.clear();
        self.streams = StreamMap::new();
        if ready {
            self.recovery = Instant::now() + self.options.recovery_interval;
            self.add_route(RouteKey::Profiles, Vec::new());
            self.add_route(RouteKey::Membership, Vec::new());
            if !self.presence_authors.is_empty() {
                self.add_route(RouteKey::Presence, Vec::new());
            }
            self.sync();
            self.outbox.push_back(LiveUpdate::Connected);
        } else {
            self.outbox.push_back(LiveUpdate::Disconnected);
        }
    }

    /// Reconciles channel routes with the inputs without rebalancing
    /// healthy batches.
    fn sync(&mut self) {
        let capacity =
            self.options.channel_capacity - usize::from(!self.presence_authors.is_empty());
        let wanted: Vec<String> = self
            .prioritized
            .iter()
            .filter(|id| self.interests.contains(*id))
            .cloned()
            .chain(
                self.interests
                    .iter()
                    .filter(|id| !self.prioritized.contains(id))
                    .cloned(),
            )
            .collect();
        let (served, limited) = wanted.split_at(wanted.len().min(capacity));
        let singleton = |id: &String| !self.joined.contains(id) || self.prioritized.contains(id);
        let singles: BTreeSet<String> = served.iter().filter(|id| singleton(id)).cloned().collect();
        let batched: BTreeSet<String> =
            served.iter().filter(|id| !singleton(id)).cloned().collect();

        // Drop channel routes no longer wanted in their current class.
        let stale: Vec<RouteKey> = self
            .routes
            .keys()
            .filter(|key| match key {
                RouteKey::Channel(id) => !singles.contains(id) && !limited.contains(id),
                _ => false,
            })
            .cloned()
            .collect();
        for key in stale {
            self.remove_route(&key);
        }
        for id in limited {
            let key = RouteKey::Channel(id.clone());
            if !self.routes.contains_key(&key) {
                self.routes.insert(
                    key.clone(),
                    Route {
                        status: RouteStatus::Limited,
                        ..self.route(key, vec![id.clone()])
                    },
                );
            }
        }
        for id in &singles {
            let key = RouteKey::Channel(id.clone());
            if !self.routes.contains_key(&key) {
                self.add_route(key, vec![id.clone()]);
            }
        }

        // Shrink batches that lost members; collect what is already batched.
        let mut covered = BTreeSet::new();
        let batches: Vec<RouteKey> = self
            .routes
            .keys()
            .filter(|key| matches!(key, RouteKey::Batch(_)))
            .cloned()
            .collect();
        for key in batches {
            let channels = self.routes[&key].channels.clone();
            let survivors: Vec<String> = channels
                .iter()
                .filter(|id| batched.contains(*id))
                .cloned()
                .collect();
            if survivors.is_empty() {
                self.remove_route(&key);
            } else if survivors.len() < channels.len() {
                covered.extend(survivors.iter().cloned());
                self.replace(&key, Some(survivors));
            } else {
                covered.extend(survivors);
            }
        }
        let fresh: Vec<String> = batched.difference(&covered).cloned().collect();
        for chunk in fresh.chunks(self.options.batch_size) {
            let key = RouteKey::Batch(self.next_batch);
            self.next_batch += 1;
            self.add_route(key, chunk.to_vec());
        }
        self.pump();
        self.outbox.push_back(LiveUpdate::Status);
    }

    fn route(&self, key: RouteKey, channels: Vec<String>) -> Route {
        Route {
            key,
            channels,
            since: now_secs().saturating_sub(self.options.lookback.as_secs()),
            status: RouteStatus::Pending,
            wire: None,
            previous: None,
            setup_deadline: None,
            quota_retries: 0,
            replayed: 0,
            live_only: false,
        }
    }

    fn add_route(&mut self, key: RouteKey, channels: Vec<String>) {
        let route = self.route(key.clone(), channels);
        self.routes.insert(key, route);
    }

    fn remove_route(&mut self, key: &RouteKey) {
        if let Some(route) = self.routes.remove(key) {
            for wire in route.wire.into_iter().chain(route.previous) {
                self.close_wire(wire);
            }
        }
    }

    fn close_wire(&mut self, wire: u64) {
        // Dropping the subscription sends CLOSE; the wire is fenced first so
        // late frames for it are ignored.
        self.wires.remove(&wire);
        self.streams.remove(&wire);
    }

    /// Re-sends a route on a fresh wire asking only for new events, keeping
    /// the old wire until the new one is established.
    fn replace(&mut self, key: &RouteKey, channels: Option<Vec<String>>) {
        let Some(route) = self.routes.get_mut(key) else {
            return;
        };
        if let Some(channels) = channels {
            route.channels = channels;
        }
        if let Some(previous) = route.previous.take() {
            self.wires.remove(&previous);
            self.streams.remove(&previous);
        }
        let route = self.routes.get_mut(key).expect("route exists");
        route.previous = route.wire.take();
        route.live_only = true;
        route.status = RouteStatus::Pending;
    }

    fn filters(&self, route: &Route) -> Vec<Filter> {
        let limit = if route.live_only {
            0
        } else {
            self.options.replay_limit
        };
        let channel = |ids: Vec<&String>| {
            object(json!({
                "kinds": kinds::CHANNEL_KINDS,
                "#h": ids,
                "since": route.since,
                "limit": limit,
            }))
        };
        match &route.key {
            RouteKey::Profiles => vec![
                object(
                    json!({"kinds": [kinds::PROFILE, kinds::AGENT_METADATA, kinds::MANAGED_AGENT], "since": route.since, "limit": limit}),
                ),
                object(
                    json!({"kinds": [kinds::USER_STATUS], "#d": ["general"], "since": route.since, "limit": limit}),
                ),
                object(
                    json!({"kinds": [kinds::CUSTOM_EMOJI], "#d": ["buzz:custom-emoji"], "since": route.since, "limit": limit}),
                ),
            ],
            RouteKey::Membership => vec![
                object(
                    json!({"kinds": [kinds::MEMBER_ADDED, kinds::MEMBER_REMOVED], "#p": [self.viewer], "since": route.since, "limit": limit}),
                ),
                object(
                    json!({"kinds": [kinds::APP_DATA], "authors": [self.viewer], "#t": ["read-state"], "since": route.since, "limit": limit}),
                ),
                object(
                    json!({"kinds": [kinds::REMINDERS], "authors": [self.viewer], "since": route.since, "limit": limit}),
                ),
            ],
            RouteKey::Presence => vec![object(json!({
                "kinds": [kinds::PRESENCE],
                "authors": self.presence_authors,
                "since": route.since,
                "limit": 0,
            }))],
            RouteKey::Channel(_) => vec![channel(route.channels.iter().collect())],
            // One filter per channel on first open; replacements use one
            // filter naming all survivors.
            RouteKey::Batch(_) if route.live_only => vec![channel(route.channels.iter().collect())],
            RouteKey::Batch(_) => route.channels.iter().map(|id| channel(vec![id])).collect(),
        }
    }

    /// Starts pending routes: globals first, then prioritized channels, at
    /// most `setup_concurrency` awaiting EOSE.
    fn pump(&mut self) {
        if !self.connected || self.cooldown.is_some_and(|until| until > Instant::now()) {
            return;
        }
        let rank = |key: &RouteKey, prioritized: &[String]| match key {
            RouteKey::Profiles | RouteKey::Membership => 0,
            RouteKey::Presence => 1,
            RouteKey::Channel(id) => match prioritized.iter().position(|p| p == id) {
                Some(index) => 2 + index,
                None => 100,
            },
            RouteKey::Batch(_) => 101,
        };
        loop {
            let setting_up = self
                .routes
                .values()
                .filter(|route| route.status == RouteStatus::SettingUp)
                .count();
            if setting_up >= self.options.setup_concurrency {
                return;
            }
            let Some(key) = self
                .routes
                .values()
                .filter(|route| route.status == RouteStatus::Pending)
                .min_by_key(|route| rank(&route.key, &self.prioritized))
                .map(|route| route.key.clone())
            else {
                return;
            };
            let filters = self.filters(&self.routes[&key]);
            let subscription = self.socket.subscribe(filters);
            let wire = self.next_wire;
            self.next_wire += 1;
            let route = self.routes.get_mut(&key).expect("pending route exists");
            route.wire = Some(wire);
            route.status = RouteStatus::SettingUp;
            route.setup_deadline = Some(Instant::now() + self.options.setup_deadline);
            route.replayed = 0;
            self.wires.insert(
                wire,
                Wire {
                    route: key.clone(),
                    scope: route.channels.clone().into(),
                    eose: false,
                },
            );
            self.streams.insert(wire, subscription);
        }
    }

    fn wire_event(&mut self, wire: u64, event: SubscriptionEvent) {
        let Some(state) = self.wires.get_mut(&wire) else {
            return;
        };
        let key = state.route.clone();
        let is_current = self
            .routes
            .get(&key)
            .is_some_and(|route| route.wire == Some(wire));
        match event {
            SubscriptionEvent::Event(event) => {
                let scope = state.scope.clone();
                let phase = if state.eose || self.routes.get(&key).is_some_and(|r| r.live_only) {
                    Phase::Live
                } else {
                    Phase::Replay
                };
                if !state.eose
                    && let Some(route) = self.routes.get_mut(&key)
                {
                    route.replayed += 1;
                }
                self.deliver(&key, scope, phase, event);
            }
            SubscriptionEvent::Eose => {
                state.eose = true;
                if !is_current {
                    return;
                }
                let route = self.routes.get_mut(&key).expect("current route exists");
                route.status = RouteStatus::Live;
                route.setup_deadline = None;
                route.quota_retries = 0;
                let channels = route.channels.clone();
                if let Some(previous) = route.previous.take() {
                    self.close_wire(previous);
                }
                if !matches!(key, RouteKey::Presence) {
                    self.outbox.push_back(LiveUpdate::Established {
                        route: key,
                        channels,
                    });
                }
                self.outbox.push_back(LiveUpdate::Status);
                self.pump();
            }
            SubscriptionEvent::Closed(reason) => {
                self.wires.remove(&wire);
                if is_current {
                    self.closed(&key, wire, reason);
                } else if let Some(route) = self.routes.get_mut(&key)
                    && route.previous == Some(wire)
                {
                    route.previous = None;
                }
            }
            SubscriptionEvent::Lost | SubscriptionEvent::Resubscribed => {}
        }
    }

    fn deliver(&mut self, key: &RouteKey, scope: Arc<[String]>, phase: Phase, event: Verified) {
        match key {
            RouteKey::Presence => {
                if event.kind_u16() == kinds::PRESENCE
                    && self.presence_authors.contains(&event.pubkey_hex())
                {
                    self.outbox.push_back(LiveUpdate::Presence(vec![event]));
                }
            }
            RouteKey::Profiles | RouteKey::Membership => {
                self.outbox.push_back(LiveUpdate::Events {
                    events: vec![event],
                    phase,
                    scope,
                });
            }
            RouteKey::Channel(_) | RouteKey::Batch(_) => {
                // Events naming channels outside this wire's scope are not
                // ours to deliver; events with no channel (reactions on some
                // relays) are attributed to the wire's scope.
                let in_scope = match event.channel() {
                    Some(channel) => scope.iter().any(|id| id == channel),
                    None => {
                        event.tags_named("h").next().is_none() && event.kind_u16() != kinds::TYPING
                    }
                };
                if in_scope {
                    self.outbox.push_back(LiveUpdate::Events {
                        events: vec![event],
                        phase,
                        scope,
                    });
                }
            }
        }
    }

    fn closed(&mut self, key: &RouteKey, wire: u64, reason: String) {
        let reason: String = reason.chars().take(512).collect();
        self.streams.remove(&wire);
        let max_retries = self.options.max_quota_retries;
        let Some(route) = self.routes.get_mut(key) else {
            return;
        };
        route.wire = None;
        route.setup_deadline = None;
        let channels = route.channels.clone();
        let mut pause = None;
        if reason.starts_with("rate-limited:") {
            let hint = quota_retry(&reason);
            match hint {
                // Quietly retried once the cooldown passes.
                Some(seconds) if seconds <= 60 && route.quota_retries < max_retries => {
                    route.quota_retries += 1;
                    route.status = RouteStatus::Pending;
                    pause = Some(seconds);
                }
                _ => {
                    route.status = RouteStatus::Error(
                        "Live request cooldown retries exhausted; retry available".into(),
                    );
                    pause = Some(hint.unwrap_or(5).min(60));
                }
            }
        } else if reason.starts_with("restricted: not a channel member") {
            route.status = RouteStatus::Error(reason.clone());
            self.outbox
                .push_back(LiveUpdate::Denied { channels, reason });
        } else if reason.starts_with("restricted: channel access revoked") {
            route.status = RouteStatus::Error(reason);
            self.outbox.push_back(LiveUpdate::AccessHint { channels });
        } else {
            route.status = RouteStatus::Error(reason);
        }
        if let Some(seconds) = pause {
            // One extra second covers the relay's whole-second rounding.
            self.pause(Duration::from_secs(seconds + 1));
        }
        self.outbox.push_back(LiveUpdate::Status);
        self.pump();
    }

    fn pause(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        self.cooldown = Some(self.cooldown.map_or(until, |current| current.max(until)));
    }

    fn expire_setups(&mut self) {
        let now = Instant::now();
        let expired: Vec<RouteKey> = self
            .routes
            .values()
            .filter(|route| route.setup_deadline.is_some_and(|deadline| deadline <= now))
            .map(|route| route.key.clone())
            .collect();
        for key in expired {
            let route = self.routes.get_mut(&key).expect("route exists");
            route.setup_deadline = None;
            if let Some(wire) = route.wire.take() {
                self.close_wire(wire);
            }
            let route = self.routes.get_mut(&key).expect("route exists");
            // A replacement that timed out keeps its working previous wire.
            route.status = if route.previous.is_some() {
                RouteStatus::Live
            } else {
                RouteStatus::Error("Live subscription setup timed out; retry available".into())
            };
        }
        self.outbox.push_back(LiveUpdate::Status);
        self.pump();
    }

    /// The minute tick: renews healthy batches on fresh wires so a relay
    /// that silently pruned a subscription is noticed, and lets the session
    /// re-check rosters.
    fn recover(&mut self) {
        self.recovery = Instant::now() + self.options.recovery_interval;
        if !self.connected {
            return;
        }
        let batches: Vec<RouteKey> = self
            .routes
            .values()
            .filter(|route| {
                matches!(route.key, RouteKey::Batch(_))
                    && route.status == RouteStatus::Live
                    && route.previous.is_none()
            })
            .map(|route| route.key.clone())
            .collect();
        for key in batches {
            self.replace(&key, None);
        }
        self.pump();
    }
}

/// `rate-limited: quota exceeded; retry in Ns` → N.
fn quota_retry(reason: &str) -> Option<u64> {
    reason
        .strip_prefix("rate-limited: quota exceeded; retry in ")?
        .strip_suffix('s')?
        .parse()
        .ok()
}

fn object(value: Value) -> Filter {
    match value {
        Value::Object(map) => map,
        _ => unreachable!("filters are objects"),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => futures::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_quota_hints() {
        assert_eq!(
            super::quota_retry("rate-limited: quota exceeded; retry in 7s"),
            Some(7)
        );
        assert_eq!(
            super::quota_retry("rate-limited: shared admission unavailable"),
            None
        );
    }
}
