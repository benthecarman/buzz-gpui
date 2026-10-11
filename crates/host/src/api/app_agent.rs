//! Custody for Agents2 agents (buzz-app's `app_agent_*` commands). The host
//! keeps each agent's key and owner attestation; the GUI asks for one bounded
//! event, read, upload or memory write at a time, which the host signs as the
//! agent and sends to the agent's community. The saved identities are the
//! agent list. JSON payloads travel as JSON text.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AppAgentRequest {
    /// Every saved identity, deleted ones included.
    List,
    /// Generates the new agent's key. The GUI signs the owner attestation
    /// for the returned pubkey and sends it in [`Self::CreateCommit`].
    /// Retrying with the same `request_id` returns the same key.
    CreatePrepare {
        request_id: String,
        /// The community, as a relay URL; saved in canonical `wss://` form.
        destination: String,
        owner: String,
        /// The agent type's key, fixed at create.
        agent_type: String,
        name: String,
    },
    /// Saves the prepared identity and its key. `auth` is the NIP-OA `auth`
    /// tag, JSON encoded.
    CreateCommit {
        request_id: String,
        auth: String,
    },
    Rename {
        pubkey: String,
        name: String,
    },
    /// Deletes the key and marks the identity deleted. The row stays until
    /// [`Self::Forget`], once its community is cleaned up.
    Delete {
        pubkey: String,
    },
    /// Drops a deleted identity.
    Forget {
        pubkey: String,
    },
    /// Signs one event as the agent (deletions, reactions, messages, edits,
    /// canvases and DM opens only) and posts it to its community.
    Publish {
        pubkey: String,
        event: AppAgentEvent,
    },
    /// Publishes the agent's name as its profile unless its community
    /// already has it.
    PublishProfile {
        pubkey: String,
    },
    /// Reads the agent's community as the agent: a JSON array of one to
    /// eight filters.
    Query {
        pubkey: String,
        filters: String,
    },
    /// Uploads base64 `data` (an image or MP4) as the agent. A file larger
    /// than one frame arrives first in [`Self::UploadPart`]s named by
    /// `staged`, and `data` is its last part.
    Upload {
        pubkey: String,
        data: String,
        mime: String,
        #[serde(default)]
        staged: Option<String>,
    },
    /// One base64 part of a staged upload, appended in order.
    UploadPart {
        staged: String,
        data: String,
    },
    /// Writes memory entry `slug`, encrypted to the owner and newer than the
    /// entry it replaces (`after`, its `created_at`, or 0).
    Remember {
        pubkey: String,
        slug: String,
        body: String,
        after: u64,
    },
    /// Whether the asking GUI runs the agent now. Every GUI connected to
    /// this host, and every copy of the app sharing its state directory,
    /// hears the same events; one runs each agent. Asked before each run,
    /// so another takes over within a tick of the holder letting go or
    /// disconnecting. Answers [`AppAgentResponse::Claimed`]. Hosts from
    /// before this answer an error, and the GUI runs the agent anyway.
    Claim {
        pubkey: String,
    },
    /// Lets another GUI or copy of the app run the agent, as when this GUI
    /// cannot.
    Release {
        pubkey: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AppAgentResponse {
    Agents(Vec<AppAgentSummary>),
    Prepared {
        pubkey: String,
    },
    Created(AppAgentSummary),
    Done,
    /// The signed event the community accepted, as JSON.
    Event(String),
    /// The events a query returned, as a JSON array.
    Events(String),
    /// The community's description of an uploaded blob, as JSON.
    Uploaded(String),
    /// Whether the asking GUI runs the agent: see [`AppAgentRequest::Claim`].
    Claimed(bool),
}

/// An Agents2 agent's identity; never its key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppAgentSummary {
    pub pubkey: String,
    /// The canonical `wss://` community origin it was created for.
    pub relay: String,
    pub owner: String,
    /// The agent type's key, fixed at create.
    pub agent_type: String,
    pub name: String,
    /// Its key is gone. Its community still has to drop it from its
    /// channels and archive it, after which it is forgotten.
    pub deleted: bool,
}

/// An event for an agent to sign.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppAgentEvent {
    pub kind: u16,
    pub content: String,
    pub tags: Vec<Vec<String>>,
}
