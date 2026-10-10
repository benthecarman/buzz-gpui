use crate::frame::{self, Frame, Hello, HostInfo, PROTOCOL_VERSION, Unknown, Welcome, Wire};
use crate::transport::{BoxConnection, Listener};
use crate::{Protocol, RpcError};
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, mpsc, oneshot};

/// Events the host broadcasts, tagged with a sequence number that increases
/// for the lifetime of the host process.
pub type EventSource<P> = broadcast::Receiver<(u64, <P as Protocol>::Event)>;

pub trait HostHandler<P: Protocol>: Send + Sync + 'static {
    fn info(&self) -> HostInfo;
    fn events(&self) -> EventSource<P>;
    fn handle(
        &self,
        peer: Peer<P>,
        request: P::ClientRequest,
    ) -> BoxFuture<'static, Result<P::ClientResponse, RpcError>>;
}

#[derive(Clone)]
pub struct ServerOptions {
    /// Required from every client when set. Only the in-memory transport
    /// inside the GUI process should run without one.
    pub token: Option<String>,
    pub handshake_timeout: Duration,
    /// A client that sends nothing (not even a ping) for this long is dropped.
    pub idle_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            token: None,
            handshake_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(120),
        }
    }
}

/// A connected GUI, used by the host to send it requests.
pub struct Peer<P: Protocol> {
    address: Arc<str>,
    outgoing: mpsc::Sender<Frame<P>>,
    pending: crate::client::Pending<P::HostResponse>,
    next_id: Arc<AtomicU64>,
    timeout: Duration,
}

impl<P: Protocol> Clone for Peer<P> {
    fn clone(&self) -> Self {
        Self {
            address: self.address.clone(),
            outgoing: self.outgoing.clone(),
            pending: self.pending.clone(),
            next_id: self.next_id.clone(),
            timeout: self.timeout,
        }
    }
}

impl<P: Protocol> Peer<P> {
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Resolves once this connection has closed, so work the GUI started
    /// over it can stop with it.
    pub async fn closed(&self) {
        self.outgoing.closed().await
    }

    /// Whether `other` came from the same connection as this peer.
    pub fn same_connection(&self, other: &Self) -> bool {
        self.outgoing.same_channel(&other.outgoing)
    }

    pub async fn request(&self, body: P::HostRequest) -> Result<P::HostResponse, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        if self
            .outgoing
            .send(Frame::HostRequest { id, body })
            .await
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            return Err(RpcError::Disconnected);
        }
        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Disconnected),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(RpcError::TimedOut)
            }
        }
    }
}

/// Accepts connections until the listener fails. Each connection runs on its
/// own task, so one slow or broken client never blocks another. Dropping the
/// returned future closes every connection it accepted.
pub async fn serve<P: Protocol, L: Listener>(
    mut listener: L,
    handler: Arc<dyn HostHandler<P>>,
    options: ServerOptions,
) -> std::io::Result<()> {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (connection, address) = accepted?;
                let handler = handler.clone();
                let options = options.clone();
                connections.spawn(async move {
                    if let Err(error) = run_connection(connection, &address, handler, options).await {
                        tracing::debug!(%address, %error, "host connection closed");
                    }
                });
            }
            Some(_) = connections.join_next() => {}
        }
    }
}

async fn run_connection<P: Protocol>(
    connection: BoxConnection,
    address: &str,
    handler: Arc<dyn HostHandler<P>>,
    options: ServerOptions,
) -> std::io::Result<()> {
    let mut wire = frame::wire(connection);
    let hello =
        match tokio::time::timeout(options.handshake_timeout, frame::receive::<P>(&mut wire)).await
        {
            Ok(Ok(Frame::Hello(hello))) => hello,
            Ok(Ok(_)) => return reject::<P>(&mut wire, "expected hello").await,
            Ok(Err(error)) => return Err(error),
            Err(_) => return reject::<P>(&mut wire, "handshake timed out").await,
        };
    if let Some(reason) = check_hello(&hello, options.token.as_deref()) {
        tracing::warn!(%address, client = %hello.client, reason, "rejected host client");
        return reject::<P>(&mut wire, reason).await;
    }
    // Subscribe before welcoming, so no event between the client's snapshot
    // request and its subscription can be missed.
    let mut events = handler.events();
    frame::send::<P>(
        &mut wire,
        &Frame::Welcome(Welcome {
            protocol: PROTOCOL_VERSION,
            host: handler.info(),
        }),
    )
    .await?;
    tracing::info!(%address, client = %hello.client, "host client connected");

    let (mut sink, mut stream) = wire.split();
    let (outgoing, mut queue) = mpsc::channel::<Frame<P>>(256);
    let peer = Peer {
        address: address.into(),
        outgoing: outgoing.clone(),
        pending: Arc::default(),
        next_id: Arc::new(AtomicU64::new(1)),
        timeout: options.request_timeout,
    };

    // Everything spawned for this connection stops with it.
    let mut writer = tokio::task::JoinSet::new();
    writer.spawn(async move {
        while let Some(frame) = queue.recv().await {
            sink.send(frame::encode(&frame)?).await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let mut tasks = tokio::task::JoinSet::new();
    let idle = tokio::time::sleep(options.idle_timeout);
    tokio::pin!(idle);

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let bytes = match incoming {
                    Some(Ok(bytes)) => bytes,
                    Some(Err(error)) => return Err(error),
                    None => return Ok(()),
                };
                idle.as_mut().reset(tokio::time::Instant::now() + options.idle_timeout);
                let frame = match frame::decode::<Frame<P>>(&bytes) {
                    Ok(frame) => frame,
                    Err(error) => match frame::peek(&bytes) {
                        Some(Unknown::Request(id)) => Frame::ClientResponse {
                            id,
                            body: Err(frame::unsupported()),
                        },
                        _ => return Err(error),
                    },
                };
                match frame {
                    // Only made above, for a request this host cannot read.
                    Frame::ClientResponse { id, body } => {
                        if outgoing.send(Frame::ClientResponse { id, body }).await.is_err() {
                            return Ok(());
                        }
                    }
                    Frame::ClientRequest { id, body } => {
                        let response = handler.handle(peer.clone(), body);
                        let outgoing = outgoing.clone();
                        tasks.spawn(async move {
                            let body = response.await;
                            let _ = outgoing.send(Frame::ClientResponse { id, body }).await;
                        });
                    }
                    Frame::HostResponse { id, body } => {
                        if let Some(sender) = peer.pending.lock().await.remove(&id) {
                            let _ = sender.send(body);
                        }
                    }
                    Frame::Ping { nonce } => {
                        if outgoing.send(Frame::Pong { nonce }).await.is_err() {
                            return Ok(());
                        }
                    }
                    _ => return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "unexpected frame from client",
                    )),
                }
            }
            event = events.recv() => {
                let frame = match event {
                    Ok((seq, body)) => Frame::Event { seq, body },
                    Err(broadcast::error::RecvError::Lagged(_)) => Frame::Resync,
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                };
                if outgoing.send(frame).await.is_err() {
                    return Ok(());
                }
            }
            _ = &mut idle => return Err(std::io::ErrorKind::TimedOut.into()),
            Some(_) = tasks.join_next() => {}
            Some(result) = writer.join_next() => {
                return match result {
                    Ok(result) => result,
                    Err(error) => Err(std::io::Error::other(error)),
                };
            }
        }
    }
}

fn check_hello(hello: &Hello, token: Option<&str>) -> Option<&'static str> {
    if hello.protocol != PROTOCOL_VERSION {
        return Some("protocol version mismatch");
    }
    let expected = token?;
    let offered = hello.token.as_deref().unwrap_or("");
    let matches: bool = offered.as_bytes().ct_eq(expected.as_bytes()).into();
    (!matches).then_some("invalid token")
}

async fn reject<P: Protocol>(wire: &mut Wire, reason: &str) -> std::io::Result<()> {
    frame::send::<P>(
        wire,
        &Frame::Rejected {
            reason: reason.into(),
        },
    )
    .await?;
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        reason,
    ))
}
