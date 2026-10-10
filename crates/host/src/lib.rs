//! An agent host: runs Buzz agents on this machine.
//!
//! The controller from buzz-app must have one serialized owner, so it lives on
//! a dedicated thread and every operation is a job sent to that thread. The
//! thread also watches process status, so GUIs receive changes they did not
//! cause (an agent crashing, finishing a restore) without polling.
//!
//! Waits outside the controller (OS credential prompts, Pi checks, sign-in,
//! installers) happen between jobs, as in buzz-app's native host, so Stop
//! and other requests stay answerable while a start waits.

pub mod api;
pub mod credentials;
pub mod harness;
mod import;
mod models;
mod profile;
mod setup;
mod start;
mod snapshot;

use api::{
    AgentAction, AgentDefaults, AgentsSnapshot, HostAnswer, HostAsk, HostEvent, HostProtocol,
    ImportCandidate, ImportPreview, Request, Response,
};
pub use buzz_agent_controller::Credentials;
use buzz_agent_controller::{Action, Controller, Imports, NewAgent, RuntimeBundle, Store};
use buzz_host_rpc::{EventSource, HostHandler, HostInfo, Peer, RpcError};
use futures::future::BoxFuture;
pub use import::Asker;
use start::{MentionReplay, PendingStart};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Holds the controller's store, run directories and locks, and the
    /// host's own installs of Pi and Claude Code.
    pub state_dir: PathBuf,
    /// Default working directory for new agents.
    pub workspace: PathBuf,
    pub credentials: Arc<dyn Credentials>,
    /// How often process status is checked for changes nobody requested.
    pub watch_interval: Duration,
    /// The platform data directory an earlier Buzz install keeps its agent
    /// library in (`~/.local/share` on Linux). `None` disables import.
    pub legacy_dir: Option<PathBuf>,
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
    /// The runtime the host started in, for work its own thread starts.
    runtime: tokio::runtime::Handle,
    /// One model lookup or sign-in at a time, by ticket.
    models: buzz_desktop::agent_models::ModelHost,
    /// One harness install at a time.
    setup: buzz_desktop::harness_setup::HarnessSetup,
    state_dir: PathBuf,
    legacy_dir: Option<PathBuf>,
    /// Loopback ports of sign-ins this host started, which a GUI may
    /// forward a browser callback to.
    sign_ins: std::sync::Mutex<BTreeMap<u16, std::time::Instant>>,
}

struct Owner {
    controller: Controller,
    credentials: Arc<dyn Credentials>,
    workspace: PathBuf,
    state_dir: PathBuf,
    /// The pending create: request ID and its generated key.
    creating: Option<(String, Arc<NewAgent>)>,
    imports: Imports,
    /// The import preview a commit must match.
    previewed: Option<import::Previewed>,
    legacy_dir: Option<PathBuf>,
    /// Problems reading earlier Buzz installs, shown on the Agents page.
    inventory_warnings: Vec<String>,
    /// Starts waiting on a credential, a Pi check or the owner check.
    starts: BTreeMap<String, PendingStart>,
    /// Agents that start with the host, not yet admitted, and the mention
    /// replay attached to them meanwhile.
    queued: BTreeMap<String, Option<MentionReplay>>,
    next_start: u64,
    /// Agents with an explicit Start or Stop since the host opened; a
    /// queued restore skips them.
    acted: BTreeSet<String>,
    /// Profile publications in flight, one per agent.
    publishing: BTreeSet<String>,
    log_challenges: BTreeMap<String, start::LogChallenge>,
    /// Refuses a start while old Buzz runs; replaced in tests.
    legacy_check: fn() -> Result<(), String>,
    last: Option<AgentsSnapshot>,
    seq: u64,
    events: broadcast::Sender<(u64, HostEvent)>,
    gate: Gate,
}

/// Whose agents may start here.
enum Gate {
    /// buzzd has no human key: any agent starts.
    Open,
    /// A GUI's host before it knows who signed in: nothing starts.
    Waiting,
    /// Kept agents answer only to the owner who authorized them, not to
    /// whoever signs in next.
    SignedIn(String),
}

impl AgentHost {
    /// Opens the store, starts agents marked to start with the host, and
    /// retries agent profiles left unpublished. Must run inside tokio.
    pub fn start(config: HostConfig) -> Result<Arc<Self>, String> {
        Self::start_gated(config, Gate::Open)
    }

    /// As [`Self::start`], for a host inside a GUI: no agent starts until
    /// [`Self::set_owner`] names the signed-in key, and then only agents
    /// that key authorized (buzz-app's `check_owner`).
    pub fn start_for_owner(config: HostConfig) -> Result<Arc<Self>, String> {
        Self::start_gated(config, Gate::Waiting)
    }

    fn start_gated(config: HostConfig, gate: Gate) -> Result<Arc<Self>, String> {
        buzz_agent_controller::warm_tools_path();
        std::fs::create_dir_all(&config.workspace)
            .map_err(|_| "Could not create the agent workspace")?;
        if let Err(error) = buzz_agent_controller::ensure_buzz_cli_skill(&config.workspace) {
            tracing::warn!(%error, "could not install the buzz CLI skill");
        }
        let mut store = Store::open(config.state_dir.join("agent-controller"))?;
        let mut inventory_warnings = match &config.legacy_dir {
            Some(parent) => store.migrate_legacy(parent),
            None => Vec::new(),
        };
        inventory_warnings.extend(store.clean_imported_prompts()?);
        let queued = store
            .snapshot()?
            .agents
            .into_iter()
            .filter(|agent| agent.start_on_app_launch)
            .map(|agent| (agent.id, None))
            .collect();
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
            state_dir: config.state_dir.clone(),
            creating: None,
            imports: Imports::default(),
            previewed: None,
            legacy_dir: config.legacy_dir.clone(),
            inventory_warnings,
            starts: BTreeMap::new(),
            queued,
            next_start: 0,
            acted: BTreeSet::new(),
            publishing: BTreeSet::new(),
            log_challenges: BTreeMap::new(),
            legacy_check: start::refuse_legacy,
            last: None,
            seq: 0,
            events: events.clone(),
            gate,
        };
        let interval = config.watch_interval;
        let open = matches!(owner.gate, Gate::Open);
        let thread = std::thread::Builder::new()
            .name("agent-host".into())
            .spawn(move || {
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
            runtime: tokio::runtime::Handle::current(),
            models: buzz_desktop::agent_models::ModelHost::new(Ok(config
                .state_dir
                .join("agent-controller"))),
            setup: Default::default(),
            state_dir: config.state_dir,
            legacy_dir: config.legacy_dir,
            sign_ins: Default::default(),
        });
        // A GUI's host restores once someone signs in.
        if open {
            host.restore();
        }
        host.publish_pending_profiles();
        Ok(host)
    }

    /// Names the key signed in to the GUI. The first call starts that key's
    /// agents marked to start with the host.
    pub fn set_owner(&self, viewer: String) {
        let (first, signed_in) = tokio::sync::oneshot::channel();
        let _ = self.jobs.send(Box::new(move |owner| {
            let _ = first.send(owner.sign_in(viewer));
        }));
        // Restores off the host thread, which must never hold the last
        // reference to the host.
        let host = self.weak.clone();
        self.runtime.spawn(async move {
            if signed_in.await == Ok(true)
                && let Some(host) = host.upgrade()
            {
                host.restore();
            }
        });
    }

    /// Stops every agent this host runs and waits until they have stopped
    /// (sign-out before Buzz restarts, and quitting, as buzz-app's
    /// `AgentHost::shutdown`). Their launch settings stay.
    pub fn shutdown(&self) -> Result<(), String> {
        let (sender, receiver) = mpsc::channel();
        self.jobs
            .send(Box::new(move |owner| {
                let _ = sender.send(owner.controller.shutdown());
            }))
            .map_err(|_| "Agent host shutdown could not be confirmed".to_string())?;
        receiver
            .recv_timeout(SHUTDOWN_TIMEOUT)
            .map_err(|_| "Agent host shutdown could not be confirmed".to_string())?
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

    /// As [`Self::run`], with the error as text.
    async fn run_text<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Owner) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        self.run(job).await.map_err(|error| error.to_string())
    }

    /// Serves a request that needs nothing from the GUI. Imports that need
    /// an owner attestation fail; use [`Self::request_with`].
    pub async fn request(&self, request: Request) -> Result<Response, RpcError> {
        self.request_with(request, None).await
    }

    /// Serves a request; `asker` reaches the GUI that sent it.
    pub async fn request_with(
        &self,
        request: Request,
        asker: Option<Arc<dyn Asker>>,
    ) -> Result<Response, RpcError> {
        match request {
            Request::Models { id, revision, edit } => self
                .legacy_models(id, revision, edit, asker)
                .await
                .map(Response::Models),
            Request::TestConnection { id, revision, edit } => self
                .legacy_test(id, revision, edit)
                .await
                .map(|model| Response::Tested { model }),
            Request::DisconnectDatabricks { workspace } => self
                .legacy_disconnect(workspace)
                .await
                .map(|_| Response::Disconnected),
            Request::ModelsBegin => self.models.begin().map(Response::Ticket).map_err(failed),
            Request::ModelsCancel { ticket } => self
                .models
                .cancel(ticket)
                .map(|_| Response::Done)
                .map_err(failed),
            Request::ModelsRun { ticket, request } => self
                .run_models(ticket, request, asker)
                .await
                .map(Response::Catalog),
            Request::SignInCallback { port, target } => self
                .sign_in_callback(port, target)
                .await
                .map(Response::SignInPage),
            Request::Defaults => {
                self.run(|owner| owner.defaults().map(Response::Defaults))
                    .await
            }
            Request::SaveDefaults { edit } => {
                let edit = convert::<buzz_agent_controller::AgentDefaultsEdit>(&edit)
                    .map_err(|error| failed(format!("Invalid agent defaults: {error}")))?;
                let (restarted, failures) = self
                    .save_and_restart(move |owner| owner.controller.save_defaults(edit).map(drop))
                    .await?;
                let mut defaults = self.run(|owner| owner.defaults()).await?;
                defaults.restarted = Some(restarted);
                defaults.restart_failures = Some(failures);
                Ok(Response::Defaults(defaults))
            }
            Request::ImportPreview {
                source,
                destination,
            } => {
                self.run(move |owner| owner.import_preview(source, destination))
                    .await
            }
            Request::ImportCommit { token, ids } => self.import_commit(token, ids, asker).await,
            Request::Agents => {
                Self::tools_path().await?;
                self.run(|owner| owner.snapshot().map(Response::Agents))
                    .await
            }
            Request::Harnesses => {
                Self::tools_path().await?;
                let app_data = self.state_dir.clone();
                tokio::task::spawn_blocking(move || {
                    let claude = harness::claude(&app_data).setup;
                    harness::legacy(&harness::options(&app_data), Some(&claude))
                })
                .await
                .map(Response::Harnesses)
                .map_err(|_| failed("Harness detection failed"))
            }
            Request::Action {
                id,
                action,
                replay_floor,
                owner,
            } => match action {
                AgentAction::Stop => {
                    self.run(move |host| {
                        host.starts.remove(&id);
                        host.queued.remove(&id);
                        host.acted.insert(id.clone());
                        host.controller.action(&id, Action::Stop)?;
                        host.snapshot().map(Response::Agents)
                    })
                    .await
                }
                AgentAction::Start | AgentAction::Restart => {
                    let action = if action == AgentAction::Start {
                        Action::Start
                    } else {
                        Action::Restart
                    };
                    self.start_agent(
                        id,
                        action,
                        start::StartOptions {
                            replay_floor,
                            owner,
                            ..Default::default()
                        },
                    )
                    .await
                    .map(Response::Agents)
                    .map_err(RpcError::Failed)
                }
            },
            Request::AttachMention {
                id,
                revision,
                replay_floor,
            } => {
                self.run(move |owner| {
                    owner
                        .attach_mention(&id, revision, replay_floor)
                        .map(|_| Response::Done)
                })
                .await
            }
            Request::SetStartOnLaunch { id, value } => {
                self.run(move |owner| {
                    owner.controller.set_start_on_app_launch(&id, value)?;
                    if !value {
                        owner.queued.remove(&id);
                    }
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            Request::Save { id, revision, edit } => {
                let (restarted, failures) = self
                    .save_and_restart(move |owner| owner.save(&id, revision, &edit))
                    .await?;
                let mut snapshot = self.run(|owner| owner.snapshot()).await?;
                snapshot.restarted = Some(restarted);
                snapshot.restart_failures = Some(failures);
                Ok(Response::Agents(snapshot))
            }
            Request::Delete { id, revision } => {
                self.run(move |owner| {
                    owner.starts.remove(&id);
                    owner.queued.remove(&id);
                    owner.acted.insert(id.clone());
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
                self.run(move |owner| owner.create_prepare(request_id, &relay_url, &human))
                    .await
            }
            Request::CreateCommit {
                request_id,
                edit,
                auth,
            } => self.create_commit(request_id, edit, auth).await,
            Request::RestoreMemory { id, owner, entries } => {
                self.restore_memory(id, owner, entries).await
            }
            Request::PublishProfile { id } => {
                self.publish_profile(&id).await.map_err(RpcError::Failed)?;
                self.run(|owner| owner.snapshot().map(Response::Agents))
                    .await
            }
            Request::UseHere { id, resolution } => {
                let resolution = convert::<buzz_agent_controller::CommunityResolution>(&resolution)
                    .map_err(|_| failed("Invalid community resolution"))?;
                self.run(move |owner| {
                    owner.controller.use_here(&id, resolution)?;
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            Request::LocalCloneSettings { id } => {
                self.run(move |owner| {
                    let settings = owner.controller.local_clone_settings(&id)?;
                    Ok(Response::CloneSettings(api::CloneSettings {
                        name: settings.name,
                        system_prompt: settings.system_prompt,
                    }))
                })
                .await
            }
            Request::CloneSettings { source, pubkey } => {
                let parent = self.legacy_parent()?;
                let settings = tokio::task::spawn_blocking(move || {
                    Imports::clone_settings(import::legacy(source), parent, &pubkey)
                })
                .await
                .map_err(|_| failed("Could not read clone settings"))?
                .map_err(RpcError::Failed)?;
                Ok(Response::CloneSettings(api::CloneSettings {
                    name: settings.name,
                    system_prompt: settings.system_prompt,
                }))
            }
            Request::Library => {
                let path = self
                    .legacy_parent()?
                    .join(buzz_agent_controller::LegacySource::Installed.app_directory())
                    .join("agents/managed-agents.json");
                // No earlier install is an empty library, not a failed read.
                if matches!(std::fs::symlink_metadata(&path), Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound)
                {
                    return Ok(Response::Library(Default::default()));
                }
                let library = tokio::task::spawn_blocking(move || {
                    buzz_desktop::relay::agent::read_agent_library(path)
                })
                .await
                .map_err(|_| failed("Current Buzz library unavailable"))?
                .map_err(RpcError::Failed)?;
                serde_json::from_value(library)
                    .map(Response::Library)
                    .map_err(|_| failed("Current Buzz library unavailable"))
            }
            Request::ClaudeAuth => Ok(Response::ClaudeAuth(self.claude_auth().await)),
            Request::InstallHarness { kind } => self
                .install(kind)
                .await
                .map(Response::Installed)
                .map_err(RpcError::Failed),
            Request::LogChallenge {
                id,
                pubkey,
                relay_url,
            } => {
                self.run(move |owner| {
                    owner
                        .log_challenge(id, pubkey, relay_url)
                        .map(Response::Nonce)
                })
                .await
            }
            Request::ReadLog {
                id,
                pubkey,
                relay_url,
                nonce,
                signature,
            } => {
                self.run(move |owner| {
                    owner
                        .read_log(&id, &pubkey, &relay_url, &nonce, &signature)
                        .map(Response::Log)
                })
                .await
            }
        }
    }

    fn legacy_parent(&self) -> Result<PathBuf, RpcError> {
        self.legacy_dir
            .clone()
            .ok_or_else(|| failed("This host has no data directory to import from"))
    }

    /// Login-shell PATH discovery blocks for up to ~15 s, once.
    async fn tools_path() -> Result<(), RpcError> {
        tokio::task::spawn_blocking(buzz_agent_controller::prepare_tools_path)
            .await
            .map_err(|_| failed("Could not find installed tools"))
    }

    async fn create_commit(
        &self,
        request_id: String,
        edit: api::AgentEdit,
        auth: String,
    ) -> Result<Response, RpcError> {
        let (prepared, credentials, edit, auth) = self
            .run(move |owner| {
                let prepared = owner
                    .creating
                    .as_ref()
                    .filter(|(id, _)| *id == request_id)
                    .map(|(_, prepared)| prepared.clone())
                    .ok_or("Create request expired; reopen Add agent")?;
                let edit = owner.draft(&edit)?;
                prepared.validate(edit.clone(), &auth)?;
                Ok((prepared, owner.credentials.clone(), edit, auth))
            })
            .await?;
        // The key write may wait on the OS keyring; never on the host thread.
        let saved = prepared.clone();
        tokio::task::spawn_blocking(move || {
            credentials.retry();
            saved.save_key(credentials.as_ref())
        })
        .await
        .map_err(|_| failed("Native credential operation failed"))?
        .map_err(RpcError::Failed)?;
        let id = prepared.id.clone();
        let snapshot = self
            .run(move |owner| {
                if !owner
                    .creating
                    .as_ref()
                    .is_some_and(|(_, pending)| Arc::ptr_eq(pending, &prepared))
                {
                    return Err("Create request was replaced".into());
                }
                owner.controller.create(&prepared, edit, &auth)?;
                owner.creating = None;
                owner.snapshot()
            })
            .await?;
        self.publish_profile_later(id);
        Ok(Response::Agents(snapshot))
    }
}

impl AgentHost {
    /// Publishes the agent's pending profile and records it, unless the
    /// agent's settings changed meanwhile. One publication per agent runs
    /// at a time.
    async fn publish_profile(&self, id: &str) -> Result<(), String> {
        let target = id.to_string();
        let (profile, credentials) = self
            .run_text(move |owner| {
                let profile = owner.controller.creation_profile(&target)?;
                if !owner.publishing.insert(target) {
                    return Err(
                        "Profile publication is already in progress; refresh status before retrying"
                            .into(),
                    );
                }
                Ok((profile, owner.credentials.clone()))
            })
            .await?;
        let result = self.publish_acquired(id, profile, credentials).await;
        let target = id.to_string();
        let _ = self
            .run(move |owner| {
                owner.publishing.remove(&target);
                Ok(())
            })
            .await;
        result
    }

    async fn publish_acquired(
        &self,
        id: &str,
        profile: buzz_agent_controller::CreationProfile,
        credentials: Arc<dyn Credentials>,
    ) -> Result<(), String> {
        let (profile, key) = tokio::task::spawn_blocking(move || {
            credentials
                .read(&profile.credential_id, &profile.pubkey)
                .map(|key| (profile, key))
        })
        .await
        .map_err(|_| "Native credential operation failed")??;
        let key = key.ok_or("The agent's key is unavailable")?;
        profile::publish(&self.http, &profile, &key).await?;
        let revision = profile.revision;
        let id = id.to_string();
        self.run_text(move |owner| {
            let current = owner.controller.creation_profile(&id)?;
            if current.revision != revision {
                return Err("Saved profile changed; retry publication".into());
            }
            owner.controller.profile_published(&id, revision)
        })
        .await
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
            let Ok(snapshot) = host.run(|owner| owner.snapshot()).await else {
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

fn failed(message: impl Into<String>) -> RpcError {
    RpcError::Failed(message.into())
}

impl AgentHost {
    /// Imports from the previewed source. Candidates without an owner
    /// attestation get one signed by the GUI, then import from a staged copy
    /// of the source that carries it.
    async fn import_commit(
        &self,
        token: String,
        ids: Vec<String>,
        asker: Option<Arc<dyn Asker>>,
    ) -> Result<Response, RpcError> {
        let selected = ids.clone();
        let pending = self
            .run(move |owner| {
                let previewed = owner
                    .previewed
                    .as_ref()
                    .filter(|previewed| previewed.token == token && !token.is_empty())
                    .ok_or("Import preview expired; choose the source again")?;
                Ok(selected
                    .iter()
                    .filter_map(|id| previewed.unattested.get(id).cloned())
                    .collect::<Vec<_>>())
            })
            .await?;
        let mut attestations = BTreeMap::new();
        if !pending.is_empty() {
            let asker = asker.ok_or_else(|| {
                failed("Importing these agents needs your owner attestation; import from the app")
            })?;
            for pubkey in pending {
                let answer = asker
                    .ask(HostAsk::AttestAgent {
                        agent_pubkey: pubkey.clone(),
                    })
                    .await?;
                let HostAnswer::Attestation(tag) = answer else {
                    return Err(failed("The app did not attest the agent"));
                };
                import::verify_attestation(&tag, &pubkey).map_err(RpcError::Failed)?;
                attestations.insert(pubkey, tag);
            }
        }
        let (prepared, credentials, staging) = self
            .run(move |owner| {
                let previewed = owner
                    .previewed
                    .take()
                    .ok_or("Import preview expired; choose the source again")?;
                let staging = if attestations.is_empty() {
                    let prepared = owner.controller.prepare_import(
                        &mut owner.imports,
                        &previewed.token,
                        &ids,
                    )?;
                    owner.imports.discard();
                    return Ok((prepared, owner.credentials.clone(), None));
                } else {
                    let root = owner.state_dir.join("import");
                    std::fs::create_dir_all(&root).map_err(|_| "Could not stage the import")?;
                    tempfile::TempDir::new_in(&root).map_err(|_| "Could not stage the import")?
                };
                let source = import::legacy(previewed.source);
                import::stage(
                    &previewed.parent.join(source.app_directory()),
                    &staging.path().join(source.app_directory()),
                    &previewed.digest,
                    &attestations,
                )?;
                owner.imports.discard();
                let mut staged = Imports::default();
                let preview = staged.preview(
                    source,
                    staging.path().to_path_buf(),
                    owner.workspace.clone(),
                    &previewed.destination,
                )?;
                let prepared =
                    owner
                        .controller
                        .prepare_import(&mut staged, &preview.token, &ids)?;
                Ok((prepared, owner.credentials.clone(), Some(staging)))
            })
            .await?;
        // Key reads and writes may wait on the OS keyring.
        let imported = tokio::task::spawn_blocking(move || {
            credentials.retry();
            prepared.acquire(credentials.as_ref())
        })
        .await
        .map_err(|_| failed("Import key handling failed"))?
        .map_err(RpcError::Failed)?;
        let response = self
            .run(move |owner| {
                owner.controller.commit_import(imported)?;
                owner.snapshot().map(Response::Agents)
            })
            .await;
        // The commit re-reads the staged copy; remove it only afterwards.
        drop(staging);
        response
    }
}

/// How long [`AgentHost::shutdown`] waits for the agents to stop.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

impl Drop for AgentHost {
    /// Stops every agent this host started, and any installer or sign-in.
    fn drop(&mut self) {
        self.models.shutdown();
        self.setup.shutdown();
        let (jobs, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.jobs, jobs));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Owner {
    /// Who an agent start must answer to: `None` for buzzd, which has no
    /// human key, and an error before anyone signed in to the GUI.
    fn gate_owner(&self) -> Result<Option<String>, String> {
        match &self.gate {
            Gate::Open => Ok(None),
            Gate::Waiting => Err("Set up your identity first".into()),
            Gate::SignedIn(viewer) => Ok(Some(viewer.clone())),
        }
    }

    /// Names the signed-in key. True the first time, when the agents
    /// marked to start with the host should start (refusing other owners'
    /// agents, as `Controller::restore` does).
    fn sign_in(&mut self, viewer: String) -> bool {
        let first = matches!(self.gate, Gate::Waiting);
        self.gate = Gate::SignedIn(viewer);
        first
    }

    /// Saves an agent. The controller keeps an agent's own effort until its
    /// harness kind changes, and has no other way to clear it; so a clear
    /// saves twice more, through a stand-in harness of another kind and
    /// back, with nothing else changed. The edit itself is saved first, so
    /// a draft the controller refuses changes nothing.
    fn save(&mut self, id: &str, revision: u64, edit: &api::AgentEdit) -> Result<(), String> {
        self.controller.save(id, revision, controller_edit(edit)?)?;
        if edit.effort != Some(None) {
            return Ok(());
        }
        let unchanged = api::AgentEdit {
            picture: None,
            session_policy: None,
            environment: BTreeMap::new(),
            effort: None,
            ..edit.clone()
        };
        let detour = api::AgentEdit {
            harness: api::HarnessEdit {
                command: effort_detour(&edit.harness.command).into(),
                ..unchanged.harness.clone()
            },
            ..unchanged.clone()
        };
        let revision = self.revision(id)?;
        self.controller
            .save(id, revision, controller_edit(&detour)?)?;
        let revision = self.revision(id)?;
        self.controller
            .save(id, revision, controller_edit(&unchanged)?)?;
        Ok(())
    }

    fn revision(&mut self, id: &str) -> Result<u64, String> {
        self.controller
            .snapshot()?
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.revision)
            .ok_or_else(|| "Agent no longer exists".into())
    }

    /// A form draft as the controller takes it; an empty workspace means
    /// the host's default, as on create.
    fn draft(&self, edit: &api::AgentEdit) -> Result<buzz_agent_controller::AgentEdit, String> {
        let mut edit = controller_edit(edit)?;
        if edit.workspace.is_empty() {
            edit.workspace = self.workspace.to_string_lossy().into_owned();
        }
        Ok(edit)
    }

    fn defaults(&mut self) -> Result<AgentDefaults, String> {
        let snapshot = self.controller.snapshot()?;
        let mut defaults: AgentDefaults =
            convert(&snapshot.default_settings).map_err(|_| "Could not read agent defaults")?;
        defaults.build = convert(&buzz_agent_controller::build_defaults())
            .map_err(|_| "Could not read agent defaults")?;
        Ok(defaults)
    }

    /// Generates the new agent's key, or returns the one an earlier
    /// attempt with the same request saved.
    fn create_prepare(
        &mut self,
        request_id: String,
        relay_url: &str,
        human: &str,
    ) -> Result<Response, String> {
        if let Some(agent) = self
            .controller
            .created_request(&request_id, relay_url, human)?
        {
            return Ok(Response::Prepared {
                id: agent.id,
                pubkey: agent.pubkey,
                saved: true,
            });
        }
        if let Some((id, prepared)) = &self.creating
            && *id == request_id
        {
            if !prepared.matches(relay_url, human)? {
                return Err("Create destination or owner changed".into());
            }
            return Ok(prepared_response(prepared));
        }
        let prepared = Arc::new(NewAgent::prepare(relay_url, human)?);
        let response = prepared_response(&prepared);
        self.creating = Some((request_id, prepared));
        Ok(response)
    }

    fn import_preview(
        &mut self,
        source: api::ImportSource,
        destination: String,
    ) -> Result<Response, String> {
        self.previewed = None;
        let parent = self
            .legacy_dir
            .clone()
            .ok_or("This host has no data directory to import from")?;
        let legacy = import::legacy(source);
        let preview =
            self.imports
                .preview(legacy, parent.clone(), self.workspace.clone(), &destination)?;
        let (digest, unattested) = import::scan(&parent.join(legacy.app_directory()))?;
        let saved: std::collections::HashSet<String> = self
            .controller
            .snapshot()?
            .agents
            .into_iter()
            .map(|agent| agent.pubkey)
            .collect();
        let mut view: ImportPreview = convert(&preview).map_err(|_| "Could not read the import")?;
        for candidate in &mut view.candidates {
            candidate.imported = saved.contains(&candidate.pubkey);
            candidate.needs_attestation = unattested.contains(&candidate.pubkey);
        }
        self.previewed = Some(import::Previewed {
            token: view.token.clone(),
            source,
            destination,
            parent,
            digest,
            unattested: view
                .candidates
                .iter()
                .filter(|candidate| candidate.needs_attestation)
                .map(|candidate: &ImportCandidate| (candidate.id.clone(), candidate.pubkey.clone()))
                .collect(),
        });
        Ok(Response::ImportPreview(view))
    }

    fn snapshot(&mut self) -> Result<AgentsSnapshot, String> {
        let mut data = self.controller.snapshot()?;
        for agent in &mut data.agents {
            if let Some(pending) = self.starts.get(&agent.id) {
                agent.status = pending.status;
                agent.error = None;
            } else if self.queued.contains_key(&agent.id) {
                agent.status = buzz_agent_controller::ProcessStatus::Waiting;
                agent.error = None;
            }
        }
        let mut snapshot: AgentsSnapshot = convert(&data).map_err(|_| "Could not read agents")?;
        let build = buzz_agent_controller::build_defaults();
        snapshot.seq = self.seq;
        snapshot.default_workspace = self.workspace.to_string_lossy().into_owned();
        snapshot.inventory_warnings = self.inventory_warnings.clone();
        snapshot.import_available = Some(self.legacy_dir.is_some());
        snapshot.create_available = Some(true);
        snapshot.avatar_editing_available = true;
        snapshot.local_inventory_actions = true;
        snapshot.harness_options = Some(harness::options(&self.state_dir));
        snapshot.claude_setup = Some(harness::claude(&self.state_dir).setup);
        snapshot.databricks_defaults = Some(api::DatabricksSettings {
            host: build.host.clone(),
            filter: build.filter.clone(),
        });
        snapshot.agent_defaults = Some(api::BuildDefaults {
            provider: build.provider,
            model: build.model,
            host: build.host,
            filter: build.filter,
            owner_only: build.owner_only,
        });
        snapshot.default_settings = convert(&data.default_settings).ok();
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

/// A harness command of another kind than `command` that accepts the same
/// environment: Pi and Goose take behavior overrides, the rest do not.
fn effort_detour(command: &str) -> &'static str {
    match std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim_end_matches(".exe"))
    {
        Some("goose" | "goose-acp") => "buzz-pi-acp",
        Some("buzz-pi-acp") => "goose",
        _ => "buzz-effort-reset",
    }
}

fn prepared_response(prepared: &NewAgent) -> Response {
    Response::Prepared {
        id: prepared.id.clone(),
        pubkey: prepared.key.pubkey().to_string(),
        saved: false,
    }
}

/// Converts through JSON field names, the controller's own contract.
fn convert<T: serde::de::DeserializeOwned>(value: &impl serde::Serialize) -> Result<T, String> {
    let json = serde_json::to_value(value).map_err(|error| error.to_string())?;
    serde_json::from_value(json).map_err(|error| error.to_string())
}

fn controller_edit(edit: &api::AgentEdit) -> Result<buzz_agent_controller::AgentEdit, String> {
    convert(edit).map_err(|error| format!("Invalid agent settings: {error}"))
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
        peer: Peer<HostProtocol>,
        request: Request,
    ) -> BoxFuture<'static, Result<Response, RpcError>> {
        let host = self.0.clone();
        Box::pin(async move { host.request_with(request, Some(Arc::new(peer))).await })
    }
}
