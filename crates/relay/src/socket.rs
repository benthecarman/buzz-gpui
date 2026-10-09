//! One WebSocket to one relay, kept open for the life of a community session.
//!
//! The socket authenticates with NIP-42 when the relay asks, holds every
//! subscription until then, and matches `OK` replies to publishes. After a
//! reconnect it either re-sends open subscriptions or reports them lost,
//! depending on [`SocketOptions::resubscribe`]. It never retries a publish itself: an event whose `OK` was
//! lost is reported as such, and the outbox decides what to do with the
//! exact signed bytes.

use crate::event::{Verified, Verifier};
use crate::origin::RelayOrigin;
use buzz_identity::Identity;
use futures::{SinkExt, StreamExt};
use nostr::prelude::{Event, EventId};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

/// A NIP-01 filter. Buzz relays accept extra fields (`search`,
/// `thread_window`, ...), so filters are plain JSON objects.
pub type Filter = serde_json::Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketState {
    Connecting { attempt: u32 },
    Authenticating,
    Ready,
    Waiting { error: String, retry_in: Duration },
}

#[derive(Debug)]
pub enum SubscriptionEvent {
    Event(Verified),
    /// Stored events have all been sent; what follows is live.
    Eose,
    /// The relay ended the subscription. It is not re-sent.
    Closed(String),
    /// The socket reconnected and re-sent this subscription. Events stored
    /// while disconnected arrive again, so consumers must dedupe by ID.
    Resubscribed,
    /// The connection dropped and this subscription was discarded (only
    /// when [`SocketOptions::resubscribe`] is off).
    Lost,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    #[error("not connected to the relay")]
    NotConnected,
    /// The connection closed before the relay answered; the event may or may
    /// not have been stored.
    #[error("connection lost before the relay answered")]
    Unknown,
    #[error("the relay did not answer in time")]
    TimedOut,
    #[error("{0}")]
    Rejected(String),
}

#[derive(Clone, Debug)]
pub struct SocketOptions {
    pub connect_timeout: Duration,
    /// How long to wait after connecting for an AUTH challenge before
    /// treating the relay as open.
    pub auth_wait: Duration,
    pub publish_timeout: Duration,
    pub ping_interval: Duration,
    /// Reconnect when nothing at all arrives for this long.
    pub dead_after: Duration,
    pub min_backoff: Duration,
    pub max_backoff: Duration,
    /// Reconnect if authentication has not finished this long after
    /// connecting.
    pub auth_deadline: Duration,
    /// Re-send open subscriptions after a reconnect. When off, they are
    /// reported [`SubscriptionEvent::Lost`] so the owner can rebuild them.
    pub resubscribe: bool,
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            auth_wait: Duration::from_millis(1500),
            publish_timeout: Duration::from_secs(15),
            ping_interval: Duration::from_secs(25),
            dead_after: Duration::from_secs(70),
            min_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
            auth_deadline: Duration::from_secs(10),
            resubscribe: true,
        }
    }
}

enum Command {
    Subscribe {
        id: u64,
        filters: Vec<Filter>,
        events: mpsc::UnboundedSender<SubscriptionEvent>,
    },
    Unsubscribe(u64),
    Publish {
        event: Box<Event>,
        reply: oneshot::Sender<Result<(), PublishError>>,
    },
    Reconnect,
}

pub struct RelaySocket {
    commands: mpsc::UnboundedSender<Command>,
    state: watch::Receiver<SocketState>,
    next_id: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

/// Publishes on a [`RelaySocket`] without owning it. Once the socket is
/// dropped every publication fails with [`PublishError::NotConnected`].
#[derive(Clone)]
pub struct SocketPublisher {
    commands: mpsc::UnboundedSender<Command>,
}

impl SocketPublisher {
    /// Sends a signed event and waits for the relay's `OK`.
    pub async fn publish(&self, event: Event) -> Result<(), PublishError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Publish {
                event: Box::new(event),
                reply,
            })
            .map_err(|_| PublishError::NotConnected)?;
        answer.await.unwrap_or(Err(PublishError::Unknown))
    }
}

/// Live events for one REQ. Dropping it sends CLOSE.
pub struct Subscription {
    id: u64,
    events: mpsc::UnboundedReceiver<SubscriptionEvent>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Subscription {
    pub async fn next(&mut self) -> Option<SubscriptionEvent> {
        self.events.recv().await
    }
}

impl futures::Stream for Subscription {
    type Item = SubscriptionEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<SubscriptionEvent>> {
        self.events.poll_recv(cx)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Unsubscribe(self.id));
    }
}

impl RelaySocket {
    pub fn connect(
        runtime: &tokio::runtime::Handle,
        origin: RelayOrigin,
        identity: Arc<Identity>,
        verifier: Arc<Verifier>,
        options: SocketOptions,
    ) -> Self {
        let (commands, queue) = mpsc::unbounded_channel();
        let (state_sender, state) = watch::channel(SocketState::Connecting { attempt: 1 });
        let task = runtime.spawn(
            Actor {
                origin,
                identity,
                verifier,
                options,
                state: state_sender,
                subscriptions: HashMap::new(),
                queue,
                subscribed_once: false,
            }
            .run(),
        );
        Self {
            commands,
            state,
            next_id: Arc::new(AtomicU64::new(1)),
            task,
        }
    }

    pub fn state(&self) -> watch::Receiver<SocketState> {
        self.state.clone()
    }

    pub fn subscribe(&self, filters: Vec<Filter>) -> Subscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (events, receiver) = mpsc::unbounded_channel();
        let _ = self.commands.send(Command::Subscribe {
            id,
            filters,
            events,
        });
        Subscription {
            id,
            events: receiver,
            commands: self.commands.clone(),
        }
    }

    /// Sends a signed event and waits for the relay's `OK`.
    pub async fn publish(&self, event: Event) -> Result<(), PublishError> {
        self.publisher().publish(event).await
    }

    /// A handle that publishes on this socket from other tasks.
    pub fn publisher(&self) -> SocketPublisher {
        SocketPublisher {
            commands: self.commands.clone(),
        }
    }

    /// Skips the current backoff.
    pub fn reconnect(&self) {
        let _ = self.commands.send(Command::Reconnect);
    }
}

impl Drop for RelaySocket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Open {
    filters: Vec<Filter>,
    events: mpsc::UnboundedSender<SubscriptionEvent>,
}

struct Actor {
    origin: RelayOrigin,
    identity: Arc<Identity>,
    verifier: Arc<Verifier>,
    options: SocketOptions,
    state: watch::Sender<SocketState>,
    subscriptions: HashMap<u64, Open>,
    queue: mpsc::UnboundedReceiver<Command>,
    /// An earlier connection already sent the subscriptions.
    subscribed_once: bool,
}

type Stream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

enum End {
    /// The socket owner dropped the handle.
    Stopped,
    Lost(String),
}

impl Actor {
    async fn run(mut self) {
        let mut attempt = 1u32;
        loop {
            self.state.send_replace(SocketState::Connecting { attempt });
            match self.session(&mut attempt).await {
                End::Stopped => return,
                End::Lost(error) => {
                    let retry_in = backoff(&self.options, attempt);
                    tracing::debug!(origin = %self.origin, %error, ?retry_in, "relay socket lost");
                    self.state
                        .send_replace(SocketState::Waiting { error, retry_in });
                    if !self.wait(retry_in).await {
                        return;
                    }
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }

    /// Waits out a backoff while still accepting commands. Returns false if
    /// the handle was dropped.
    async fn wait(&mut self, duration: Duration) -> bool {
        let deadline = tokio::time::sleep(duration);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => return true,
                command = self.queue.recv() => match command {
                    None => return false,
                    Some(Command::Reconnect) => return true,
                    Some(command) => self.offline(command),
                },
            }
        }
    }

    /// Handles a command while there is no connection.
    fn offline(&mut self, command: Command) {
        match command {
            Command::Subscribe {
                id,
                filters,
                events,
            } => {
                self.subscriptions.insert(id, Open { filters, events });
            }
            Command::Unsubscribe(id) => {
                self.subscriptions.remove(&id);
            }
            Command::Publish { reply, .. } => {
                let _ = reply.send(Err(PublishError::NotConnected));
            }
            Command::Reconnect => {}
        }
    }

    async fn session(&mut self, attempt: &mut u32) -> End {
        let connected = tokio::time::timeout(
            self.options.connect_timeout,
            tokio_tungstenite::connect_async(self.origin.websocket()),
        )
        .await;
        let mut socket: Stream = match connected {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(error)) => return End::Lost(error.to_string()),
            Err(_) => return End::Lost("connection timed out".into()),
        };

        let mut session = Session::default();
        let dead = tokio::time::sleep(self.options.dead_after);
        tokio::pin!(dead);
        let mut ping = tokio::time::interval(self.options.ping_interval);
        ping.reset();
        // Before the relay asks for AUTH (or the wait runs out), hold every
        // REQ: a relay that requires auth would only refuse it.
        let auth_wait = tokio::time::sleep(self.options.auth_wait);
        tokio::pin!(auth_wait);
        let auth_deadline = tokio::time::sleep(self.options.auth_deadline);
        tokio::pin!(auth_deadline);
        self.state.send_replace(SocketState::Authenticating);

        loop {
            tokio::select! {
                incoming = socket.next() => {
                    let text = match incoming {
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Close(_))) | None => {
                            return self.lost(&mut session, "relay closed the connection");
                        }
                        Some(Ok(_)) => {
                            dead.as_mut().reset(Instant::now() + self.options.dead_after);
                            continue;
                        }
                        Some(Err(error)) => return self.lost(&mut session, &error.to_string()),
                    };
                    dead.as_mut().reset(Instant::now() + self.options.dead_after);
                    let outgoing = self.receive(&mut session, text.as_str());
                    if session.ready && !session.announced {
                        session.announced = true;
                        *attempt = 1;
                        self.state.send_replace(SocketState::Ready);
                    }
                    for message in outgoing {
                        if let Err(error) = socket.send(Message::Text(message.into())).await {
                            return self.lost(&mut session, &error.to_string());
                        }
                    }
                }
                _ = &mut auth_wait, if !session.ready && session.auth_event.is_none() => {
                    // No challenge: an open relay.
                    session.ready = true;
                    session.announced = true;
                    *attempt = 1;
                    self.state.send_replace(SocketState::Ready);
                    for message in self.resubscribe_all() {
                        if let Err(error) = socket.send(Message::Text(message.into())).await {
                            return self.lost(&mut session, &error.to_string());
                        }
                    }
                }
                command = self.queue.recv() => {
                    let Some(command) = command else {
                        let _ = socket.close(None).await;
                        return End::Stopped;
                    };
                    let outgoing = match command {
                        Command::Reconnect => {
                            return self.lost(&mut session, "reconnect requested");
                        }
                        command => self.command(&mut session, command),
                    };
                    for message in outgoing {
                        if let Err(error) = socket.send(Message::Text(message.into())).await {
                            return self.lost(&mut session, &error.to_string());
                        }
                    }
                }
                _ = ping.tick() => {
                    if let Err(error) = socket.send(Message::Ping(Vec::new().into())).await {
                        return self.lost(&mut session, &error.to_string());
                    }
                }
                _ = &mut dead => return self.lost(&mut session, "relay stopped responding"),
                _ = &mut auth_deadline, if session.auth_event.is_some() && !session.authenticated => {
                    return self.lost(&mut session, "authentication timed out");
                }
                _ = session.next_publish_deadline() => {
                    session.expire_publishes();
                }
            }
        }
    }

    /// Fails everything waiting on this connection and keeps subscriptions
    /// for the next one.
    fn lost(&mut self, session: &mut Session, error: &str) -> End {
        for (_, (reply, _)) in session.publishes.drain() {
            let _ = reply.send(Err(PublishError::Unknown));
        }
        if !self.options.resubscribe {
            for (_, open) in self.subscriptions.drain() {
                let _ = open.events.send(SubscriptionEvent::Lost);
            }
        }
        End::Lost(error.to_string())
    }

    fn command(&mut self, session: &mut Session, command: Command) -> Vec<String> {
        match command {
            Command::Subscribe {
                id,
                filters,
                events,
            } => {
                let message = session.ready.then(|| req(id, &filters));
                self.subscriptions.insert(id, Open { filters, events });
                message.into_iter().collect()
            }
            Command::Unsubscribe(id) => {
                let was_open = self.subscriptions.remove(&id).is_some();
                if was_open && session.ready {
                    vec![json!(["CLOSE", sub_id(id)]).to_string()]
                } else {
                    Vec::new()
                }
            }
            Command::Publish { event, reply } => {
                if !session.ready {
                    let _ = reply.send(Err(PublishError::NotConnected));
                    return Vec::new();
                }
                let message = json!(["EVENT", event]).to_string();
                session.publishes.insert(
                    event.id,
                    (reply, Instant::now() + self.options.publish_timeout),
                );
                vec![message]
            }
            Command::Reconnect => Vec::new(),
        }
    }

    fn resubscribe_all(&mut self) -> Vec<String> {
        let resubscribed = self.subscribed_once;
        self.subscribed_once = true;
        self.subscriptions
            .retain(|_, open| !open.events.is_closed());
        self.subscriptions
            .iter()
            .map(|(id, open)| {
                if resubscribed {
                    let _ = open.events.send(SubscriptionEvent::Resubscribed);
                }
                req(*id, &open.filters)
            })
            .collect()
    }

    fn receive(&mut self, session: &mut Session, text: &str) -> Vec<String> {
        let Ok(Value::Array(message)) = serde_json::from_str::<Value>(text) else {
            tracing::debug!(origin = %self.origin, "ignoring malformed relay message");
            return Vec::new();
        };
        let kind = message.first().and_then(Value::as_str).unwrap_or("");
        match kind {
            "EVENT" => {
                let (Some(id), Some(event)) =
                    (message.get(1).and_then(parse_sub_id), message.get(2))
                else {
                    return Vec::new();
                };
                let Some(open) = self.subscriptions.get(&id) else {
                    return Vec::new();
                };
                match self.verifier.verify_json(event) {
                    Ok(event) => {
                        let _ = open.events.send(SubscriptionEvent::Event(event));
                    }
                    Err(_) => {
                        tracing::warn!(origin = %self.origin, "dropping event with a bad signature")
                    }
                }
                Vec::new()
            }
            "EOSE" => {
                if let Some(open) = message
                    .get(1)
                    .and_then(parse_sub_id)
                    .and_then(|id| self.subscriptions.get(&id))
                {
                    let _ = open.events.send(SubscriptionEvent::Eose);
                }
                Vec::new()
            }
            "CLOSED" => {
                let Some(id) = message.get(1).and_then(parse_sub_id) else {
                    return Vec::new();
                };
                let reason = message
                    .get(2)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if reason.starts_with("auth-required:") && !session.authenticated {
                    // Kept, and re-sent once authentication completes.
                    return Vec::new();
                }
                if let Some(open) = self.subscriptions.remove(&id) {
                    let _ = open.events.send(SubscriptionEvent::Closed(reason));
                }
                Vec::new()
            }
            "OK" => {
                let (Some(id), Some(accepted)) = (
                    message
                        .get(1)
                        .and_then(Value::as_str)
                        .and_then(|id| EventId::from_hex(id).ok()),
                    message.get(2).and_then(Value::as_bool),
                ) else {
                    return Vec::new();
                };
                let reason = message.get(3).and_then(Value::as_str).unwrap_or("");
                if session.auth_event == Some(id) {
                    if accepted {
                        session.ready = true;
                        session.authenticated = true;
                        return self.resubscribe_all();
                    }
                    tracing::warn!(origin = %self.origin, %reason, "relay refused authentication");
                    return Vec::new();
                }
                if let Some((reply, _)) = session.publishes.remove(&id) {
                    // A duplicate means the relay already has these exact bytes.
                    let result = if accepted || reason.starts_with("duplicate:") {
                        Ok(())
                    } else {
                        Err(PublishError::Rejected(reason.to_string()))
                    };
                    let _ = reply.send(result);
                }
                Vec::new()
            }
            "AUTH" => {
                let Some(challenge) = message.get(1).and_then(Value::as_str) else {
                    return Vec::new();
                };
                match self.identity.relay_auth(self.origin.websocket(), challenge) {
                    Ok(event) => {
                        session.auth_event = Some(event.id);
                        vec![json!(["AUTH", event]).to_string()]
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not sign relay authentication");
                        Vec::new()
                    }
                }
            }
            "NOTICE" => {
                let notice = message.get(1).and_then(Value::as_str).unwrap_or("");
                tracing::info!(origin = %self.origin, %notice, "relay notice");
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

#[derive(Default)]
struct Session {
    /// Subscriptions may be sent.
    ready: bool,
    /// `SocketState::Ready` was published for this session.
    announced: bool,
    /// The AUTH event sent for this connection, if the relay asked.
    auth_event: Option<EventId>,
    /// The relay accepted our AUTH.
    authenticated: bool,
    publishes: HashMap<EventId, (oneshot::Sender<Result<(), PublishError>>, Instant)>,
}

impl Session {
    async fn next_publish_deadline(&self) {
        match self.publishes.values().map(|(_, deadline)| *deadline).min() {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => futures::future::pending().await,
        }
    }

    fn expire_publishes(&mut self) {
        let now = Instant::now();
        let expired: Vec<EventId> = self
            .publishes
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some((reply, _)) = self.publishes.remove(&id) {
                let _ = reply.send(Err(PublishError::TimedOut));
            }
        }
    }
}

fn req(id: u64, filters: &[Filter]) -> String {
    let mut message = vec![json!("REQ"), json!(sub_id(id))];
    message.extend(filters.iter().cloned().map(Value::Object));
    Value::Array(message).to_string()
}

fn sub_id(id: u64) -> String {
    format!("s{id}")
}

fn parse_sub_id(value: &Value) -> Option<u64> {
    value.as_str()?.strip_prefix('s')?.parse().ok()
}

/// Exponential backoff with jitter in [50%, 100%], so many clients
/// dropped together do not reconnect in lockstep.
fn backoff(options: &SocketOptions, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let full = options
        .min_backoff
        .saturating_mul(1 << exponent)
        .min(options.max_backoff);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    full.mul_f64(0.5 + f64::from(nanos % 1000) / 2000.0)
}
