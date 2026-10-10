//! An agent's memories as its owner reads them: kind 30174 events the agent
//! encrypts to the owner, read over the relay's NIP-98 `/query` and opened
//! with the human key, which never leaves `buzz-identity`.

use buzz_identity::Identity;
use buzz_relay_client::{RelayHttp, RelayOrigin};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMemory {
    pub slug: String,
    pub body: String,
    pub event_id: String,
    pub created_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Listing {
    pub entries: Vec<StoredMemory>,
    /// Some records could not be read, or the read hit its cap.
    pub partial: bool,
}

impl Listing {
    pub fn get(&self, slug: &str) -> Option<&StoredMemory> {
        self.entries.iter().find(|entry| entry.slug == slug)
    }
}

/// The newest memory per slug that `agent` wrote to this identity in
/// `origin`'s community. Runs on tokio.
pub async fn read(
    origin: RelayOrigin,
    identity: Arc<Identity>,
    agent: &str,
) -> Result<Listing, String> {
    const FAILED: &str = "Memory read failed";
    let query = buzz_desktop::relay::agent::memory_query(&identity.pubkey_hex(), agent)?;
    let http = RelayHttp::new(origin, identity.clone());
    let body = http.post("/query", query).await.map_err(|_| FAILED)?;
    let text = std::str::from_utf8(&body).map_err(|_| FAILED)?;
    let records = buzz_desktop::relay::agent::memory_wire_records(text)?;
    let listing = identity.decode_agent_memories(agent, &records)?;
    serde_json::from_value(listing).map_err(|_| FAILED.into())
}

/// A complete listing, or the reference's error for a partial one.
pub async fn read_complete(
    origin: RelayOrigin,
    identity: Arc<Identity>,
    agent: &str,
) -> Result<Listing, String> {
    const INCOMPLETE: &str =
        "Couldn’t load complete memories. Check your connection and try again.";
    match read(origin, identity, agent).await {
        Ok(listing) if !listing.partial => Ok(listing),
        _ => Err(INCOMPLETE.into()),
    }
}
