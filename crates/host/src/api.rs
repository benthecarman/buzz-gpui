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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Agents(AgentsSnapshot),
    Harnesses(Vec<Harness>),
    Prepared { id: String, pubkey: String },
}

/// Requests the host makes of the GUI. None yet; signing requests for
/// import and log access will go here.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostAsk {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HostAnswer {}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_policy: Option<Option<SessionPolicy>>,
    pub workspace: String,
    pub harness: HarnessEdit,
    /// `None` values remove a saved variable. Values are write-only: the
    /// host never sends them back.
    pub environment: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessEdit {
    pub command: String,
    pub args: Vec<String>,
    pub model: String,
    pub provider: String,
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
