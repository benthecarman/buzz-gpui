//! An agent host: runs Buzz agents on this machine.
//!
//! The controller from buzz-app must have one serialized owner, so it lives on
//! a dedicated thread and every operation is a job sent to that thread. The
//! thread also watches process status, so GUIs receive changes they did not
//! cause (an agent crashing, finishing a restore) without polling.

pub mod api;
pub mod credentials;
pub mod harness;
mod import;
mod models;
mod profile;

use api::{
    AgentAction, AgentDefaults, AgentsSnapshot, HostAnswer, HostAsk, HostEvent, HostProtocol,
    ImportCandidate, ImportPreview, Request, Response,
};
pub use buzz_agent_controller::Credentials;
use buzz_agent_controller::{Action, Controller, Imports, NewAgent, RuntimeBundle, Store};
use buzz_host_rpc::{EventSource, HostHandler, HostInfo, Peer, RpcError};
use futures::future::BoxFuture;
pub use import::Asker;
use std::collections::BTreeMap;
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
}

struct Owner {
    controller: Controller,
    credentials: Arc<dyn Credentials>,
    workspace: PathBuf,
    /// The pending create: request ID and its generated key.
    creating: Option<(String, Arc<NewAgent>)>,
    imports: Imports,
    /// The import preview a commit must match.
    previewed: Option<import::Previewed>,
    legacy_dir: Option<PathBuf>,
    state_dir: PathBuf,
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
            imports: Imports::default(),
            previewed: None,
            legacy_dir: config.legacy_dir,
            state_dir: config.state_dir,
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
            Request::Models { id, revision, edit } => {
                self.models(id, revision, edit).await.map(Response::Models)
            }
            Request::Defaults => {
                self.run(|owner| owner.defaults().map(Response::Defaults))
                    .await
            }
            Request::SaveDefaults { edit } => {
                self.run(move |owner| {
                    let json = serde_json::to_value(&edit).map_err(|_| "Invalid agent defaults")?;
                    let edit: buzz_agent_controller::AgentDefaultsEdit =
                        serde_json::from_value(json)
                            .map_err(|error| format!("Invalid agent defaults: {error}"))?;
                    // Agents that inherit a changed default restart with it.
                    let (restarted, failures) = owner
                        .save_and_restart(|owner| owner.controller.save_defaults(edit).map(drop))?;
                    let mut defaults = owner.defaults()?;
                    defaults.restarted = Some(restarted);
                    defaults.restart_failures = Some(failures);
                    Ok(Response::Defaults(defaults))
                })
                .await
            }
            Request::TestConnection { id, revision, edit } => self
                .test_connection(id, revision, edit)
                .await
                .map(|model| Response::Tested { model }),
            Request::DisconnectDatabricks { workspace } => {
                self.run(move |owner| {
                    let workspace = match workspace.as_str() {
                        "" => owner
                            .controller
                            .inherited_workspace()?
                            .ok_or("Agent defaults no longer set a Databricks workspace")?,
                        _ => workspace,
                    };
                    owner.controller.disconnect(&workspace)?;
                    Ok(Response::Disconnected)
                })
                .await
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
                    let (restarted, failures) =
                        owner.save_and_restart(|owner| owner.save(&id, revision, &edit))?;
                    let mut snapshot = owner.snapshot()?;
                    snapshot.restarted = Some(restarted);
                    snapshot.restart_failures = Some(failures);
                    Ok(Response::Agents(snapshot))
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

fn failed(message: impl Into<String>) -> RpcError {
    RpcError::Failed(message.into())
}

impl AgentHost {
    /// Model contexts come from the controller on its thread; the harness
    /// then runs on tokio, so a slow lookup never blocks other requests.
    async fn models(
        &self,
        id: Option<String>,
        revision: u64,
        edit: api::AgentEdit,
    ) -> Result<api::ModelCatalog, RpcError> {
        match models::lookup(&edit.harness.command) {
            models::Lookup::Databricks => Err(failed(
                "Buzz Agent lists models only for Databricks, which needs a sign-in this \
                 host cannot run yet. Enter a model ID.",
            )),
            models::Lookup::None => Err(failed(
                "This harness does not list its models. Enter a model ID.",
            )),
            models::Lookup::Goose => {
                let context = self.goose_context(id, revision, edit).await?;
                models::goose(context).await.map_err(RpcError::Failed)
            }
            models::Lookup::Pi => {
                let context = self.pi_context(id, revision, edit).await?;
                models::pi(context).await.map_err(RpcError::Failed)
            }
        }
    }

    /// Sends one prompt through the draft's Pi or Goose setup. Returns the
    /// model that answered, unless a saved override chose it: override
    /// values never leave the host.
    async fn test_connection(
        &self,
        id: Option<String>,
        revision: u64,
        edit: api::AgentEdit,
    ) -> Result<Option<String>, RpcError> {
        match models::lookup(&edit.harness.command) {
            models::Lookup::Goose => {
                let context = self.goose_context(id, revision, edit).await?;
                let overridden =
                    context.model_overridden || context.environment.contains_key("GOOSE_PROVIDER");
                let model = models::goose_test(context)
                    .await
                    .map_err(RpcError::Failed)?;
                Ok((!overridden).then_some(model))
            }
            models::Lookup::Pi => {
                let provider = edit.harness.provider.clone();
                let model = edit.harness.model.clone();
                let context = self.pi_context(id, revision, edit).await?;
                models::pi_test(context, &provider, &model)
                    .await
                    .map(Some)
                    .map_err(RpcError::Failed)
            }
            _ => Err(failed(
                "Connection tests are only available for Pi and Goose",
            )),
        }
    }

    /// Login-shell PATH discovery blocks for up to ~15 s, once.
    async fn tools_path() -> Result<(), RpcError> {
        tokio::task::spawn_blocking(buzz_agent_controller::prepare_tools_path)
            .await
            .map_err(|_| failed("Could not find installed tools"))
    }

    async fn goose_context(
        &self,
        id: Option<String>,
        revision: u64,
        edit: api::AgentEdit,
    ) -> Result<buzz_agent_controller::GooseModelContext, RpcError> {
        Self::tools_path().await?;
        self.run(move |owner| {
            let edit = owner.draft(&edit)?;
            match id {
                Some(id) => owner.controller.goose_model_context(&id, revision, edit),
                None => {
                    let edit = owner.controller.effective_draft(edit)?;
                    owner.controller.draft_goose_model_context(edit)
                }
            }
        })
        .await
    }

    async fn pi_context(
        &self,
        id: Option<String>,
        revision: u64,
        edit: api::AgentEdit,
    ) -> Result<buzz_agent_controller::pi::PiContext, RpcError> {
        Self::tools_path().await?;
        self.run(move |owner| {
            let edit = owner.draft(&edit)?;
            match id {
                Some(id) => owner.controller.pi_model_context(&id, revision, edit),
                None => Controller::draft_pi_model_context(owner.controller.effective_draft(edit)?),
            }
        })
        .await
    }

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
                let HostAnswer::Attestation(tag) = answer;
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

impl AgentHost {
    /// Stops every agent now and waits for the controller to confirm it,
    /// as quitting buzz-app does (`AgentHost::shutdown`). Blocks the
    /// caller; the host keeps serving afterwards.
    pub fn shutdown(&self) -> Result<(), String> {
        let (done, confirmed) = mpsc::channel();
        self.jobs
            .send(Box::new(move |owner: &mut Owner| {
                let _ = done.send(
                    owner
                        .controller
                        .shutdown()
                        .map_err(|error| error.to_string()),
                );
            }))
            .map_err(|_| "Agent host shutdown could not be confirmed".to_string())?;
        confirmed
            .recv_timeout(SHUTDOWN_TIMEOUT)
            .map_err(|_| "Agent host shutdown could not be confirmed".to_string())?
    }
}

/// How long [`AgentHost::shutdown`] waits for the agents to stop.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

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

    /// The agent is running, and runs (`Some(true)`) or does not run yet
    /// (`Some(false)`) its saved settings. `None` when it is not running.
    fn current(&mut self, id: &str) -> Result<Option<bool>, String> {
        Ok(self
            .controller
            .snapshot()?
            .agents
            .into_iter()
            .find(|agent| {
                agent.id == id
                    && agent.enabled
                    && agent.status == buzz_agent_controller::ProcessStatus::Running
            })
            .map(|agent| agent.restart_diff.is_empty()))
    }

    /// Runs a save, then restarts running agents whose launch settings it
    /// changed (buzz-app's `save_and_restart`). Stopped agents stay
    /// stopped. Returns how many restarted and how many did not.
    fn save_and_restart(
        &mut self,
        save: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(u32, u32), String> {
        let before = self.controller.running_settings()?;
        save(self)?;
        let after = self.controller.running_settings()?;
        let (mut restarted, mut failures) = (0, 0);
        for id in changed_running(&before, &after) {
            if self.current(&id)? != Some(false) {
                continue;
            }
            let outcome = self.controller.action(&id, Action::Restart);
            match (outcome, self.current(&id)) {
                (Ok(_), Ok(Some(true))) => restarted += 1,
                (outcome, _) => {
                    if let Err(error) = outcome {
                        tracing::warn!(agent = %id, %error, "agent did not restart after a save");
                    }
                    failures += 1;
                }
            }
        }
        Ok((restarted, failures))
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
        let json = serde_json::to_value(&snapshot.default_settings)
            .map_err(|_| "Could not read agent defaults")?;
        serde_json::from_value(json).map_err(|_| "Could not read agent defaults".into())
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
        let json = serde_json::to_value(&preview).map_err(|_| "Could not read the import")?;
        let mut view: ImportPreview =
            serde_json::from_value(json).map_err(|_| "Could not read the import")?;
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
        let snapshot = self.controller.snapshot()?;
        let json = serde_json::to_value(&snapshot).map_err(|_| "Could not read agents")?;
        let mut snapshot: AgentsSnapshot =
            serde_json::from_value(json).map_err(|_| "Could not read agents")?;
        snapshot.seq = self.seq;
        snapshot.default_workspace = self.workspace.to_string_lossy().into_owned();
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

/// Agents running both before and after a save whose launch settings
/// differ.
fn changed_running(
    before: &BTreeMap<String, serde_json::Value>,
    after: &BTreeMap<String, serde_json::Value>,
) -> Vec<String> {
    after
        .iter()
        .filter(|(id, settings)| before.get(*id).is_some_and(|old| old != *settings))
        .map(|(id, _)| id.clone())
        .collect()
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
        peer: Peer<HostProtocol>,
        request: Request,
    ) -> BoxFuture<'static, Result<Response, RpcError>> {
        let host = self.0.clone();
        Box::pin(async move { host.request_with(request, Some(Arc::new(peer))).await })
    }
}
