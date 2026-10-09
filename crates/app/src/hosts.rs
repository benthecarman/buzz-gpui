//! Every agent host the GUI manages: the one embedded in this process and any
//! remote `buzzd` instances from the config file.

use crate::config::{Config, CredentialsBackend, RemoteHost};
use crate::runtime::Runtime;
use buzz_host::api::{
    AgentsSnapshot, Harness, HostAnswer, HostAsk, HostEvent, HostProtocol, Request, Response,
};
use buzz_host::credentials::{FileCredentials, KeyringCredentials};
use buzz_host::{AgentHost, HostConfig, RpcHandler};
use buzz_host_rpc::transport::{self, Connector, TcpConnector, UnixConnector};
use buzz_host_rpc::{
    ClientEvent, ClientHandler, ConnectionState, HostClient, HostClientOptions, HostInfo, RpcError,
    ServerOptions,
};
use futures::StreamExt;
use futures::future::BoxFuture;
use gpui_kit::{App, AppContext, Context, Entity, Global, SharedString, Task};
use std::sync::Arc;
use std::time::Duration;

/// Agent keys created on this machine use the same credential service as
/// buzz-app, so both apps see them.
const AGENT_KEY_SERVICE: &str = "dev.local.buzz.foundation.agents";

pub struct Hosts {
    pub hosts: Vec<Entity<HostConnection>>,
    /// Owns the embedded host's server; dropping it stops local agents.
    _embedded: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Global for Hosts {}

pub struct HostConnection {
    pub label: SharedString,
    /// `None` for the host inside this process.
    pub address: Option<SharedString>,
    pub state: ConnectionState,
    pub agents: AgentsSnapshot,
    pub harnesses: Option<Vec<Harness>>,
    client: Arc<HostClient<HostProtocol>>,
    _watch: Task<()>,
}

impl Hosts {
    pub fn init(config: &Config, cx: &mut App) {
        let runtime = Runtime::handle(cx);
        let mut hosts = Vec::new();
        let embedded = match start_embedded(config, &runtime) {
            Ok((connector, server)) => {
                hosts.push(HostConnection::new(
                    "This computer".into(),
                    None,
                    Arc::new(connector),
                    None,
                    cx,
                ));
                Some(server)
            }
            Err(error) => {
                tracing::error!(%error, "could not start the local agent host");
                None
            }
        };
        for remote in &config.hosts {
            match remote_connector(remote) {
                Ok(connector) => hosts.push(HostConnection::new(
                    remote.name.clone().into(),
                    Some(remote.address.clone().into()),
                    connector,
                    remote_token(remote),
                    cx,
                )),
                Err(error) => tracing::error!(host = %remote.name, %error, "invalid host"),
            }
        }
        cx.set_global(Self {
            hosts,
            _embedded: embedded,
        });
    }
}

fn start_embedded(
    config: &Config,
    runtime: &tokio::runtime::Handle,
) -> Result<
    (
        transport::MemoryConnector,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ),
    String,
> {
    let data = dirs::data_dir()
        .ok_or("No data directory")?
        .join("buzz-gpui");
    let credentials: Arc<dyn buzz_host::Credentials> = match config.embedded.credentials {
        CredentialsBackend::Keyring => Arc::new(KeyringCredentials::new(AGENT_KEY_SERVICE)),
        CredentialsBackend::Files => Arc::new(
            FileCredentials::new(data.join("agent-keys")).map_err(|error| error.to_string())?,
        ),
    };
    // The host starts background work on the I/O runtime.
    let _runtime = runtime.enter();
    let host = AgentHost::start(HostConfig {
        info: HostInfo {
            id: "local".into(),
            name: "This computer".into(),
            platform: std::env::consts::OS.into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        state_dir: data.join("host"),
        workspace: config
            .embedded
            .workspace
            .clone()
            .or_else(|| dirs::home_dir().map(|home| home.join("buzz-agents")))
            .ok_or("No home directory")?,
        credentials,
        watch_interval: Duration::from_secs(1),
    })?;
    let (connector, listener) = transport::memory();
    let server = runtime.spawn(buzz_host_rpc::serve(
        listener,
        Arc::new(RpcHandler(host)),
        // In-process only, so there is nobody to authenticate.
        ServerOptions::default(),
    ));
    Ok((connector, server))
}

fn remote_connector(host: &RemoteHost) -> Result<Arc<dyn Connector>, String> {
    if let Some(address) = host.address.strip_prefix("tcp://") {
        Ok(Arc::new(TcpConnector {
            address: address.into(),
        }))
    } else if let Some(path) = host.address.strip_prefix("unix://") {
        Ok(Arc::new(UnixConnector { path: path.into() }))
    } else {
        Err(format!(
            "address {:?} must start with tcp:// or unix://",
            host.address
        ))
    }
}

/// Tokens live in the OS keyring under the host's address. A token in the
/// config file is accepted for machines without one.
fn remote_token(host: &RemoteHost) -> Option<String> {
    if let Some(token) = &host.token {
        return Some(token.clone());
    }
    buzz_credential_store::read(crate::config::HOST_TOKEN_SERVICE, &host.address)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
}

/// The host never asks the GUI anything yet.
struct Answers;

impl ClientHandler<HostProtocol> for Answers {
    fn handle(&self, ask: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        match ask {}
    }
}

impl HostConnection {
    fn new(
        label: SharedString,
        address: Option<SharedString>,
        connector: Arc<dyn Connector>,
        token: Option<String>,
        cx: &mut App,
    ) -> Entity<Self> {
        let runtime = Runtime::handle(cx);
        let client = Arc::new(HostClient::<HostProtocol>::spawn(
            &runtime,
            connector,
            Arc::new(Answers),
            HostClientOptions {
                token,
                ..Default::default()
            },
        ));
        cx.new(|cx| {
            let watch = Self::watch(&client, &runtime, cx);
            Self {
                label,
                address,
                state: client.state().borrow().clone(),
                agents: AgentsSnapshot::default(),
                harnesses: None,
                client,
                _watch: watch,
            }
        })
    }

    /// Mirrors connection state and host events into this entity.
    fn watch(
        client: &Arc<HostClient<HostProtocol>>,
        runtime: &tokio::runtime::Handle,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        enum Update {
            State(ConnectionState),
            Event(ClientEvent<HostEvent>),
        }
        let (sender, mut updates) = futures::channel::mpsc::unbounded();
        let mut states = client.state();
        let mut events = client.events();
        runtime.spawn(async move {
            loop {
                let update = tokio::select! {
                    changed = states.changed() => match changed {
                        Ok(()) => Update::State(states.borrow_and_update().clone()),
                        Err(_) => return,
                    },
                    event = events.recv() => match event {
                        Ok(event) => Update::Event(event),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            Update::Event(ClientEvent::Resync)
                        }
                        Err(_) => return,
                    },
                };
                if sender.unbounded_send(update).is_err() {
                    return;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            while let Some(update) = updates.next().await {
                let refresh = this.update(cx, |this, cx| {
                    let refresh = match update {
                        Update::State(state) => {
                            this.state = state;
                            false
                        }
                        Update::Event(ClientEvent::Connected(_) | ClientEvent::Resync) => true,
                        Update::Event(ClientEvent::Event {
                            body: HostEvent::Agents(snapshot),
                            ..
                        }) => {
                            this.apply(snapshot);
                            false
                        }
                        Update::Event(ClientEvent::Disconnected) => false,
                    };
                    cx.notify();
                    refresh
                });
                match refresh {
                    Ok(true) => {
                        if let Ok(task) = this.update(cx, |this, cx| this.refresh(cx)) {
                            task.detach();
                        }
                    }
                    Ok(false) => {}
                    Err(_) => return,
                }
            }
        })
    }

    /// Ignores snapshots older than the one already shown.
    fn apply(&mut self, snapshot: AgentsSnapshot) {
        if snapshot.seq >= self.agents.seq {
            self.agents = snapshot;
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let agents = self.request(Request::Agents, cx);
        let harnesses = self.request(Request::Harnesses, cx);
        cx.spawn(async move |this, cx| {
            let (agents, harnesses) = futures::join!(agents, harnesses);
            let _ = this.update(cx, |this, cx| {
                if let Ok(Response::Agents(snapshot)) = agents {
                    this.apply(snapshot);
                }
                if let Ok(Response::Harnesses(list)) = harnesses {
                    this.harnesses = Some(list);
                }
                cx.notify();
            });
        })
    }

    /// Runs a request on the I/O runtime. Responses that carry a snapshot
    /// are applied here, so callers only handle errors.
    pub fn request(
        &self,
        request: Request,
        cx: &mut Context<Self>,
    ) -> Task<Result<Response, RpcError>> {
        let client = self.client.clone();
        let response = Runtime::handle(cx).spawn(async move { client.request(request).await });
        cx.spawn(async move |this, cx| {
            let response = response
                .await
                .unwrap_or_else(|_| Err(RpcError::Failed("Request was cancelled".into())));
            if let Ok(Response::Agents(snapshot)) = &response {
                let snapshot = snapshot.clone();
                let _ = this.update(cx, |this, cx| {
                    this.apply(snapshot);
                    cx.notify();
                });
            }
            response
        })
    }

    pub fn reconnect(&self) {
        self.client.reconnect();
    }
}
