//! An agent host: runs Buzz agents on this machine.
//!
//! The controller from buzz-app must have one serialized owner, so it lives on
//! a dedicated thread and every operation is a job sent to that thread. The
//! thread also watches process status, so GUIs receive changes they did not
//! cause (an agent crashing, finishing a restore) without polling.

pub mod api;
pub mod credentials;
pub mod harness;
mod profile;

use api::{AgentAction, AgentsSnapshot, HostEvent, HostProtocol, Request, Response};
pub use buzz_agent_controller::Credentials;
use buzz_agent_controller::{Action, Controller, NewAgent, RuntimeBundle, Store};
use buzz_host_rpc::{EventSource, HostHandler, HostInfo, Peer, RpcError};
use futures::future::BoxFuture;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

/// Call first in `main`, before any other setup. When this process was
/// started as an agent supervisor it runs that supervisor and never returns.
pub fn dispatch_supervisor() {
    buzz_agent_controller::dispatch_agent_supervisor();
}

pub struct HostConfig {
    pub info: HostInfo,
    /// Holds the controller's store, run directories and locks.
    pub state_dir: PathBuf,
    /// Default working directory for new agents.
    pub workspace: PathBuf,
    pub credentials: Arc<dyn Credentials>,
    /// How often process status is checked for changes nobody requested.
    pub watch_interval: Duration,
}

type Job = Box<dyn FnOnce(&mut Owner) + Send>;

pub struct AgentHost {
    info: HostInfo,
    http: reqwest::Client,
    /// Lets background work started from `&self` keep the host alive.
    weak: std::sync::Weak<AgentHost>,
    jobs: mpsc::Sender<Job>,
    events: broadcast::Sender<(u64, HostEvent)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Owner {
    controller: Controller,
    credentials: Arc<dyn Credentials>,
    workspace: PathBuf,
    /// The pending create: request ID and its generated key.
    creating: Option<(String, Arc<NewAgent>)>,
    last: Option<AgentsSnapshot>,
    seq: u64,
    events: broadcast::Sender<(u64, HostEvent)>,
}

impl AgentHost {
    /// Opens the store, starts agents marked to start with the host, and
    /// retries agent profiles left unpublished. Must run inside tokio.
    pub fn start(config: HostConfig) -> Result<Arc<Self>, String> {
        buzz_agent_controller::warm_tools_path();
        std::fs::create_dir_all(&config.workspace)
            .map_err(|_| "Could not create the agent workspace")?;
        if let Err(error) = buzz_agent_controller::ensure_buzz_cli_skill(&config.workspace) {
            tracing::warn!(%error, "could not install the buzz CLI skill");
        }
        let store = Store::open(config.state_dir.join("agent-controller"))?;
        let controller = Controller::new(
            store,
            config.credentials.clone(),
            Ok(RuntimeBundle::installed()),
            config.state_dir.join("agent-ownership"),
        );
        let (events, _) = broadcast::channel(256);
        let (jobs, queue) = mpsc::channel::<Job>();
        let mut owner = Owner {
            controller,
            credentials: config.credentials,
            workspace: config.workspace,
            creating: None,
            last: None,
            seq: 0,
            events: events.clone(),
        };
        let interval = config.watch_interval;
        let thread = std::thread::Builder::new()
            .name("agent-host".into())
            .spawn(move || {
                if let Err(error) = owner.controller.restore() {
                    tracing::warn!(%error, "could not restore agents");
                }
                owner.publish();
                loop {
                    match queue.recv_timeout(interval) {
                        Ok(job) => job(&mut owner),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                    owner.publish();
                }
                if let Err(error) = owner.controller.shutdown() {
                    tracing::warn!(%error, "agent shutdown failed");
                }
            })
            .map_err(|_| "Could not start the agent host thread")?;
        let host = Arc::new_cyclic(|weak| Self {
            info: config.info,
            http: profile::client(),
            weak: weak.clone(),
            jobs,
            events,
            thread: Some(thread),
        });
        host.publish_pending_profiles();
        Ok(host)
    }

    async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Owner) -> Result<T, String> + Send + 'static,
    ) -> Result<T, RpcError> {
        let (sender, receiver) = oneshot::channel();
        self.jobs
            .send(Box::new(move |owner| {
                let _ = sender.send(job(owner));
            }))
            .map_err(|_| RpcError::Failed("The agent host stopped".into()))?;
        receiver
            .await
            .map_err(|_| RpcError::Failed("The agent host stopped".into()))?
            .map_err(RpcError::Failed)
    }

    pub async fn request(&self, request: Request) -> Result<Response, RpcError> {
        match request {
            Request::Agents => {
                self.run(|owner| owner.snapshot().map(Response::Agents))
                    .await
            }
            Request::Harnesses => tokio::task::spawn_blocking(harness::detect)
                .await
                .map(Response::Harnesses)
                .map_err(|_| RpcError::Failed("Harness detection failed".into())),
            Request::Action { id, action } => {
                self.run(move |owner| {
                    let action = match action {
                        AgentAction::Start => Action::Start,
                        AgentAction::Stop => Action::Stop,
                        AgentAction::Restart => Action::Restart,
                    };
                    owner.controller.action(&id, action)?;
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            Request::SetStartOnLaunch { id, value } => {
                self.run(move |owner| {
                    owner.controller.set_start_on_app_launch(&id, value)?;
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            Request::Save { id, revision, edit } => {
                self.run(move |owner| {
                    owner
                        .controller
                        .save(&id, revision, controller_edit(&edit)?)?;
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            Request::Delete { id, revision } => {
                self.run(move |owner| {
                    owner.controller.delete(&id, revision)?;
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            Request::CreatePrepare {
                request_id,
                relay_url,
                owner: human,
            } => {
                self.run(move |owner| {
                    if let Some((id, prepared)) = &owner.creating
                        && *id == request_id
                    {
                        if !prepared.matches(&relay_url, &human)? {
                            return Err("Create destination or owner changed".into());
                        }
                        return Ok(prepared_response(prepared));
                    }
                    let prepared = Arc::new(NewAgent::prepare(&relay_url, &human)?);
                    let response = prepared_response(&prepared);
                    owner.creating = Some((request_id, prepared));
                    Ok(response)
                })
                .await
            }
            Request::CreateCommit {
                request_id,
                edit,
                auth,
            } => self
                .run(move |owner| {
                    let prepared = owner
                        .creating
                        .as_ref()
                        .filter(|(id, _)| *id == request_id)
                        .map(|(_, prepared)| prepared.clone())
                        .ok_or("Create request expired; start again")?;
                    let mut edit = controller_edit(&edit)?;
                    if edit.workspace.is_empty() {
                        edit.workspace = owner.workspace.to_string_lossy().into_owned();
                    }
                    prepared.validate(edit.clone(), &auth)?;
                    prepared.save_key(owner.credentials.as_ref())?;
                    owner.controller.create(&prepared, edit, &auth)?;
                    owner.creating = None;
                    owner
                        .snapshot()
                        .map(|snapshot| (snapshot, prepared.id.clone()))
                })
                .await
                .map(|(snapshot, id)| {
                    self.publish_profile_later(id);
                    Response::Agents(snapshot)
                }),
            Request::PublishProfile { id } => {
                self.publish_profile(&id).await.map_err(RpcError::Failed)?;
                self.run(|owner| owner.snapshot().map(Response::Agents))
                    .await
            }
        }
    }
}

impl AgentHost {
    /// Publishes the agent's pending profile and records it, unless the
    /// agent's settings changed meanwhile.
    async fn publish_profile(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        let target = id.clone();
        let (profile, key) = self
            .run(move |owner| {
                let profile = owner.controller.creation_profile(&target)?;
                let key = owner
                    .credentials
                    .read(&profile.credential_id, &profile.pubkey)?
                    .ok_or("The agent's key is unavailable")?;
                Ok((profile, key))
            })
            .await
            .map_err(|error| error.to_string())?;
        profile::publish(&self.http, &profile, &key).await?;
        let revision = profile.revision;
        self.run(move |owner| {
            let current = owner.controller.creation_profile(&id)?;
            if current.revision != revision {
                return Err("The agent changed while publishing; retry".into());
            }
            owner.controller.profile_published(&id, revision)
        })
        .await
        .map_err(|error| error.to_string())
    }

    /// Fire-and-forget publication; failures leave the profile pending,
    /// which the snapshot shows.
    fn publish_profile_later(&self, id: String) {
        let Some(host) = self.weak.upgrade() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(error) = host.publish_profile(&id).await {
                tracing::warn!(agent = %id, %error, "agent profile publication failed");
            }
        });
    }

    /// Retries profiles left pending, e.g. by a crash or an offline relay.
    pub fn publish_pending_profiles(self: &Arc<Self>) {
        let host = self.clone();
        tokio::spawn(async move {
            let Ok(Response::Agents(snapshot)) = host.request(Request::Agents).await else {
                return;
            };
            for agent in snapshot.agents.iter().filter(|agent| agent.profile_pending) {
                if let Err(error) = host.publish_profile(&agent.id).await {
                    tracing::warn!(agent = %agent.id, %error, "agent profile publication failed");
                }
            }
        });
    }
}

impl Drop for AgentHost {
    /// Stops every agent this host started.
    fn drop(&mut self) {
        let (jobs, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, jobs));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Owner {
    fn snapshot(&mut self) -> Result<AgentsSnapshot, String> {
        let snapshot = self.controller.snapshot()?;
        let json = serde_json::to_value(&snapshot).map_err(|_| "Could not read agents")?;
        let mut snapshot: AgentsSnapshot =
            serde_json::from_value(json).map_err(|_| "Could not read agents")?;
        snapshot.seq = self.seq;
        Ok(snapshot)
    }

    /// Broadcasts the snapshot if it changed since the last broadcast.
    fn publish(&mut self) {
        let Ok(mut snapshot) = self.snapshot() else {
            return;
        };
        let changed = self.last.as_ref().is_none_or(|last| {
            AgentsSnapshot {
                seq: 0,
                ..last.clone()
            } != AgentsSnapshot {
                seq: 0,
                ..snapshot.clone()
            }
        });
        if changed {
            self.seq += 1;
            snapshot.seq = self.seq;
            self.last = Some(snapshot.clone());
            let _ = self.events.send((self.seq, HostEvent::Agents(snapshot)));
        }
    }
}

fn prepared_response(prepared: &NewAgent) -> Response {
    Response::Prepared {
        id: prepared.id.clone(),
        pubkey: prepared.key.pubkey().to_string(),
    }
}

/// Converts through JSON field names, the controller's own contract.
fn controller_edit(edit: &api::AgentEdit) -> Result<buzz_agent_controller::AgentEdit, String> {
    let json = serde_json::to_value(edit).map_err(|_| "Invalid agent settings")?;
    serde_json::from_value(json).map_err(|error| format!("Invalid agent settings: {error}"))
}

/// Serves an [`AgentHost`] over host-rpc.
pub struct RpcHandler(pub Arc<AgentHost>);

impl HostHandler<HostProtocol> for RpcHandler {
    fn info(&self) -> HostInfo {
        self.0.info.clone()
    }

    fn events(&self) -> EventSource<HostProtocol> {
        self.0.events.subscribe()
    }

    fn handle(
        &self,
        _: Peer<HostProtocol>,
        request: Request,
    ) -> BoxFuture<'static, Result<Response, RpcError>> {
        let host = self.0.clone();
        Box::pin(async move { host.request(request).await })
    }
}
