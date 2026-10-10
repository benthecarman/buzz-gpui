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
    /// for Pi. One install runs at a time.
    InstallHarness {
        kind: HarnessKind,
    },
    /// Asks Claude Code whether it is signed in. Never part of a snapshot.
    ClaudeAuth,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Agents(AgentsSnapshot),
    Harnesses(Vec<Harness>),
    Prepared {
        id: String,
        pubkey: String,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostAnswer {
    /// The `auth` tag, JSON encoded.
    Attestation(String),
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
}

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
    pub configured: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HarnessSettings {
    pub command: String,
    pub args: Vec<String>,
    pub model: String,
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
    pub command: String,
    pub args: Vec<String>,
    pub model: String,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    BuzzAgent,
    Goose,
    Claude,
    Pi,
    Hermes,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
