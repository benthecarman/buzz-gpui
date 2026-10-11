//! The host protocol: what a GUI can ask an agent host, and what the host
//! pushes back.
//!
//! These types are ours, not the controller's. The controller's views are
//! converted through their JSON field names, so a host and a GUI built from
//! different controller revisions still understand each other; fields one
//! side does not know default.

use buzz_host_rpc::Protocol;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod app_agent;
pub use app_agent::*;

pub use crate::terminal::{TerminalContext, TerminalOutput};

pub struct HostProtocol;

impl Protocol for HostProtocol {
    type ClientRequest = Request;
    type ClientResponse = Response;
    type HostRequest = HostAsk;
    type HostResponse = HostAnswer;
    type Event = HostEvent;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Agents,
    Harnesses,
    Action {
        id: String,
        action: AgentAction,
        /// Start or Restart replays channel messages from this Unix time,
        /// for a mention sent while the agent was stopped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay_floor: Option<u64>,
        /// The human asking. A Start or Restart is refused when the agent's
        /// owner attestation names someone else; older GUIs send none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
    },
    SetStartOnLaunch {
        id: String,
        value: bool,
    },
    Save {
        id: String,
        revision: u64,
        edit: AgentEdit,
    },
    Delete {
        id: String,
        revision: u64,
    },
    /// Generates the new agent's key on the host. The GUI then signs the
    /// owner attestation for the returned pubkey with the human key and
    /// sends it in [`Request::CreateCommit`]. Retrying with the same
    /// `request_id` returns the same key.
    CreatePrepare {
        request_id: String,
        relay_url: String,
        owner: String,
    },
    CreateCommit {
        request_id: String,
        edit: AgentEdit,
        /// The NIP-OA `auth` tag, JSON encoded.
        auth: String,
    },
    /// Publishes the agent's pending profile. Also done automatically after
    /// a create and when the host starts; this is the manual retry.
    PublishProfile {
        id: String,
    },
    /// Lists the models the draft's harness and provider offer. With `id`,
    /// the draft is resolved against that saved agent at `revision`, so its
    /// saved (write-only) environment applies; without, against the host's
    /// agent defaults.
    Models {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        revision: u64,
        edit: AgentEdit,
    },
    /// The host's agent defaults: what agents use for choices they leave
    /// empty.
    Defaults,
    SaveDefaults {
        edit: DefaultsEdit,
    },
    /// Lists the agents an earlier Buzz install on the host could import
    /// into `destination` (a relay URL). The returned token names this
    /// preview in [`Request::ImportCommit`].
    ImportPreview {
        source: ImportSource,
        destination: String,
    },
    /// Imports the chosen candidates, stopped. Candidates without an owner
    /// attestation are attested by the GUI through [`HostAsk::AttestAgent`].
    ImportCommit {
        token: String,
        ids: Vec<String>,
    },
    /// Sends one short prompt through the draft's harness, provider and
    /// model (Pi and Goose only). `id` and `revision` resolve the draft as
    /// in [`Request::Models`].
    TestConnection {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        revision: u64,
        edit: AgentEdit,
    },
    /// Removes this host's cached Databricks sign-in for `workspace`, an
    /// HTTPS origin. Empty means the workspace agent defaults set.
    DisconnectDatabricks {
        workspace: String,
    },
    /// Installs Pi or Claude Code with a pinned Node.js into this host's own
    /// directory, never user-global, then restarts agents that were waiting
    /// for Pi. Codex installs only its ACP adapter; the CLI and its sign-in
    /// stay the user's. One install runs at a time. Hosts before Codex
    /// cannot read a Codex install and answer it as unsupported.
    InstallHarness {
        kind: HarnessKind,
    },
    /// Asks Claude Code whether it is signed in. Never part of a snapshot.
    ClaudeAuth,
    /// Asks the Codex CLI whether it is signed in. Never part of a snapshot.
    CodexAuth,
    /// Adds a mention's replay floor to a start that is already waiting or
    /// starting. Never starts an agent itself.
    AttachMention {
        id: String,
        revision: u64,
        replay_floor: u64,
    },
    /// Finishes an older incomplete import in a community, with the
    /// owner's signed [`CommunityResolution`]. The agent stays stopped.
    UseHere {
        id: String,
        resolution: CommunityResolution,
    },
    /// The name and instructions a new agent cloned from a saved one starts
    /// with.
    LocalCloneSettings {
        id: String,
    },
    /// As [`Request::LocalCloneSettings`], for an identity in an earlier
    /// Buzz install's library.
    CloneSettings {
        source: ImportSource,
        pubkey: String,
    },
    /// The agent library of the earlier Buzz install on the host.
    Library,
    /// Reserves the host's one model lookup. Answered with
    /// [`Response::Ticket`].
    ModelsBegin,
    /// Cancels the ticket's lookup, or releases an unused ticket.
    ModelsCancel {
        ticket: u64,
    },
    /// Lists, tests, signs in to or disconnects a draft's models under a
    /// ticket from [`Request::ModelsBegin`]. A Databricks sign-in asks the
    /// GUI to open the browser with [`HostAsk::OpenUrl`].
    ModelsRun {
        ticket: u64,
        request: ModelRequest,
    },
    /// Starts reading an agent's harness log. The GUI signs the returned
    /// nonce with the human key and sends it in [`Request::ReadLog`].
    LogChallenge {
        id: String,
        pubkey: String,
        relay_url: String,
    },
    ReadLog {
        id: String,
        pubkey: String,
        relay_url: String,
        nonce: String,
        signature: String,
    },
    /// The GUI's browser reached the loopback address of a sign-in the
    /// host started. The host replays the request on its own loopback
    /// listener at `port` and returns the page to show.
    SignInCallback {
        port: u16,
        target: String,
    },
    // snapshots
    /// Restores snapshot memory under agent `id`, which must be stopped and
    /// created by this host with `owner`'s attestation. Each entry is
    /// encrypted to `owner`, signed with the agent's key and published; the
    /// GUI confirms them by reading the memories back with the owner key.
    /// An empty `id` only asks whether this host can restore memory.
    RestoreMemory {
        id: String,
        owner: String,
        entries: Vec<MemoryRestore>,
    },
    // Agents2 custody
    /// An Agents2 agent's key, held by this host: see [`AppAgentRequest`].
    AppAgent(AppAgentRequest),
    // teams
    /// Portable snapshots of the saved agents `members` (pubkeys) in
    /// `community` (an HTTPS or WSS origin), as a team named by `team`.
    /// Answers [`Response::TeamSnapshot`].
    TeamCapture {
        team: TeamMeta,
        members: Vec<String>,
        community: String,
    },
    /// Validates a team snapshot file's JSON and answers it normalized as
    /// [`Response::TeamSnapshot`]. Nothing is created.
    TeamPreview {
        content: String,
    },
    /// Applies a saved team's instructions to one member before a deploy,
    /// after reconciling the host's team bindings with `teams`, the
    /// owner's current team heads in `community`. Sent by GUIs from before
    /// team text records; newer ones send [`Request::TeamSync`].
    TeamInstructions {
        id: String,
        revision: u64,
        instructions: String,
        team: String,
        community: String,
        owner: String,
        teams: BTreeMap<String, TeamCatalogEntry>,
    },
    /// buzz-app's `agent_control_team_sync`, after a team save or delete
    /// and once teams load: copies each team's shared instructions
    /// (`texts`, by team ID) to its members in `teams`, the owner's current
    /// team heads in `community`, and clears them from agents no team with
    /// text lists any longer. A team missing from `texts` could not be read
    /// and keeps its members' text. Never restarts an agent. Answered with
    /// [`Response::Agents`].
    TeamSync {
        community: String,
        owner: String,
        teams: BTreeMap<String, TeamCatalogEntry>,
        texts: BTreeMap<String, String>,
    },
    /// [`Request::CreatePrepare`] for one member of a team import. A
    /// request ID that already created an agent answers that agent with
    /// `saved`, so an interrupted import resumes without new keys.
    TeamMemberPrepare {
        request_id: String,
        relay_url: String,
        owner: String,
    },
    /// [`Request::CreateCommit`] for a team member: saves the agent bound
    /// to the imported team with its portable settings.
    TeamMemberCommit {
        request_id: String,
        edit: AgentEdit,
        auth: String,
        bundle: Box<BundleMember>,
    },
    // claude code
    /// Starts process `process`, one that agent type `owner` declares (see
    /// `processes.rs`), with `args` after its declared arguments. `handle`
    /// is chosen by the GUI, unique on its connection, so output can arrive
    /// before this request's response. Output and the exit code come back
    /// as [`HostAsk::ProcessOutput`] on this connection, which owns the
    /// process: it is killed when the connection closes.
    ProcessSpawn {
        handle: u64,
        owner: String,
        process: String,
        args: Vec<String>,
        /// Absolute, or under home written `~/…`; created if missing.
        #[serde(default)]
        cwd: Option<String>,
        /// Added to the host's environment; `None` removes a variable.
        #[serde(default)]
        env: BTreeMap<String, Option<String>>,
    },
    /// Writes `data` to the process's stdin; `close` then ends its input.
    ProcessWrite {
        handle: u64,
        data: String,
        close: bool,
    },
    /// Asks the process to stop, then kills it and its descendants. Its
    /// exit follows as output.
    ProcessKill {
        handle: u64,
    },
    /// Registers a terminal owner. Its sessions end when it closes or the
    /// connection that made it does. Answered with
    /// [`Response::TerminalOwner`].
    TerminalOwner,
    /// Starts the host user's login shell. Answered with
    /// [`Response::TerminalSession`].
    TerminalSpawn {
        owner: String,
        context: TerminalContext,
        cols: u16,
        rows: u16,
    },
    /// The session's output since the last read, waiting up to `wait_ms`
    /// for some to arrive. Answered with [`Response::TerminalOutput`].
    TerminalRead {
        owner: String,
        id: String,
        #[serde(default)]
        wait_ms: u32,
    },
    TerminalWrite {
        owner: String,
        id: String,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    TerminalResize {
        owner: String,
        id: String,
        cols: u16,
        rows: u16,
    },
    TerminalClose {
        owner: String,
        id: String,
    },
    TerminalCloseOwner {
        owner: String,
    },
}

// Responses are moved once, from the wire to their caller; boxing the
// snapshot would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Agents(AgentsSnapshot),
    Harnesses(Vec<Harness>),
    Prepared {
        id: String,
        pubkey: String,
        /// A retried create whose agent is already saved.
        #[serde(default)]
        saved: bool,
    },
    Models(ModelCatalog),
    Defaults(AgentDefaults),
    ImportPreview(ImportPreview),
    /// The connection test got a reply. `model` is `provider/model`, absent
    /// when a saved environment override picked it.
    Tested {
        model: Option<String>,
    },
    Disconnected,
    Installed(InstallReport),
    /// `Some(false)` when Claude Code says it is signed out; `None` when
    /// that could not be confirmed.
    ClaudeAuth(Option<bool>),
    /// As [`Response::ClaudeAuth`], for the Codex CLI.
    CodexAuth(Option<bool>),
    /// Answers requests that return nothing.
    Done,
    CloneSettings(CloneSettings),
    Library(Library),
    Ticket(u64),
    Catalog(Catalog),
    Nonce(String),
    Log(String),
    SignInPage(SignInPage),
    // snapshots
    /// The memory events published, in entry order, and why the others
    /// were not.
    MemoryRestored {
        published: Vec<RestoredMemory>,
        errors: Vec<String>,
    },
    // Agents2 custody
    AppAgent(AppAgentResponse),
    // teams
    TeamSnapshot(TeamSnapshot),
    TeamMemberPrepared {
        id: String,
        pubkey: String,
        /// The request already created this agent.
        saved: bool,
    },
    // claude code
    /// A process request succeeded.
    ProcessDone,
    TerminalOwner(String),
    TerminalSession(String),
    TerminalOutput(TerminalOutput),
}

/// What an install did (buzz-app's `InstallReport`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstallReport {
    pub ready: bool,
    /// Agents waiting for Pi that restarted, and those that did not.
    pub restarted: u32,
    pub restart_failures: u32,
    /// The install log on the host, and its last 8 KiB.
    pub log_path: String,
    pub output: String,
    pub error: Option<String>,
}

/// Requests the host makes of the GUI, which holds the human key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostAsk {
    /// Sign the NIP-OA owner attestation for an agent key being imported.
    /// The GUI only answers for keys it asked to import.
    AttestAgent { agent_pubkey: String },
    /// Open this sign-in page in the human's browser. Its redirect goes to
    /// a loopback port on the host; a GUI on another machine forwards the
    /// browser's callback with [`Request::SignInCallback`].
    OpenUrl { url: String },
    // claude code
    /// Output of a process this connection spawned, in order. An
    /// [`ProcessOutput::Exit`] is always the last.
    ProcessOutput {
        handle: u64,
        output: Vec<ProcessOutput>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostAnswer {
    /// The `auth` tag, JSON encoded.
    Attestation(String),
    Opened,
    // claude code
    /// The output was taken. An error instead stops the process.
    ProcessReceived,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostEvent {
    /// Sent whenever anything in the snapshot changes, including process
    /// status changes the GUI did not cause.
    Agents(AgentsSnapshot),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentAction {
    Start,
    Stop,
    Restart,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentsSnapshot {
    /// The host event sequence this snapshot reflects. Events with a lower
    /// or equal sequence are older than this snapshot.
    pub seq: u64,
    pub agents: Vec<Agent>,
    pub runtime_available: bool,
    pub runtime_message: Option<String>,
    /// Where new agents work when the create request leaves the workspace
    /// empty.
    pub default_workspace: String,
    /// Set on the reply to a save: running agents whose launch settings the
    /// save changed, restarted with them, and those that did not restart.
    pub restarted: Option<u32>,
    pub restart_failures: Option<u32>,
    /// Identities an earlier Buzz install on the host knows, which can be
    /// imported.
    pub parked: Vec<ParkedIdentity>,
    /// Problems reading those installs.
    pub inventory_warnings: Vec<String>,
    /// `None` from hosts that do not say; treat as available.
    pub import_available: Option<bool>,
    pub create_available: Option<bool>,
    pub avatar_editing_available: bool,
    /// The host serves Use here and clone settings.
    pub local_inventory_actions: bool,
    /// The harnesses agents can use, with their setup status. `None` from
    /// older hosts, which answer [`Request::Harnesses`] instead.
    pub harness_options: Option<Vec<HarnessOption>>,
    pub claude_setup: Option<ClaudeSetup>,
    /// How to sign in to Codex on the host. `None` from hosts before Codex.
    pub codex_setup: Option<CodexSetup>,
    /// The build's Databricks workspace and model filter.
    pub databricks_defaults: Option<DatabricksSettings>,
    /// The build's model defaults for Buzz Agent.
    pub agent_defaults: Option<BuildDefaults>,
    /// The host's agent defaults, as [`Request::Defaults`] returns them.
    pub default_settings: Option<AgentDefaults>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ParkedIdentity {
    pub pubkey: String,
    pub name: String,
    pub sources: Vec<ImportSource>,
}

/// One harness agents can use, as buzz-app's `harnessOptions` lists it.
/// Availability is executable presence only, not sign-in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarnessOption {
    /// The controller's `HarnessIntegration`: `buzz-agent`, `goose`, `pi`,
    /// `codex` or `external`. `None` from hosts before Codex.
    pub id: Option<String>,
    /// What [`HarnessEdit::command`] holds for this harness.
    pub command: String,
    pub label: String,
    pub available: bool,
    /// `ready`, `cli-needed`, `adapter-needed`, `check-needed` or
    /// `not-enabled`.
    pub status: String,
    /// The host can install it (Pi, Claude Code and Codex's adapter).
    pub install_supported: Option<bool>,
    /// The host's own install is older than the pinned release.
    pub update_supported: Option<bool>,
    pub default_args: Vec<String>,
    pub providers: Vec<ProviderOption>,
    pub configuration_policy: Option<ConfigurationPolicy>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderOption {
    pub value: String,
    pub label: String,
}

/// The controller's `HarnessConfigurationPolicy`, as strings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConfigurationPolicy {
    /// `provider`, `harnessWithOverrides` or `external`.
    pub authentication: String,
    /// `selector`, `discovered` or `external`.
    pub provider: String,
    /// `optional` or `withProvider`.
    pub model: String,
    pub selector_environment: Option<SelectorEnvironment>,
    /// Saved configuration modes the integration admits: `default`,
    /// `advanced`. Empty for all but Codex.
    pub supported_modes: Vec<String>,
    /// `unknown`, or `modelSpecific` when the integration reports effort
    /// levels per model.
    pub effort_discovery: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SelectorEnvironment {
    pub model: String,
    pub provider: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClaudeSetup {
    pub status: String,
    pub install_supported: bool,
    /// Signs in to Claude Code in a terminal on the host.
    pub login_command: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexSetup {
    /// Signs in to the Codex CLI in a terminal on the host; `None` while
    /// the CLI is not installed.
    pub login_command: Option<String>,
}

/// The controller's `AiConfiguration` (`{"mode":"default"}` or
/// `{"mode":"advanced","effort":{"kind":"value","value":…}}` /
/// `{"kind":"unsupported"}`). Kept as JSON, so a mode a newer host adds
/// neither breaks this side's snapshot nor is lost when an edit sends it
/// back.
pub type AiConfiguration = serde_json::Value;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Agent {
    pub id: String,
    pub pubkey: String,
    pub relay_url: String,
    pub name: String,
    pub picture: Option<String>,
    pub system_prompt: String,
    pub session_policy: Option<SessionPolicy>,
    pub workspace: String,
    pub harness: HarnessSettings,
    pub revision: u64,
    pub running_revision: Option<u64>,
    pub enabled: bool,
    pub status: ProcessStatus,
    pub error: Option<String>,
    pub diagnostics: Vec<String>,
    pub profile_pending: bool,
    pub start_on_app_launch: bool,
    pub respond_to: Option<String>,
    pub acp_command: Option<String>,
    pub mcp_command: Option<String>,
    pub launch_model: Option<String>,
    pub launch_provider: Option<String>,
    pub launch_parallelism: Option<u32>,
    pub launch_effort: Option<String>,
    pub deployed_remote: bool,
    pub needs_team_import: bool,
    /// Older hosts leave it out, which means configured.
    #[serde(default = "yes")]
    pub configured: bool,
    /// The environment variable that picks the launch model or provider,
    /// when one does; its value stays on the host.
    pub launch_model_env: Option<String>,
    pub launch_provider_env: Option<String>,
    /// Settings saved since the running process started.
    pub restart_diff: Vec<RestartDiffEntry>,
    /// Imported provider backend; local agents have none.
    pub backend: Option<String>,
    /// Why a snapshot export would not be faithful, as fixed reason names
    /// ("team instructions", "idle timeout", ...). `None` from hosts that
    /// predate snapshots.
    pub snapshot_export_limitations: Option<Vec<String>>,
    /// When the host saw the agent enter its current status, in Unix
    /// milliseconds. `None` from hosts that predate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_since: Option<u64>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RestartDiffEntry {
    pub field: String,
    /// The redacted change, as the controller reports it. Absent from
    /// older hosts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<RestartChange>,
}

/// A saved-versus-running difference (the controller's `RestartChange`):
/// safe values verbatim, long text as character counts, secrets masked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RestartChange {
    Value {
        before: serde_json::Value,
        after: serde_json::Value,
    },
    #[serde(rename_all = "camelCase")]
    Text {
        before_chars: Option<usize>,
        after_chars: Option<usize>,
    },
    Masked {
        before: Option<String>,
        after: Option<String>,
    },
    Added,
    Removed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarnessSettings {
    /// The controller's `HarnessIntegration`; `None` for older and custom
    /// harnesses.
    pub integration: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub model: String,
    /// Codex's Default or Advanced choice.
    pub configuration: Option<AiConfiguration>,
    pub provider: String,
    pub environment_keys: Vec<String>,
    pub databricks: Option<DatabricksSettings>,
}

/// Mirrors the controller's `DatabricksSettings`: Buzz Agent's Databricks
/// workspace origin and its comma-separated model ID globs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DatabricksSettings {
    pub host: String,
    pub filter: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessStatus {
    #[default]
    Stopped,
    Waiting,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionPolicy {
    Channel,
    Thread,
}

/// Mirrors the controller's `AgentEdit`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEdit {
    pub name: String,
    #[serde(default)]
    pub picture: Option<String>,
    pub system_prompt: String,
    /// `None` leaves the saved choice; `Some(None)` clears it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub session_policy: Option<Option<SessionPolicy>>,
    pub workspace: String,
    pub harness: HarnessEdit,
    /// `None` values remove a saved variable. Values are write-only: the
    /// host never sends them back.
    pub environment: BTreeMap<String, Option<String>>,
    /// `None` leaves the agent's own effort; `Some(None)` clears it, so the
    /// agent follows its defaults again. Hosts before clearing existed read
    /// `Some(None)` as `None`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub effort: Option<Option<String>>,
}

/// Tells a missing field (`None`, by `default`) from an explicit null
/// (`Some(None)`), which plain `Option<Option<_>>` reads as `None`.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessEdit {
    /// The controller's `HarnessIntegration`; only Codex agents set one.
    /// Left out when `None`, as the controller writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<AiConfiguration>,
    pub provider: String,
    /// Buzz Agent's Databricks settings. `None` clears them, so an edit
    /// sends back what it does not change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub databricks: Option<DatabricksSettings>,
}

/// A harness this host can run, as detected on its PATH.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Harness {
    pub kind: HarnessKind,
    pub label: String,
    /// What to put in [`HarnessEdit::command`]. `None` while not installed.
    pub command: Option<String>,
    /// Executables that must be installed before this harness can run.
    pub missing: Vec<String>,
    pub setup_hint: String,
    pub setup_url: Option<String>,
    /// `None` from hosts that predate it.
    #[serde(default)]
    pub status: Option<HarnessStatus>,
    /// The host can install this harness itself.
    #[serde(default)]
    pub install_supported: bool,
    /// The host's own install is older than the release it pins.
    #[serde(default)]
    pub update_supported: bool,
    /// Claude Code: the terminal command that signs in.
    #[serde(default)]
    pub login_command: Option<String>,
}

/// buzz-app's harness status: what a harness still needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessStatus {
    Ready,
    /// The CLI, or Node.js for it, is missing.
    CliNeeded,
    /// The CLI is there but its ACP adapter is not.
    AdapterNeeded,
    /// The tools are there but need checking again.
    CheckNeeded,
    /// Not available on this platform yet.
    NotEnabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    BuzzAgent,
    Goose,
    Claude,
    Pi,
    Hermes,
    /// Never sent to GUIs from before Codex.
    Codex,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelCatalog {
    pub models: Vec<ModelOption>,
    /// An environment override picks the model, so a choice here is ignored.
    pub model_overridden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelOption {
    /// For Pi, `provider/model`.
    pub id: String,
    pub name: String,
}

/// Mirrors the controller's `AgentDefaultsView`: environment keys only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentDefaults {
    /// `buzz-agent`, `goose` or `pi`.
    pub harness: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub session_policy: Option<SessionPolicy>,
    pub environment_keys: Vec<String>,
    /// Set on the reply to a save, as in [`AgentsSnapshot`].
    pub restarted: Option<u32>,
    pub restart_failures: Option<u32>,
    /// What this host's build fills in for Buzz Agent.
    pub build: BuildDefaults,
}

/// Mirrors the controller's `BuildDefaults`: public, nonsecret build
/// configuration for Buzz Agent. Empty when the build sets none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BuildDefaults {
    pub provider: String,
    pub model: String,
    /// The Databricks workspace and model filter.
    pub host: String,
    pub filter: String,
    /// Agents answer only their owner and same-owner agents.
    pub owner_only: bool,
}

/// Mirrors the controller's `AgentDefaultsEdit`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultsEdit {
    pub harness: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    /// `None` keeps the saved choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_policy: Option<SessionPolicy>,
    /// `None` values remove a saved variable. Values are write-only.
    pub environment: BTreeMap<String, Option<String>>,
}

/// Which earlier Buzz install to import from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportSource {
    Installed,
    Development,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportPreview {
    /// Empty when nothing can be imported from this preview.
    pub token: String,
    pub source_path: String,
    pub candidates: Vec<ImportCandidate>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImportCandidate {
    pub id: String,
    pub pubkey: String,
    pub relay_url: String,
    pub name: String,
    /// The imported prompt drops a team section old Buzz baked into it.
    pub strips_team_instructions: bool,
    /// The key is already saved on this host, so it cannot be imported.
    pub imported: bool,
    /// The old record has no owner attestation; importing asks the GUI to
    /// sign one.
    pub needs_attestation: bool,
}

/// The owner's signed intent to set up one agent identity in one
/// community: a schnorr signature over
/// `nostr:agent-community:{pubkey}:{relay_url}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CommunityResolution {
    pub pubkey: String,
    /// `wss://host`, without a trailing slash.
    pub relay_url: String,
    pub owner: String,
    pub signature: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CloneSettings {
    pub name: String,
    pub system_prompt: String,
}

/// The agent library of an earlier Buzz install: definitions (profiles)
/// and the identities created from them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Library {
    pub definitions: Vec<LibraryDefinition>,
    pub identities: Vec<LibraryIdentity>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LibraryDefinition {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LibraryIdentity {
    pub pubkey: String,
    pub name: String,
    pub avatar: Option<String>,
    pub definition_id: Option<String>,
}

/// buzz-app's model request (`agent_models_run`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelRequest {
    /// The saved agent being edited; `None` for a new draft.
    pub id: Option<String>,
    /// Set exactly when `id` is.
    pub expected_revision: Option<u64>,
    /// Only a disconnect may leave it out.
    pub edit: Option<AgentEdit>,
    /// The Databricks workspace and model filter the form shows.
    pub host: String,
    pub filter: String,
    pub action: ModelAction,
    /// Blank `host` and `filter` follow the host's agent defaults.
    pub inherit_workspace: bool,
    /// The harness's `HarnessIntegration`. Only `codex` changes the lookup,
    /// which then lists the models the user's Codex CLI reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration: Option<String>,
    /// The model whose effort levels a Codex lookup also reports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_model: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelAction {
    /// Browse or Retry: list models, signing in if needed.
    #[default]
    Connect,
    /// List models without signing in.
    Refresh,
    Disconnect,
    /// One short completion with the draft's provider and model.
    Test,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Catalog {
    /// The Databricks workspace used; empty for Pi, Goose or an inherited
    /// one.
    pub host: String,
    pub models: Vec<ModelOption>,
    pub model_overridden: bool,
    pub disconnected: bool,
    /// For a test: the `provider/model` that replied, unless an override
    /// chose it.
    pub tested_model: Option<String>,
    /// What the Codex CLI reported, for Codex lookups only.
    pub codex: Option<CodexCatalog>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexCatalog {
    /// False when Codex published no usable model list; true with no
    /// models is a known-empty catalog.
    pub models_known: bool,
    /// The effort levels of the selected model.
    pub effort: Option<CodexEffort>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexEffort {
    /// The model these levels belong to.
    pub model: String,
    /// Its default level, when reported.
    pub current: Option<String>,
    pub options: Vec<ModelOption>,
}

/// What the host's loopback sign-in listener answered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SignInPage {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

#[cfg(test)]
mod tests {
    //! The wire stays readable across versions: what an older peer sends
    //! decodes here, and what this side adds an older peer ignores.
    use super::*;

    fn wire<T: Serialize>(value: &T) -> Vec<u8> {
        rmp_serde::to_vec_named(value).unwrap()
    }

    #[test]
    fn an_older_gui_start_has_no_floor_or_owner() {
        #[derive(Serialize)]
        enum OldRequest {
            Action { id: String, action: AgentAction },
        }
        let old = wire(&OldRequest::Action {
            id: "a".into(),
            action: AgentAction::Start,
        });
        let Request::Action {
            replay_floor,
            owner,
            ..
        } = rmp_serde::from_slice(&old).unwrap()
        else {
            panic!("expected an action");
        };
        assert_eq!((replay_floor, owner), (None, None));
    }

    #[test]
    fn an_older_host_snapshot_reads_as_configured_with_no_options() {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct OldAgent {
            id: String,
            status: ProcessStatus,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct OldSnapshot {
            seq: u64,
            agents: Vec<OldAgent>,
            runtime_available: bool,
        }
        let old = wire(&OldSnapshot {
            seq: 3,
            agents: vec![OldAgent {
                id: "a".into(),
                status: ProcessStatus::Running,
            }],
            runtime_available: true,
        });
        let snapshot: AgentsSnapshot = rmp_serde::from_slice(&old).unwrap();
        assert!(snapshot.agents[0].configured);
        assert!(snapshot.harness_options.is_none());
        assert_eq!(snapshot.import_available, None);
    }

    #[test]
    fn an_older_peer_skips_the_new_snapshot_fields() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OldSnapshot {
            seq: u64,
            agents: Vec<serde::de::IgnoredAny>,
        }
        let snapshot = AgentsSnapshot {
            seq: 7,
            agents: vec![Agent::default()],
            harness_options: Some(vec![HarnessOption::default()]),
            parked: vec![ParkedIdentity::default()],
            ..Default::default()
        };
        let old: OldSnapshot = rmp_serde::from_slice(&wire(&snapshot)).unwrap();
        assert_eq!((old.seq, old.agents.len()), (7, 1));
    }

    #[test]
    fn status_since_travels_and_older_peers_ignore_it() {
        let agent = Agent {
            id: "a".into(),
            status: ProcessStatus::Running,
            status_since: Some(1_760_000_000_000),
            ..Default::default()
        };
        let read: Agent = rmp_serde::from_slice(&wire(&agent)).unwrap();
        assert_eq!(read, agent);
        // An older GUI reads the agent it knows.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct OldAgent {
            id: String,
            status: ProcessStatus,
        }
        let old: OldAgent = rmp_serde::from_slice(&wire(&agent)).unwrap();
        assert_eq!((old.id.as_str(), old.status), ("a", ProcessStatus::Running));
        // An older host's agent has no time, and none is written for it.
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct OlderAgent {
            id: String,
            status: ProcessStatus,
        }
        let older: Agent = rmp_serde::from_slice(&wire(&OlderAgent {
            id: "a".into(),
            status: ProcessStatus::Failed,
        }))
        .unwrap();
        assert_eq!(older.status_since, None);
        let bytes = wire(&Agent::default());
        assert!(!bytes.windows(11).any(|w| w == b"statusSince"));
    }

    #[test]
    fn codex_settings_survive_modes_this_side_does_not_know() {
        let configuration = serde_json::json!({
            "mode": "advanced",
            "effort": { "kind": "value", "value": "high" },
            "later": { "kind": "something new" },
        });
        let harness = HarnessSettings {
            integration: Some("codex".into()),
            configuration: Some(configuration.clone()),
            ..Default::default()
        };
        let read: HarnessSettings = rmp_serde::from_slice(&wire(&harness)).unwrap();
        assert_eq!(read.configuration, Some(configuration));
        assert_eq!(read.integration.as_deref(), Some("codex"));
    }

    #[test]
    fn edits_without_an_integration_leave_its_fields_out() {
        // As the controller writes them: no integration, no fields.
        let edit = HarnessEdit {
            command: "goose".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&edit).unwrap();
        assert!(json.get("integration").is_none());
        assert!(json.get("configuration").is_none());
        let request = serde_json::to_value(ModelRequest::default()).unwrap();
        assert!(request.get("integration").is_none());
        assert!(request.get("selectedModel").is_none());
    }

    #[test]
    fn an_older_host_catalog_has_no_codex_details() {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct OldCatalog {
            host: String,
            models: Vec<ModelOption>,
        }
        let old = wire(&OldCatalog {
            host: String::new(),
            models: vec![ModelOption::default()],
        });
        let catalog: Catalog = rmp_serde::from_slice(&old).unwrap();
        assert_eq!((catalog.models.len(), catalog.codex), (1, None));
    }
}

/// One snapshot memory entry to write.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MemoryRestore {
    /// `core` or a `mem/...` path.
    pub slug: String,
    pub body: String,
    /// When the agent already holds this slug: its entry's timestamp, so
    /// the new event supersedes it.
    pub after: Option<u64>,
}

// teams

/// Mirrors the controller's `TeamMeta`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMeta {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
}

/// Mirrors the controller's `TeamSnapshot`: a portable team file. Keys,
/// credentials and local paths never travel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSnapshot {
    pub format: String,
    pub version: u32,
    pub team: TeamMeta,
    pub members: Vec<MemberSnapshot>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberSnapshot {
    pub format: String,
    pub version: u32,
    pub definition: MemberDefinition,
    pub profile: MemberProfile,
    pub memory: MemberMemory,
}

fn channel() -> SessionPolicy {
    SessionPolicy::Channel
}

/// Mirrors the controller's team `Definition`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberDefinition {
    pub name: String,
    #[serde(default)]
    pub source_is_builtin: bool,
    #[serde(default)]
    pub name_pool: Vec<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default = "channel")]
    pub session_policy: SessionPolicy,
    #[serde(default)]
    pub respond_to: Option<String>,
    #[serde(default)]
    pub respond_to_allowlist: Vec<String>,
    #[serde(default)]
    pub parallelism: Option<u32>,
    #[serde(default)]
    pub idle_timeout_seconds: Option<u64>,
    #[serde(default)]
    pub max_turn_duration_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Default for MemberDefinition {
    fn default() -> Self {
        Self {
            name: String::new(),
            source_is_builtin: false,
            name_pool: Vec::new(),
            system_prompt: None,
            runtime: None,
            model: None,
            provider: None,
            session_policy: SessionPolicy::Channel,
            respond_to: None,
            respond_to_allowlist: Vec::new(),
            parallelism: None,
            idle_timeout_seconds: None,
            max_turn_duration_seconds: None,
            effort: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberProfile {
    pub display_name: String,
    #[serde(default)]
    pub about: Option<String>,
    #[serde(default)]
    pub avatar_data_url: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// `level` is `none`, `core` or `everything`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberMemory {
    pub level: String,
    #[serde(default)]
    pub entries: Vec<MemoryEntry>,
}

impl Default for MemberMemory {
    fn default() -> Self {
        Self {
            level: "none".into(),
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub slug: String,
    pub body: String,
}

/// Mirrors the controller's `TeamCatalogEntry`: one team's current head.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamCatalogEntry {
    pub created_at: u64,
    pub event_id: String,
    pub members: Vec<String>,
}

/// Mirrors the controller's `BundleMember`: what binds a new agent to the
/// team it is imported with. Memories are restored separately.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleMember {
    /// The saved team's ID.
    pub team: String,
    pub member: MemberSnapshot,
    pub instructions: String,
    pub keep_allowlist: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RestoredMemory {
    pub slug: String,
    pub event_id: String,
}

// claude code
/// What a spawned process printed, or how it ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessOutput {
    Stdout(String),
    Stderr(String),
    /// `None` when a signal ended it.
    Exit(Option<i32>),
}
