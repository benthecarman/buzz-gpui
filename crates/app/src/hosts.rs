//! Every agent host the GUI manages: the one embedded in this process and any
//! remote `buzzd` instances from the config file.

use crate::config::{Config, CredentialsBackend, HOST_TOKEN_SERVICE, RemoteHost};
use crate::runtime::Runtime;
use buzz_host::api::{
    AgentDefaults, AgentsSnapshot, Harness, HostAnswer, HostAsk, HostEvent, HostProtocol, Request,
    Response,
};
use buzz_host::credentials::{FileCredentials, KeyringCredentials};
use buzz_host::{AgentHost, HostConfig, RpcHandler};
use buzz_host_rpc::transport::{self, Connector, TcpConnector, UnixConnector};
use buzz_host_rpc::{
    ClientEvent, ClientHandler, ConnectionState, HostClient, HostClientOptions, HostInfo, RpcError,
    ServerOptions,
};
use buzz_identity::Identity;
use futures::StreamExt;
use futures::future::BoxFuture;
use gpui_kit::{App, AppContext, Context, Entity, Global, SharedString, Task};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
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
    pub remote: Option<RemoteHost>,
    pub state: ConnectionState,
    pub agents: AgentsSnapshot,
    pub harnesses: Option<Vec<Harness>>,
    /// Fetched when a form needs them, not on connect: hosts older than
    /// this request drop the connection on it.
    pub defaults: Option<AgentDefaults>,
    signing: SigningSlot,
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
                Ok(connector) => {
                    let token = remote_token(remote);
                    hosts.push(HostConnection::new(
                        remote.name.clone().into(),
                        Some(remote.clone()),
                        connector,
                        token,
                        cx,
                    ))
                }
                Err(error) => tracing::error!(host = %remote.name, %error, "invalid host"),
            }
        }
        cx.set_global(Self {
            hosts,
            _embedded: embedded,
        });
    }

    fn remotes(&self, cx: &App) -> Vec<RemoteHost> {
        self.hosts
            .iter()
            .filter_map(|host| host.read(cx).remote.clone())
            .collect()
    }

    /// Saves a new remote host and connects to it. The token goes to the OS
    /// keyring; without one it is kept in the config file instead, which
    /// the returned host records in its `token` field.
    pub fn add(name: &str, address: &str, token: &str, cx: &mut App) -> Result<RemoteHost, String> {
        let (name, address, token) = (name.trim(), address.trim(), token.trim());
        if name.is_empty() {
            return Err("Enter a name for this host.".into());
        }
        if token.is_empty() {
            return Err("Enter the host's token. Run `buzzd token` on that machine.".into());
        }
        let mut remote = RemoteHost {
            name: name.into(),
            address: address.into(),
            token: None,
        };
        let connector = remote_connector(&remote)?;
        let mut remotes = cx.global::<Self>().remotes(cx);
        if remotes.iter().any(|other| other.address == address) {
            return Err("That host is already added.".into());
        }
        let keyring = store_token(address, token);
        if let Err(error) = &keyring {
            tracing::warn!(
                ?error,
                "keyring unavailable; keeping the host token in the config file"
            );
            remote.token = Some(token.into());
        }
        remotes.push(remote.clone());
        if let Err(error) = crate::config::save_hosts(&remotes) {
            if keyring.is_ok() {
                let _ = buzz_credential_store::delete(HOST_TOKEN_SERVICE, address);
            }
            return Err(error);
        }
        let connection = HostConnection::new(
            name.to_string().into(),
            Some(remote.clone()),
            connector,
            Some(token.into()),
            cx,
        );
        cx.global_mut::<Self>().hosts.push(connection);
        Ok(remote)
    }

    /// Disconnects from a remote host and forgets it and its token. Its
    /// agents keep running on that machine.
    pub fn remove(host: &Entity<HostConnection>, cx: &mut App) -> Result<(), String> {
        let Some(remote) = host.read(cx).remote.clone() else {
            return Err("This computer's host cannot be removed.".into());
        };
        let remotes: Vec<RemoteHost> = cx
            .global::<Self>()
            .remotes(cx)
            .into_iter()
            .filter(|other| other.address != remote.address)
            .collect();
        crate::config::save_hosts(&remotes)?;
        match buzz_credential_store::delete(HOST_TOKEN_SERVICE, &remote.address) {
            Ok(()) | Err(buzz_credential_store::Error::Absent) => {}
            Err(error) => tracing::warn!(?error, "could not delete the host token"),
        }
        let id = host.entity_id();
        cx.global_mut::<Self>()
            .hosts
            .retain(|host| host.entity_id() != id);
        Ok(())
    }
}

/// Replaces any token saved for this address.
fn store_token(address: &str, token: &str) -> Result<(), buzz_credential_store::Error> {
    match buzz_credential_store::delete(HOST_TOKEN_SERVICE, address) {
        Ok(()) | Err(buzz_credential_store::Error::Absent) => {}
        Err(error) => return Err(error),
    }
    buzz_credential_store::add(HOST_TOKEN_SERVICE, address, token.as_bytes())
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
        legacy_dir: dirs::data_dir(),
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
    if let Some(address) = host.address.strip_prefix("tcp://")
        && !address.is_empty()
    {
        Ok(Arc::new(TcpConnector {
            address: address.into(),
        }))
    } else if let Some(path) = host.address.strip_prefix("unix://")
        && !path.is_empty()
    {
        Ok(Arc::new(UnixConnector { path: path.into() }))
    } else {
        Err("Enter an address that starts with tcp:// or unix://.".into())
    }
}

/// Tokens live in the OS keyring under the host's address. A token in the
/// config file is accepted for machines without one.
fn remote_token(host: &RemoteHost) -> Option<String> {
    if let Some(token) = &host.token {
        return Some(token.clone());
    }
    buzz_credential_store::read(HOST_TOKEN_SERVICE, &host.address)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes.to_vec()).ok())
}

/// What the GUI will sign for a host: armed by an import the user started,
/// for exactly the keys they chose, so a host cannot get any other agent
/// key attested to the user.
struct Signing {
    identity: Arc<Identity>,
    pubkeys: HashSet<String>,
}

type SigningSlot = Arc<Mutex<Option<Signing>>>;

struct Answers(SigningSlot);

impl ClientHandler<HostProtocol> for Answers {
    fn handle(&self, ask: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        let HostAsk::AttestAgent { agent_pubkey } = ask;
        let mut slot = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let answer = match slot.as_mut() {
            Some(signing) if signing.pubkeys.contains(&agent_pubkey) => {
                signing.pubkeys.remove(&agent_pubkey);
                signing
                    .identity
                    .attest_agent(&agent_pubkey)
                    .map(HostAnswer::Attestation)
                    .map_err(|error| RpcError::Failed(error.to_string()))
            }
            _ => Err(RpcError::Failed(
                "This app did not ask to import that agent".into(),
            )),
        };
        Box::pin(async move { answer })
    }
}

impl HostConnection {
    fn new(
        label: SharedString,
        remote: Option<RemoteHost>,
        connector: Arc<dyn Connector>,
        token: Option<String>,
        cx: &mut App,
    ) -> Entity<Self> {
        let runtime = Runtime::handle(cx);
        let signing = SigningSlot::default();
        let client = Arc::new(HostClient::<HostProtocol>::spawn(
            &runtime,
            connector,
            Arc::new(Answers(signing.clone())),
            HostClientOptions {
                token,
                ..Default::default()
            },
        ));
        cx.new(|cx| {
            let watch = Self::watch(&client, &runtime, cx);
            Self {
                label,
                remote,
                state: client.state().borrow().clone(),
                agents: AgentsSnapshot::default(),
                harnesses: None,
                defaults: None,
                signing,
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
            match &response {
                Ok(Response::Agents(snapshot)) => {
                    let snapshot = snapshot.clone();
                    let _ = this.update(cx, |this, cx| {
                        this.apply(snapshot);
                        cx.notify();
                    });
                }
                Ok(Response::Defaults(defaults)) => {
                    let defaults = defaults.clone();
                    let _ = this.update(cx, |this, cx| {
                        this.defaults = Some(defaults);
                        cx.notify();
                    });
                }
                _ => {}
            }
            response
        })
    }

    /// Loads the host's agent defaults into [`Self::defaults`].
    pub fn load_defaults(&self, cx: &mut Context<Self>) {
        self.request(Request::Defaults, cx).detach();
    }

    /// Commits an import. While it runs, the host may ask this app to attest
    /// the given agent keys, and only those, with `identity`.
    pub fn import(
        &self,
        token: String,
        ids: Vec<String>,
        pubkeys: HashSet<String>,
        identity: Arc<Identity>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Response, RpcError>> {
        let slot = self.signing.clone();
        *slot.lock().unwrap_or_else(|poison| poison.into_inner()) =
            Some(Signing { identity, pubkeys });
        let response = self.request(Request::ImportCommit { token, ids }, cx);
        cx.spawn(async move |_, _| {
            let response = response.await;
            *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
            response
        })
    }

    pub fn reconnect(&self) {
        self.client.reconnect();
    }
}
