//! One community session (`session.ts`): everything the app knows about one
//! relay for one viewer.
//!
//! An actor on the I/O runtime owns the relay connection, the reader and
//! every feature store, and handles one message at a time, so the stores
//! need no locks and a read result can never interleave with another
//! update. Reads run as separate tasks and report back as messages, fenced
//! by an access epoch so a result started before a revocation is dropped.
//!
//! The UI reads immutable snapshots from watch channels: one for the
//! session (status, channels, profiles) and one per open channel.

mod timeline;

pub use timeline::{TimelineSnapshot, TimelineStatus};

use crate::discovery::{DiscoveryState, RosterScan, ScanOptions, ScanStage, membership_hint};
use crate::event::{Verified, Verifier, kinds};
use crate::http::RelayHttp;
use crate::live::{Live, LiveOptions, LiveUpdate};
use crate::origin::RelayOrigin;
use crate::profiles::{ProfileDirectory, Profiles};
use crate::reader::{ReadError, ReadErrorKind, ReadOptions, Reader, ReaderOptions};
use crate::socket::Filter;
use crate::window::{Cursor, page_filter, parse_page};
use crate::{ChannelSummary, Events};
use buzz_identity::Identity;
use nostr::prelude::PublicKey;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use timeline::Timeline;
use tokio::sync::{mpsc, watch};

/// Closed channels kept warm for a quick return.
const RETAINED_CLOSED: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Connecting,
    Ready,
    /// Could not reach the relay or it did not identify itself. Retried
    /// automatically with backoff, or at once with [`Session::retry`].
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosterStatus {
    Loading,
    Ready,
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RelayInfo {
    pub name: Option<String>,
    pub icon: Option<String>,
    /// The relay's own signing key (NIP-11 `self`).
    pub authority: String,
    pub supports_channel_creation: bool,
}

#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub status: SessionStatus,
    /// The live socket is authenticated; otherwise views may be stale.
    pub live: bool,
    pub relay: Option<RelayInfo>,
    pub roster: RosterStatus,
    /// Channels the viewer is a member of, DMs included.
    pub channels: Arc<[ChannelSummary]>,
    pub profiles: Profiles,
}

pub struct Session {
    commands: mpsc::UnboundedSender<Message>,
    snapshot: watch::Receiver<Arc<SessionSnapshot>>,
    viewer: String,
    origin: RelayOrigin,
    task: tokio::task::JoinHandle<()>,
}

/// An open channel. Dropping it releases the view; the session keeps a few
/// recently closed channels warm.
pub struct ChannelView {
    pub snapshot: watch::Receiver<Arc<TimelineSnapshot>>,
    channel: String,
    commands: mpsc::UnboundedSender<Message>,
}

impl ChannelView {
    pub fn load_older(&self) {
        let _ = self.commands.send(Message::LoadOlder(self.channel.clone()));
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }
}

impl Drop for ChannelView {
    fn drop(&mut self) {
        let _ = self.commands.send(Message::Close(self.channel.clone()));
    }
}

impl Session {
    pub fn open(
        runtime: &tokio::runtime::Handle,
        origin: RelayOrigin,
        identity: Arc<Identity>,
    ) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        let snapshot = watch::Sender::new(Arc::new(SessionSnapshot {
            status: SessionStatus::Connecting,
            live: false,
            relay: None,
            roster: RosterStatus::Loading,
            channels: Arc::new([]),
            profiles: Profiles::default(),
        }));
        let receiver = snapshot.subscribe();
        let verifier = Arc::new(Verifier::default());
        let http = RelayHttp::new(origin.clone(), identity.clone());
        let reader = Reader::spawn(
            runtime,
            Arc::new(http.clone()),
            verifier.clone(),
            ReaderOptions::default(),
        );
        let viewer = identity.pubkey_hex();
        let actor = Actor {
            runtime: runtime.clone(),
            origin: origin.clone(),
            identity,
            verifier,
            http,
            reader,
            commands: commands.clone(),
            snapshot,
            live: None,
            relay: None,
            discovery: None,
            scan: None,
            rescan: None,
            roster: RosterStatus::Loading,
            epoch: 0,
            timelines: HashMap::new(),
            closed: VecDeque::new(),
            profiles: ProfileDirectory::default(),
            selected: None,
            connect_attempt: 0,
        };
        let task = runtime.spawn(actor.run(inbox));
        Self {
            commands,
            snapshot: receiver,
            viewer,
            origin,
            task,
        }
    }

    pub fn snapshot(&self) -> watch::Receiver<Arc<SessionSnapshot>> {
        self.snapshot.clone()
    }

    pub fn viewer(&self) -> &str {
        &self.viewer
    }

    pub fn origin(&self) -> &RelayOrigin {
        &self.origin
    }

    /// Never blocks: the view starts at a loading snapshot and fills in.
    pub fn open_channel(&self, channel: &str) -> ChannelView {
        let (view, snapshot) = watch::channel(Arc::new(TimelineSnapshot::loading(channel)));
        let _ = self.commands.send(Message::Open {
            channel: channel.to_string(),
            view,
        });
        ChannelView {
            snapshot,
            channel: channel.to_string(),
            commands: self.commands.clone(),
        }
    }

    /// The channel in front of the user: its live route and catch-up go
    /// first.
    pub fn select(&self, channel: Option<String>) {
        let _ = self.commands.send(Message::Select(channel));
    }

    pub fn retry(&self) {
        let _ = self.commands.send(Message::Retry);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum Message {
    Open {
        channel: String,
        view: watch::Sender<Arc<TimelineSnapshot>>,
    },
    Close(String),
    LoadOlder(String),
    Select(Option<String>),
    Retry,
    Connected(Result<RelayInfo, String>),
    ScanRead {
        epoch: u64,
        result: Result<Events, ReadError>,
    },
    Page {
        channel: String,
        generation: u64,
        sent: Option<Cursor>,
        catch_up: bool,
        result: Result<Events, ReadError>,
    },
    Profiles {
        ids: Vec<String>,
        result: Result<Events, ReadError>,
    },
    Reconnect,
}

struct Actor {
    runtime: tokio::runtime::Handle,
    origin: RelayOrigin,
    identity: Arc<Identity>,
    verifier: Arc<Verifier>,
    http: RelayHttp,
    reader: Reader,
    commands: mpsc::UnboundedSender<Message>,
    snapshot: watch::Sender<Arc<SessionSnapshot>>,
    live: Option<Live>,
    relay: Option<RelayInfo>,
    discovery: Option<DiscoveryState>,
    scan: Option<RosterScan>,
    /// Another scan was asked for while one ran.
    rescan: Option<ScanOptions>,
    roster: RosterStatus,
    /// Bumped on revocation and reconnect; fences stale read results.
    epoch: u64,
    timelines: HashMap<String, Timeline>,
    /// Closed channels kept warm, most recent last.
    closed: VecDeque<String>,
    profiles: ProfileDirectory,
    selected: Option<String>,
    connect_attempt: u32,
}

impl Actor {
    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Message>) {
        self.connect();
        loop {
            let message = match &mut self.live {
                Some(live) => tokio::select! {
                    message = inbox.recv() => message,
                    update = live.next() => {
                        self.live_update(update);
                        continue;
                    }
                },
                None => inbox.recv().await,
            };
            let Some(message) = message else {
                return;
            };
            self.handle(message);
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Open { channel, view } => self.open(&channel, view),
            Message::Close(channel) => self.close(&channel),
            Message::LoadOlder(channel) => self.load_older(&channel),
            Message::Select(channel) => {
                self.selected = channel;
                self.update_interests();
            }
            Message::Retry => {
                if self.relay.is_none() {
                    self.connect();
                } else {
                    if let Some(live) = &mut self.live {
                        live.retry();
                    }
                    self.start_scan(ScanOptions {
                        strong: true,
                        ..Default::default()
                    });
                }
            }
            Message::Reconnect => {
                if self.relay.is_none() {
                    self.connect();
                }
            }
            Message::Connected(result) => self.connected(result),
            Message::ScanRead { epoch, result } => {
                if epoch == self.epoch {
                    self.scan_read(result);
                }
            }
            Message::Page {
                channel,
                generation,
                sent,
                catch_up,
                result,
            } => self.page(&channel, generation, sent, catch_up, result),
            Message::Profiles { ids, result } => {
                self.profiles.finished(&ids);
                if let Ok(events) = result {
                    let wanted: std::collections::HashSet<&String> = ids.iter().collect();
                    let events: Vec<Verified> = events
                        .iter()
                        .filter(|event| wanted.contains(&event.pubkey_hex()))
                        .cloned()
                        .collect();
                    if self.profiles.accept(&events) {
                        self.publish();
                    }
                }
            }
        }
    }

    // ----- connection -----

    /// Reads NIP-11 to learn the relay's signing key, then starts live
    /// traffic and the roster scan.
    fn connect(&mut self) {
        self.set_status(SessionStatus::Connecting);
        let http = self.http.clone();
        let commands = self.commands.clone();
        self.runtime.spawn(async move {
            let info = tokio::time::timeout(Duration::from_secs(8), http.information()).await;
            let result = match info {
                Ok(Ok(info)) => relay_info(&info),
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Err("Relay connection timed out. Try again.".to_string()),
            };
            let _ = commands.send(Message::Connected(result));
        });
    }

    fn connected(&mut self, result: Result<RelayInfo, String>) {
        let info = match result {
            Ok(info) => info,
            Err(error) => {
                self.connect_attempt += 1;
                let delay = Duration::from_secs(2u64.pow(self.connect_attempt.min(5)));
                self.set_status(SessionStatus::Failed(error));
                let commands = self.commands.clone();
                self.runtime.spawn(async move {
                    tokio::time::sleep(delay).await;
                    let _ = commands.send(Message::Reconnect);
                });
                return;
            }
        };
        self.connect_attempt = 0;
        self.discovery = Some(DiscoveryState::new(
            self.identity.pubkey_hex(),
            info.authority.clone(),
        ));
        self.relay = Some(info);
        self.live = Some(Live::connect(
            &self.runtime,
            self.origin.clone(),
            self.identity.clone(),
            self.verifier.clone(),
            LiveOptions::default(),
        ));
        self.set_status(SessionStatus::Ready);
        self.start_scan(ScanOptions::default());
    }

    fn set_status(&mut self, status: SessionStatus) {
        self.snapshot.send_if_modified(|current| {
            if current.status == status {
                return false;
            }
            *current = Arc::new(SessionSnapshot {
                status,
                ..(**current).clone()
            });
            true
        });
    }

    fn relay_author(&self) -> Option<PublicKey> {
        self.relay
            .as_ref()
            .and_then(|relay| PublicKey::from_hex(&relay.authority).ok())
    }

    // ----- roster -----

    fn start_scan(&mut self, options: ScanOptions) {
        let Some(discovery) = &self.discovery else {
            return;
        };
        if self.scan.is_some() {
            self.rescan = Some(options);
            return;
        }
        self.scan = Some(RosterScan::new(discovery, options));
        self.scan_next();
    }

    fn scan_next(&mut self) {
        let Some(scan) = &self.scan else {
            return;
        };
        let Some(read) = scan.next_read() else {
            self.scan_done();
            return;
        };
        let reader = self.reader.clone();
        let commands = self.commands.clone();
        let epoch = self.epoch;
        self.runtime.spawn(async move {
            let result = reader
                .read(
                    vec![read.filter],
                    ReadOptions {
                        fresh: read.fresh,
                        ..Default::default()
                    },
                )
                .await;
            let _ = commands.send(Message::ScanRead { epoch, result });
        });
    }

    fn scan_read(&mut self, result: Result<Events, ReadError>) {
        let (Some(scan), Some(discovery)) = (&mut self.scan, &mut self.discovery) else {
            return;
        };
        let mut denied_all = false;
        match result {
            Ok(events) => {
                if let Err(error) = scan.accept(discovery, &events) {
                    self.roster = RosterStatus::Error(error.to_string());
                }
            }
            Err(error) => {
                denied_all = scan.fail(discovery, error.kind == ReadErrorKind::Denied);
                self.roster = RosterStatus::Error(error.message);
            }
        }
        let done = scan.stage() == ScanStage::Done;
        if denied_all {
            self.purge_denied();
        }
        if done {
            self.scan_done();
        } else {
            self.scan_next();
        }
    }

    fn scan_done(&mut self) {
        let scan = self.scan.take();
        if scan.is_some_and(|scan| scan.certified())
            || !matches!(self.roster, RosterStatus::Error(_))
        {
            self.roster = RosterStatus::Ready;
        }
        self.purge_denied();
        self.update_interests();
        self.ensure_profiles(self.channel_people());
        self.publish();
        if let Some(options) = self.rescan.take() {
            self.start_scan(options);
        }
    }

    /// Drops timelines of channels the viewer can no longer read.
    fn purge_denied(&mut self) {
        let Some(discovery) = &self.discovery else {
            return;
        };
        let denied: Vec<String> = self
            .timelines
            .keys()
            .filter(|id| !discovery.can_access(id))
            .cloned()
            .collect();
        if denied.is_empty() {
            return;
        }
        self.epoch += 1;
        for id in denied {
            if let Some(mut timeline) = self.timelines.remove(&id) {
                timeline.fail("Conversation is unavailable".into());
                timeline.publish();
            }
            self.closed.retain(|closed| closed != &id);
        }
    }

    fn channel_people(&self) -> Vec<String> {
        let Some(discovery) = &self.discovery else {
            return Vec::new();
        };
        discovery
            .channels()
            .into_iter()
            .filter_map(|channel| channel.participants)
            .flatten()
            .collect()
    }

    /// Live interests: every joined channel plus open ones, with the open
    /// ones (selected first) set up before the rest.
    fn update_interests(&mut self) {
        let (Some(live), Some(discovery)) = (&mut self.live, &self.discovery) else {
            return;
        };
        let joined: Vec<String> = discovery.channels().into_iter().map(|c| c.id).collect();
        let mut prioritized: Vec<String> = self.selected.iter().cloned().collect();
        prioritized.extend(
            self.timelines
                .values()
                .filter(|t| t.viewers > 0 && Some(&t.id) != self.selected.as_ref())
                .map(|t| t.id.clone()),
        );
        let interests: Vec<String> = joined
            .iter()
            .cloned()
            .chain(prioritized.iter().cloned())
            .filter(|id| discovery.can_access(id))
            .collect();
        live.update(interests, joined, prioritized);
    }

    // ----- live -----

    fn live_update(&mut self, update: LiveUpdate) {
        match update {
            LiveUpdate::Events { events, .. } => self.accept(&events),
            LiveUpdate::Presence(_) | LiveUpdate::Status => {}
            LiveUpdate::Established { route, channels } => {
                if matches!(route, crate::RouteKey::Profiles) {
                    // Profiles changed while we were away are re-read.
                    for (ids, filter) in self.profiles.refresh() {
                        self.read_profiles(ids, filter);
                    }
                }
                for channel in channels {
                    if self.timelines.get(&channel).is_some_and(|t| t.stale) {
                        self.load_head(&channel, true);
                    }
                }
            }
            LiveUpdate::Denied { channels, .. } => {
                if let Some(discovery) = &mut self.discovery {
                    for id in &channels {
                        discovery.deny(id);
                    }
                }
                self.purge_denied();
                self.update_interests();
                self.publish();
            }
            LiveUpdate::AccessHint { .. } => self.start_scan(ScanOptions {
                strong: true,
                ..Default::default()
            }),
            LiveUpdate::Connected => {
                self.set_live(true);
                self.start_scan(ScanOptions::default());
            }
            LiveUpdate::Disconnected => {
                self.set_live(false);
                for timeline in self.timelines.values_mut() {
                    timeline.mark_stale();
                    timeline.publish();
                }
            }
        }
    }

    fn set_live(&mut self, live: bool) {
        self.snapshot.send_if_modified(|current| {
            if current.live == live {
                return false;
            }
            *current = Arc::new(SessionSnapshot {
                live,
                ..(**current).clone()
            });
            true
        });
    }

    /// Every verified event goes through here, in the reference order:
    /// discovery, membership hints, profiles, then channel timelines.
    fn accept(&mut self, events: &[Verified]) {
        let mut publish = false;
        if let Some(discovery) = &mut self.discovery {
            let mut changed = false;
            for event in events {
                if matches!(
                    event.kind_u16(),
                    kinds::CHANNEL_METADATA | kinds::CHANNEL_MEMBERS
                ) {
                    changed |= discovery.accept(event, false);
                }
            }
            if changed {
                publish = true;
                self.purge_denied();
                self.update_interests();
            }
        }
        let viewer = self.identity.pubkey_hex();
        let hint = events.iter().any(|event| {
            matches!(
                event.kind_u16(),
                kinds::MEMBER_ADDED | kinds::MEMBER_REMOVED
            ) && self
                .relay
                .as_ref()
                .is_some_and(|relay| membership_hint(event, &relay.authority, &viewer).is_some())
        });
        if hint {
            self.start_scan(ScanOptions {
                strong: true,
                ..Default::default()
            });
        }
        publish |= self.profiles.accept(events);
        let mut people = Vec::new();
        for timeline in self.timelines.values_mut() {
            if timeline.accept(events) {
                timeline.publish();
                people.extend(timeline.people());
            }
        }
        self.ensure_profiles(people);
        if publish {
            self.publish();
        }
    }

    // ----- timelines -----

    fn open(&mut self, channel: &str, view: watch::Sender<Arc<TimelineSnapshot>>) {
        let author = self
            .relay_author()
            .unwrap_or(PublicKey::from_slice(&[1; 32]).expect("valid key"));
        self.closed.retain(|id| id != channel);
        let timeline = self
            .timelines
            .entry(channel.to_string())
            .or_insert_with(|| Timeline::new(channel.to_string(), author));
        timeline.viewers += 1;
        timeline.attach(view);
        if timeline.needs_head() {
            self.load_head(channel, false);
        }
        self.update_interests();
    }

    fn close(&mut self, channel: &str) {
        let Some(timeline) = self.timelines.get_mut(channel) else {
            return;
        };
        timeline.viewers = timeline.viewers.saturating_sub(1);
        if timeline.viewers > 0 {
            return;
        }
        self.closed.push_back(channel.to_string());
        while self.closed.len() > RETAINED_CLOSED {
            if let Some(evicted) = self.closed.pop_front() {
                self.timelines.remove(&evicted);
            }
        }
        self.update_interests();
    }

    fn load_head(&mut self, channel: &str, catch_up: bool) {
        if self.relay.is_none() {
            return;
        }
        let Some(timeline) = self.timelines.get_mut(channel) else {
            return;
        };
        if timeline.loading_head {
            return;
        }
        timeline.loading_head = true;
        let generation = timeline.generation;
        self.read_page(channel.to_string(), generation, None, catch_up);
    }

    fn load_older(&mut self, channel: &str) {
        let Some(timeline) = self.timelines.get_mut(channel) else {
            return;
        };
        if !timeline.has_more || timeline.loading_older || timeline.history_limited {
            return;
        }
        let Some(cursor) = timeline.cursor.clone() else {
            return;
        };
        timeline.loading_older = true;
        timeline.publish();
        let generation = timeline.generation;
        self.read_page(channel.to_string(), generation, Some(cursor), false);
    }

    fn read_page(&self, channel: String, generation: u64, sent: Option<Cursor>, catch_up: bool) {
        let reader = self.reader.clone();
        let commands = self.commands.clone();
        let filter = page_filter(&channel, sent.as_ref());
        let background = catch_up && self.selected.as_deref() != Some(channel.as_str());
        self.runtime.spawn(async move {
            let options = if background {
                ReadOptions::background()
            } else {
                ReadOptions::default()
            };
            let result = reader.read(vec![filter], options).await;
            let _ = commands.send(Message::Page {
                channel,
                generation,
                sent,
                catch_up,
                result,
            });
        });
    }

    fn page(
        &mut self,
        channel: &str,
        generation: u64,
        sent: Option<Cursor>,
        catch_up: bool,
        result: Result<Events, ReadError>,
    ) {
        let Some(authority) = self.relay.as_ref().map(|relay| relay.authority.clone()) else {
            return;
        };
        let Some(timeline) = self.timelines.get_mut(channel) else {
            return;
        };
        if timeline.generation != generation {
            return;
        }
        let head = sent.is_none();
        if head {
            timeline.loading_head = false;
        } else {
            timeline.loading_older = false;
        }
        match result
            .map_err(|error| (error.kind, error.message))
            .and_then(|events| {
                parse_page(channel, &authority, sent.as_ref(), &events)
                    .map_err(|error| (ReadErrorKind::InvalidResponse, error.to_string()))
            }) {
            Ok(page) => {
                if catch_up {
                    timeline.merge_head(page.events, page.cursor, page.has_more);
                } else {
                    timeline.apply_page(page.events, page.cursor, page.has_more, head);
                }
            }
            Err((ReadErrorKind::Denied, _)) => {
                if let Some(discovery) = &mut self.discovery {
                    discovery.deny(channel);
                }
                self.purge_denied();
                self.publish();
                return;
            }
            Err((ReadErrorKind::Cancelled, _)) => {}
            Err((_, message)) => timeline.fail(message),
        }
        timeline.publish();
        let people = timeline.people();
        self.ensure_profiles(people);
    }

    // ----- profiles -----

    fn ensure_profiles(&mut self, ids: Vec<String>) {
        if self.relay.is_none() || ids.is_empty() {
            return;
        }
        let reads = self.profiles.ensure(ids.iter().map(String::as_str));
        for (ids, filter) in reads {
            self.read_profiles(ids, filter);
        }
    }

    fn read_profiles(&self, ids: Vec<String>, filter: Filter) {
        let reader = self.reader.clone();
        let commands = self.commands.clone();
        self.runtime.spawn(async move {
            let result = reader.read(vec![filter], ReadOptions::background()).await;
            let _ = commands.send(Message::Profiles { ids, result });
        });
    }

    // ----- output -----

    fn publish(&mut self) {
        let channels: Arc<[ChannelSummary]> = self
            .discovery
            .as_ref()
            .map(|discovery| Arc::<[ChannelSummary]>::from(discovery.channels()))
            .unwrap_or_else(|| Arc::new([]));
        let next = SessionSnapshot {
            status: self.snapshot.borrow().status.clone(),
            live: self.live.as_ref().is_some_and(Live::is_connected),
            relay: self.relay.clone(),
            roster: self.roster.clone(),
            channels,
            profiles: self.profiles.snapshot(),
        };
        self.snapshot.send_if_modified(|current| {
            let same = current.status == next.status
                && current.live == next.live
                && current.relay == next.relay
                && current.roster == next.roster
                && Arc::ptr_eq(&current.profiles, &next.profiles)
                && *current.channels == *next.channels;
            if !same {
                *current = Arc::new(next);
            }
            !same
        });
    }
}

/// NIP-11 fields the client uses (spec §1.4). `self` is required: without
/// it relay-signed events cannot be trusted.
fn relay_info(info: &serde_json::Value) -> Result<RelayInfo, String> {
    let authority = info
        .get("self")
        .and_then(serde_json::Value::as_str)
        .filter(|key| {
            key.len() == 64 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or("Relay did not advertise its identity")?;
    let text = |key: &str| {
        info.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Ok(RelayInfo {
        name: text("name"),
        icon: text("icon")
            .filter(|icon| icon.starts_with("https://") || icon.starts_with("data:image/")),
        authority: authority.to_string(),
        supports_channel_creation: info
            .get("supported_nips")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|nips| nips.iter().any(|nip| nip.as_u64() == Some(29))),
    })
}
