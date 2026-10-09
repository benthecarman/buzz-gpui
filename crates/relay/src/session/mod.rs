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
//! session (status, channels, profiles, unread counts, presence, typing)
//! and one per open channel. Read state and unread live in [`unread`],
//! presence and typing in [`presence`], notification candidates in
//! [`incoming`].

mod actions;
mod incoming;
mod presence;
mod threads;
mod timeline;
mod unread;

pub use actions::{MessageSearch, PeoplePage, SearchOutcome};
pub use incoming::IncomingMessage;
pub use threads::ThreadView;
pub use timeline::{TimelineSnapshot, TimelineStatus};
pub use unread::{ChannelUnread, READING_DWELL_MS, UnreadCounts};

use crate::discovery::{DiscoveryState, RosterScan, ScanOptions, ScanStage, membership_hint};
use crate::event::{Verified, Verifier, kinds};
use crate::fold::{LocalDelivery, MessageClock};
use crate::http::RelayHttp;
use crate::live::{Live, LiveOptions, LiveUpdate};
use crate::media::Media;
use crate::messages::{self, Author, Compose, EmojiCatalog, Mentions};
use crate::origin::RelayOrigin;
use crate::outbox::{HttpPublisher, Outbox, OutboxOptions, SendOptions};
use crate::presence::{
    PresenceDirectory, PresenceMode, PresencePublisher, PresenceState, Publication, PublishOutcome,
    SnapshotRead, SnapshotResult,
};
use crate::profiles::{ProfileDirectory, Profiles};
use crate::read_state::{PublishFailure, RefreshRequest, SnapshotDescriptor};
use crate::reader::{ReadError, ReadErrorKind, ReadOptions, Reader, ReaderOptions};
use crate::socket::Filter;
use crate::store::{Partition, SqliteJournal, Store};
use crate::typing::Typing;
use crate::window::{Cursor, page_filter, parse_page};
use crate::{ChannelSummary, Events};
use buzz_identity::Identity;
use nostr::prelude::{EventId, PublicKey};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use timeline::Timeline;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use unread::{Mark, Reads};

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
    /// NIP-11 `read_state_snapshot`: read state can be fetched completely.
    pub read_state_snapshot: Option<SnapshotDescriptor>,
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
    /// Unread rows and badges per channel. Replaced only when it changes.
    pub unread: Arc<UnreadCounts>,
    /// Observed statuses of the people passed to
    /// [`Session::watch_presence`]. Absent people are unknown: show no
    /// badge. Replaced only when it changes.
    pub presence: Arc<HashMap<String, PresenceState>>,
    /// Channel id → people typing at its top level, in the order they
    /// started. Replaced only when it changes.
    pub typing: Arc<HashMap<String, Vec<String>>>,
    /// Thread root id → people typing in that thread.
    pub thread_typing: Arc<HashMap<String, Vec<String>>>,
    /// DMs the viewer hid (relay-signed 30622). The sidebar leaves them
    /// out; opening one still works.
    pub hidden_dms: Arc<[String]>,
}

pub struct Session {
    commands: mpsc::UnboundedSender<Message>,
    runtime: tokio::runtime::Handle,
    snapshot: watch::Receiver<Arc<SessionSnapshot>>,
    incoming: broadcast::Sender<IncomingMessage>,
    viewer: String,
    origin: RelayOrigin,
    next_thread: std::sync::atomic::AtomicU64,
    media: Media,
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

    /// Reports what the view shows while its channel is selected: the
    /// message rows fully in view (at most 128, newest last) and whether
    /// the list sits at its newest message. Rows that stay in view for
    /// [`READING_DWELL_MS`] are read, and at the bottom the channel is
    /// caught up. Report nothing in view and `at_bottom: false` while the
    /// window is hidden or unfocused. Until the first report the view is
    /// taken to be at the bottom.
    pub fn viewport(&self, visible: Vec<EventId>, at_bottom: bool) {
        let _ = self.commands.send(Message::Viewport {
            channel: self.channel.clone(),
            visible,
            at_bottom,
        });
    }
}

impl Drop for ChannelView {
    fn drop(&mut self) {
        let _ = self.commands.send(Message::Close(self.channel.clone()));
    }
}

impl Session {
    /// `store` holds the outbox journal and other local state.
    pub fn open(
        runtime: &tokio::runtime::Handle,
        origin: RelayOrigin,
        identity: Arc<Identity>,
        store: Store,
    ) -> Self {
        let (commands, inbox) = mpsc::unbounded_channel();
        let snapshot = watch::Sender::new(Arc::new(SessionSnapshot {
            status: SessionStatus::Connecting,
            live: false,
            relay: None,
            roster: RosterStatus::Loading,
            channels: Arc::new([]),
            profiles: Profiles::default(),
            unread: Arc::default(),
            presence: Arc::default(),
            typing: Arc::default(),
            thread_typing: Arc::default(),
            hidden_dms: Arc::new([]),
        }));
        let (incoming, _) = broadcast::channel(64);
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
        let media = Media::new(runtime.clone(), origin.clone(), identity.clone());
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
            store,
            clock: Arc::new(MessageClock::new()),
            outbox: None,
            outbox_changes: None,
            reads: None,
            threads: HashMap::new(),
            presence: PresenceDirectory::new(viewer.clone()),
            publisher: PresencePublisher::default(),
            typing: Typing::new(viewer.clone()),
            incoming: incoming::Incoming::default(),
            incoming_sender: incoming.clone(),
            hidden_dms: Arc::new([]),
        };
        let task = runtime.spawn(actor.run(inbox));
        Self {
            commands,
            runtime: runtime.clone(),
            snapshot: receiver,
            incoming,
            viewer,
            origin,
            next_thread: std::sync::atomic::AtomicU64::new(1),
            media,
            task,
        }
    }

    pub fn snapshot(&self) -> watch::Receiver<Arc<SessionSnapshot>> {
        self.snapshot.clone()
    }

    pub fn viewer(&self) -> &str {
        &self.viewer
    }

    /// Downloads for this community's media and other image URLs.
    pub fn media(&self) -> &Media {
        &self.media
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
    /// Opens a thread panel on `message`: the root, or with `exact` a reply
    /// to reveal in its thread.
    pub fn open_thread(&self, channel: &str, message: EventId, exact: bool) -> ThreadView {
        let id = self
            .next_thread
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (view, snapshot) =
            watch::channel(Arc::new(crate::threads::ThreadSnapshot::idle(exact)));
        let _ = self.commands.send(Message::OpenThread {
            id,
            channel: channel.to_string(),
            message,
            exact,
            view,
        });
        ThreadView {
            snapshot,
            id,
            commands: self.commands.clone(),
        }
    }

    pub fn select(&self, channel: Option<String>) {
        let _ = self.commands.send(Message::Select(channel));
    }

    pub fn retry(&self) {
        let _ = self.commands.send(Message::Retry);
    }

    /// Follows these people's presence (at most 256; the viewer first,
    /// then in this order). Replaces the previous set; people no longer
    /// listed lose their status. Results arrive in
    /// [`SessionSnapshot::presence`].
    pub fn watch_presence(&self, authors: Vec<String>) {
        let _ = self.commands.send(Message::WatchPresence(authors));
    }

    /// Whether the machine has been idle long enough to show away. Only
    /// matters in [`PresenceMode::Auto`].
    pub fn set_idle(&self, idle: bool) {
        let _ = self.commands.send(Message::SetIdle(idle));
    }

    /// The user's own status choice.
    pub fn set_presence_mode(&self, mode: PresenceMode) {
        let _ = self.commands.send(Message::SetPresenceMode(mode));
    }

    /// New messages from others that should notify: mentions, direct
    /// messages and replies in the viewer's conversations, still unread and
    /// not in view, each sent once. Lagging receivers lose old items.
    pub fn incoming(&self) -> broadcast::Receiver<IncomingMessage> {
        self.incoming.subscribe()
    }

    async fn write(&self, write: Write) -> Result<(), String> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Message::Write { write, reply })
            .map_err(|_| "The session has closed".to_string())?;
        answer
            .await
            .unwrap_or_else(|_| Err("The session has closed".to_string()))
    }

    /// Sends a message, or a thread reply when `thread` names
    /// `(root, parent)`. Resolves once the intent is safely queued; the
    /// timeline shows its delivery.
    pub async fn send(
        &self,
        channel: &str,
        content: &str,
        mentions: Mentions,
        thread: Option<(EventId, EventId)>,
    ) -> Result<(), String> {
        self.write(Write::Send {
            channel: channel.to_string(),
            content: content.to_string(),
            mentions,
            thread,
        })
        .await
    }

    pub async fn edit(&self, channel: &str, id: EventId, content: &str) -> Result<(), String> {
        self.write(Write::Edit {
            channel: channel.to_string(),
            id,
            content: content.to_string(),
        })
        .await
    }

    pub async fn react(&self, channel: &str, id: EventId, emoji: &str) -> Result<(), String> {
        self.write(Write::React {
            channel: channel.to_string(),
            id,
            content: emoji.to_string(),
        })
        .await
    }

    /// Deletes our own message or reaction.
    pub async fn remove(&self, channel: &str, id: EventId) -> Result<(), String> {
        self.write(Write::Remove {
            channel: channel.to_string(),
            id,
        })
        .await
    }

    /// Republishes the exact signed bytes of a failed or unknown send.
    pub async fn retry_send(&self, id: EventId) -> Result<(), String> {
        self.write(Write::Retry(id)).await
    }

    /// Discards a failed send.
    pub async fn dismiss(&self, id: EventId) -> Result<(), String> {
        self.write(Write::Dismiss(id)).await
    }

    async fn mark(&self, mark: Mark) -> Result<(), String> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Message::Mark { mark, reply })
            .map_err(|_| "The session has closed".to_string())?;
        answer
            .await
            .unwrap_or_else(|_| Err("The session has closed".to_string()))
    }

    /// Reads the whole channel, through the newest message seen or now,
    /// whichever is later, and clears its manual unread marks. Saved at
    /// once; synced to the viewer's other devices shortly after.
    pub async fn mark_channel_read(&self, channel: &str) -> Result<(), String> {
        self.mark(Mark::Channel(channel.to_string())).await
    }

    /// Marks every channel with anything unread as read, with one cut.
    pub async fn mark_all_read(&self) -> Result<(), String> {
        self.mark(Mark::All).await
    }

    /// Reads one loaded message and its loaded replies.
    pub async fn mark_message_read(&self, channel: &str, id: EventId) -> Result<(), String> {
        self.mark(Mark::MessageRead(channel.to_string(), id)).await
    }

    /// Shows a loaded message (and its loaded replies) as unread for the
    /// rest of this channel visit, and the channel as unread on this
    /// device until it is next opened. Never synced.
    pub async fn mark_message_unread(&self, channel: &str, id: EventId) -> Result<(), String> {
        self.mark(Mark::MessageUnread(channel.to_string(), id))
            .await
    }
}

enum Write {
    Send {
        channel: String,
        content: String,
        mentions: Mentions,
        thread: Option<(EventId, EventId)>,
    },
    Edit {
        channel: String,
        id: EventId,
        content: String,
    },
    React {
        channel: String,
        id: EventId,
        content: String,
    },
    Remove {
        channel: String,
        id: EventId,
    },
    Retry(EventId),
    Dismiss(EventId),
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum Message {
    OpenThread {
        id: u64,
        channel: String,
        message: EventId,
        exact: bool,
        view: watch::Sender<Arc<crate::threads::ThreadSnapshot>>,
    },
    CloseThread(u64),
    ThreadMore(u64),
    ThreadRefresh(u64),
    ThreadRead {
        id: u64,
        token: u64,
        channel: String,
        result: Result<Events, ReadError>,
    },
    Open {
        channel: String,
        view: watch::Sender<Arc<TimelineSnapshot>>,
    },
    Close(String),
    LoadOlder(String),
    Select(Option<String>),
    Retry,
    Write {
        write: Write,
        reply: oneshot::Sender<Result<(), String>>,
    },
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
    Mark {
        mark: Mark,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Viewport {
        channel: String,
        visible: Vec<EventId>,
        at_bottom: bool,
    },
    ReadRefresh {
        request: RefreshRequest,
        result: Result<Vec<Verified>, String>,
    },
    ReadPublished(Result<(), PublishFailure>),
    ReadBack(Result<Vec<Verified>, String>),
    Repair(Result<Events, ReadError>),
    Homes {
        request: crate::unread::HomesRequest,
        result: Result<Events, ReadError>,
    },
    Lookup(Result<Events, ReadError>),
    WatchPresence(Vec<String>),
    SetIdle(bool),
    SetPresenceMode(PresenceMode),
    PresenceRead {
        read: SnapshotRead,
        result: SnapshotResult,
    },
    PresencePublished {
        publication: Publication,
        outcome: PublishOutcome,
    },
    /// Runs on the actor between messages: how session operations read and
    /// change its state.
    With(Box<dyn FnOnce(&mut Actor) + Send>),
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
    store: Store,
    /// Shared by the outbox and every timeline, so sends sort after what
    /// is shown.
    clock: Arc<MessageClock>,
    outbox: Option<Outbox>,
    outbox_changes: Option<watch::Receiver<u64>>,
    /// Read state and unread, from NIP-11 on.
    reads: Option<Reads>,
    threads: HashMap<u64, threads::OpenThread>,
    presence: PresenceDirectory,
    publisher: PresencePublisher,
    typing: Typing,
    incoming: incoming::Incoming,
    incoming_sender: broadcast::Sender<IncomingMessage>,
    hidden_dms: Arc<[String]>,
}

impl Actor {
    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Message>) {
        self.connect();
        loop {
            let deadline = [self.reads_deadline(), self.presence_deadline()]
                .into_iter()
                .flatten()
                .min();
            let outbox_changed = async {
                match &mut self.outbox_changes {
                    Some(changes) => changes.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            };
            let live_update = async {
                match &mut self.live {
                    Some(live) => Some(live.next().await),
                    None => std::future::pending().await,
                }
            };
            let timer = async move {
                match deadline {
                    Some(at) => {
                        let wait = at.saturating_sub(unread::now_ms());
                        tokio::time::sleep(Duration::from_millis(wait)).await;
                    }
                    None => std::future::pending().await,
                }
            };
            let message = tokio::select! {
                message = inbox.recv() => message,
                () = timer => {
                    self.reads_tick();
                    self.presence_tick();
                    continue;
                }
                Some(update) = live_update => {
                    self.live_update(update);
                    continue;
                }
                changed = outbox_changed => {
                    if changed {
                        self.local_changed();
                    } else {
                        self.outbox_changes = None;
                    }
                    continue;
                }
            };
            let Some(message) = message else {
                return;
            };
            self.handle(message);
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::OpenThread {
                id,
                channel,
                message,
                exact,
                view,
            } => self.open_thread(id, channel, message, exact, view),
            Message::CloseThread(id) => {
                self.threads.remove(&id);
            }
            Message::ThreadMore(id) => self.thread_more(id, false),
            Message::ThreadRefresh(id) => self.thread_more(id, true),
            Message::ThreadRead {
                id,
                token,
                channel,
                result,
            } => self.thread_result(id, token, &channel, result),
            Message::Open { channel, view } => self.open(&channel, view),
            Message::Close(channel) => self.close(&channel),
            Message::LoadOlder(channel) => self.load_older(&channel),
            Message::Select(channel) => {
                self.selected = channel;
                self.update_interests();
                self.update_reading();
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
                    self.retry_reads();
                }
            }
            Message::Reconnect => {
                if self.relay.is_none() {
                    self.connect();
                }
            }
            Message::Connected(result) => self.connected(result),
            Message::Write {
                write: Write::Dismiss(id),
                reply,
            } => match &self.outbox {
                Some(outbox) => {
                    let dismissed = outbox.dismiss(&id);
                    self.runtime.spawn(async move {
                        let _ = reply.send(dismissed.await.map_err(|error| error.to_string()));
                    });
                }
                None => {
                    let _ = reply.send(Err("Not connected to the relay yet".into()));
                }
            },
            Message::Write { write, reply } => {
                let _ = reply.send(self.write(write));
            }
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
            } => {
                if let Ok(events) = &result
                    && self.reads_accept(events)
                {
                    self.publish();
                }
                self.page(&channel, generation, sent, catch_up, result);
                self.reading_loaded(&channel);
            }
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
            Message::Mark { mark, reply } => {
                let _ = reply.send(self.mark(mark));
            }
            Message::Viewport {
                channel,
                visible,
                at_bottom,
            } => self.viewport(&channel, visible, at_bottom),
            Message::ReadRefresh { request, result } => self.read_refreshed(request, result),
            Message::ReadPublished(outcome) => self.read_published(outcome),
            Message::ReadBack(result) => self.read_back(result),
            Message::Repair(result) => self.repaired(result),
            Message::Homes { request, result } => self.homes(request, result),
            Message::Lookup(result) => self.looked_up(result),
            Message::WatchPresence(authors) => self.watch_presence(authors),
            Message::SetIdle(idle) => {
                self.publisher.set_idle(idle, unread::now_ms());
            }
            Message::SetPresenceMode(mode) => {
                self.publisher.set_mode(mode, unread::now_ms());
            }
            Message::PresenceRead { read, result } => self.presence_read(read, result),
            Message::PresencePublished {
                publication,
                outcome,
            } => self.presence_published(publication, outcome),
            Message::With(run) => run(self),
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
        let journal = SqliteJournal::new(
            self.store.clone(),
            Partition::new(&self.origin, &self.identity.pubkey_hex()),
        );
        let outbox = Outbox::new(
            self.identity.clone(),
            Arc::new(HttpPublisher::new(self.http.clone(), self.reader.clone())),
            Arc::new(journal),
            OutboxOptions {
                message_clock: self.clock.clone(),
                ..Default::default()
            },
        );
        self.outbox_changes = Some(outbox.changes());
        self.outbox = Some(outbox);
        self.open_reads();
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
        self.reads_channels(true);
        self.ensure_profiles(self.channel_people());
        self.refresh_visibility();
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
        self.threads_purge();
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
            LiveUpdate::Events {
                events,
                phase,
                scope,
            } => {
                // Typing pulses never enter history, unread or caches.
                let (pulses, events): (Vec<Verified>, Vec<Verified>) = events
                    .into_iter()
                    .partition(|event| event.kind_u16() == kinds::TYPING);
                let candidates = if phase == crate::live::Phase::Live {
                    self.incoming_candidates(&events, &scope)
                } else {
                    Vec::new()
                };
                if !events.is_empty() {
                    self.accept(&events);
                }
                self.typing_accept(&[events, pulses].concat());
                self.admit_incoming(candidates);
            }
            LiveUpdate::Presence(events) => self.presence_live(&events),
            LiveUpdate::Status => {}
            LiveUpdate::Established { route, channels } => {
                if matches!(route, crate::RouteKey::Profiles) {
                    // Profiles changed while we were away are re-read.
                    for (ids, filter) in self.profiles.refresh() {
                        self.read_profiles(ids, filter);
                    }
                }
                self.threads_established(&channels);
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
                self.reads_channels(false);
                self.publish();
            }
            LiveUpdate::AccessHint { .. } => self.start_scan(ScanOptions {
                strong: true,
                ..Default::default()
            }),
            LiveUpdate::Connected => {
                self.set_live(true);
                self.presence_connected(true);
                self.start_scan(ScanOptions::default());
                self.reads_reconnected();
            }
            LiveUpdate::Disconnected => {
                self.set_live(false);
                self.presence_connected(false);
                for timeline in self.timelines.values_mut() {
                    timeline.mark_stale();
                    timeline.publish();
                }
                self.reads_disconnected();
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
    /// discovery, membership hints, read state, unread, outbox echo,
    /// profiles, then channel timelines.
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
                self.reads_channels(false);
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
        publish |= self.reads_accept(events);
        if let Some(outbox) = &self.outbox {
            outbox.observe(events);
        }
        publish |= self.profiles.accept(events);
        let mut people = Vec::new();
        for timeline in self.timelines.values_mut() {
            if timeline.accept(events) {
                timeline.publish();
                people.extend(timeline.people());
            }
        }
        self.threads_receive(events);
        self.ensure_profiles(people);
        self.reading_arrivals(events);
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
            .or_insert_with(|| Timeline::new(channel.to_string(), author, self.clock.clone()));
        timeline.viewers += 1;
        timeline.attach(view);
        if timeline.needs_head() {
            self.load_head(channel, false);
        }
        self.update_interests();
        self.update_reading();
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
        self.update_reading();
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
                self.reads_channels(false);
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

    // ----- writes -----

    fn write(&mut self, write: Write) -> Result<(), String> {
        let outbox = self
            .outbox
            .clone()
            .ok_or("Not connected to the relay yet")?;
        let viewer = self.identity.pubkey_hex();
        let authority = self.relay.as_ref().map(|relay| relay.authority.clone());
        let author = Author {
            viewer: &viewer,
            signing_authority: authority.as_deref(),
        };
        let find = |channel: &str, id: &EventId| {
            self.timelines
                .get(channel)
                .and_then(|timeline| timeline.find(id))
                .ok_or_else(|| "Load the message first".to_string())
        };
        let drafts = match write {
            Write::Send {
                channel,
                content,
                mentions,
                thread,
            } => {
                if !self
                    .discovery
                    .as_ref()
                    .is_some_and(|d| d.can_participate(&channel))
                {
                    return Err("You can't post in this channel".into());
                }
                if let Some(discovery) = &self.discovery {
                    crate::members::validate_mentions(
                        discovery.get(&channel).as_ref(),
                        discovery.can_participate(&channel),
                        &mentions.recipients,
                    )?;
                }
                let compose = Compose {
                    channel_id: &channel,
                    content: &content,
                    mentions: &mentions,
                    attachments: &[],
                    emoji: EmojiCatalog::Ready(&[]),
                    origin: &self.origin,
                };
                let draft = match thread {
                    Some((root, parent)) => {
                        messages::reply(&compose, &root.to_hex(), &parent.to_hex())
                    }
                    None => messages::message(&compose),
                };
                vec![draft.map_err(|error| error.to_string())?]
            }
            Write::Edit {
                channel,
                id,
                content,
            } => {
                let original = find(&channel, &id)?;
                vec![
                    messages::edit(
                        &author,
                        &original,
                        &original,
                        &content,
                        EmojiCatalog::Ready(&[]),
                    )
                    .map_err(|error| error.to_string())?,
                ]
            }
            Write::React {
                channel,
                id,
                content,
            } => {
                let original = find(&channel, &id)?;
                vec![
                    messages::react(&original, &content, None, EmojiCatalog::Ready(&[]))
                        .map_err(|error| error.to_string())?,
                ]
            }
            Write::Remove { channel, id } => {
                let target = find(&channel, &id)?;
                let ids = vec![id.to_hex()];
                messages::remove(&author, &ids, |_| Some(target.event().as_ref()), None)
                    .map_err(|error| error.to_string())?
            }
            Write::Retry(id) => return outbox.retry(&id, None).map_err(|error| error.to_string()),
            Write::Dismiss(_) => unreachable!("dismissals finish asynchronously"),
        };
        for draft in drafts {
            outbox
                .send(draft, SendOptions::default())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Shows the outbox's signed operations in their timelines.
    fn local_changed(&mut self) {
        let local = self.local_operations();
        for timeline in self.timelines.values_mut() {
            timeline.set_local(&local);
            timeline.publish();
        }
        self.threads_local(&local);
    }

    /// The outbox's signed operations with their delivery state.
    fn local_operations(&self) -> Vec<(Verified, LocalDelivery)> {
        let Some(outbox) = &self.outbox else {
            return Vec::new();
        };
        outbox
            .visible()
            .iter()
            .filter_map(|op| {
                let signed = op.signed.clone()?;
                let event = self.verifier.verify(signed).ok()?;
                let status = match op.delivery {
                    crate::outbox::Delivery::Sending => crate::fold::Delivery::Sending,
                    crate::outbox::Delivery::Accepted => crate::fold::Delivery::Accepted,
                    crate::outbox::Delivery::Unknown => crate::fold::Delivery::Unknown,
                    crate::outbox::Delivery::Failed => crate::fold::Delivery::Failed,
                    crate::outbox::Delivery::Seen => crate::fold::Delivery::Seen,
                };
                Some((
                    event,
                    LocalDelivery {
                        status,
                        error: op.error.clone(),
                    },
                ))
            })
            .collect()
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
        let unread = self.unread_counts();
        let next = SessionSnapshot {
            status: self.snapshot.borrow().status.clone(),
            live: self.live.as_ref().is_some_and(Live::is_connected),
            relay: self.relay.clone(),
            roster: self.roster.clone(),
            channels,
            profiles: self.profiles.snapshot(),
            unread,
            presence: self.presence.snapshot(),
            typing: self.typing.snapshot().channels.clone(),
            thread_typing: self.typing.snapshot().threads.clone(),
            hidden_dms: self.hidden_dms.clone(),
        };
        self.snapshot.send_if_modified(|current| {
            let same = current.status == next.status
                && current.live == next.live
                && current.relay == next.relay
                && current.roster == next.roster
                && Arc::ptr_eq(&current.profiles, &next.profiles)
                && Arc::ptr_eq(&current.unread, &next.unread)
                && Arc::ptr_eq(&current.presence, &next.presence)
                && Arc::ptr_eq(&current.typing, &next.typing)
                && Arc::ptr_eq(&current.thread_typing, &next.thread_typing)
                && Arc::ptr_eq(&current.hidden_dms, &next.hidden_dms)
                && *current.channels == *next.channels;
            if !same {
                *current = Arc::new(next);
            }
            !same
        });
        self.after_publish();
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
        read_state_snapshot: SnapshotDescriptor::from_nip11(info),
    })
}
