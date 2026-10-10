//! Restoring an imported snapshot's memory under the new agent identity
//! (buzz-app `src-tauri/src/agents/snapshot_memory.rs`).
//!
//! Memories are owner-addressed (kind 30174, NIP-44 to the owner, signed by
//! the agent). The agent key stays here and the human key in the GUI, so
//! the work splits: the GUI reads the agent's current memories with the
//! owner key and sends only the entries that differ, each with the
//! timestamp of the entry it replaces; the host checks custody, signs and
//! publishes; the GUI then reads the memories back to confirm each event
//! became current.

use crate::AgentHost;
use crate::api::{MemoryRestore, Response, RestoredMemory};
use crate::profile::{authorization, body};
use buzz_agent_controller::{CreationProfile, ProcessStatus, Secret};
use buzz_host_rpc::RpcError;
use serde_json::Value;
use std::collections::BTreeSet;

type Result<T> = std::result::Result<T, String>;

/// The import limits and the native reader's budget (`validate`).
fn validate(entries: &[MemoryRestore]) -> Result<()> {
    if entries.len() > 128
        || entries.iter().any(|entry| entry.body.len() > 64 * 1024)
        || entries.iter().map(|entry| entry.body.len()).sum::<usize>() > 1024 * 1024
    {
        return Err("Snapshot memory exceeds the import limit".into());
    }
    if entries
        .iter()
        .any(|entry| !buzz_desktop::relay::agent::slug(&entry.slug))
    {
        return Err("Invalid snapshot memory slug".into());
    }
    for entry in entries {
        buzz_agent_controller::validate_snapshot_memory_envelope(&entry.slug, &entry.body)
            .map_err(|_| "Snapshot memory contains hidden text or exceeds the readable limit")?;
    }
    // The reader's serialized DTO budget, with a full u64 timestamp.
    let reader_bytes = entries
        .iter()
        .map(|entry| {
            serde_json::to_vec(&serde_json::json!({
                "slug": entry.slug, "body": entry.body,
                "eventId": "0".repeat(64), "createdAt": u64::MAX
            }))
            .map_or(usize::MAX, |bytes| bytes.len())
        })
        .try_fold(0usize, |sum, bytes| sum.checked_add(bytes))
        .ok_or("Snapshot memory exceeds the readable limit")?;
    if reader_bytes > 1024 * 1024 {
        return Err("Snapshot memory exceeds the readable limit".into());
    }
    let mut slugs = BTreeSet::new();
    if entries.iter().any(|entry| !slugs.insert(&entry.slug)) {
        return Err("Duplicate snapshot memory slug".into());
    }
    Ok(())
}

fn same(a: &CreationProfile, b: &CreationProfile) -> bool {
    a.pubkey == b.pubkey
        && a.revision == b.revision
        && a.credential_id == b.credential_id
        && a.auth == b.auth
        && a.url == b.url
}

fn now() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| "System clock is unavailable".into())
}

impl AgentHost {
    /// The agent's memory target, when `owner` attested it and it is
    /// stopped.
    async fn memory_target(&self, id: String, owner: String) -> Result<CreationProfile> {
        self.run(move |host| {
            // Checks the native-created marker and the signed attestation.
            let profile = host.controller.memory_target(&id)?;
            let authorization: Vec<String> =
                serde_json::from_str(&profile.auth).map_err(|_| "Invalid owner authorization")?;
            if authorization.get(1) != Some(&owner) {
                return Err("Snapshot memory belongs to another owner".into());
            }
            let agent = host
                .controller
                .snapshot()?
                .agents
                .into_iter()
                .find(|agent| agent.id == id)
                .ok_or("Agent no longer exists")?;
            if agent.status != ProcessStatus::Stopped {
                return Err("Stop the imported agent before restoring memories".into());
            }
            Ok(profile)
        })
        .await
        .map_err(|error| error.to_string())
    }

    /// Fails when the agent changed since `expected` was read.
    async fn still(&self, id: &str, owner: &str, expected: &CreationProfile) -> Result<()> {
        let current = self.memory_target(id.into(), owner.into()).await?;
        if !same(&current, expected) {
            return Err("Agent changed during memory restore".into());
        }
        Ok(())
    }

    pub(crate) async fn restore_memory(
        &self,
        id: String,
        owner: String,
        entries: Vec<MemoryRestore>,
    ) -> std::result::Result<Response, RpcError> {
        if id.is_empty() {
            return Ok(Response::MemoryRestored {
                published: Vec::new(),
                errors: Vec::new(),
            });
        }
        validate(&entries).map_err(RpcError::Failed)?;
        // Custody is checked before the key is read, and before each send.
        let profile = self
            .memory_target(id.clone(), owner.clone())
            .await
            .map_err(RpcError::Failed)?;
        let (credential, pubkey) = (profile.credential_id.clone(), profile.pubkey.clone());
        let key = self
            .run(move |host| {
                host.credentials
                    .read(&credential, &pubkey)?
                    .ok_or_else(|| "Agent key unavailable".into())
            })
            .await?;
        if key.pubkey() != profile.pubkey {
            return Err(RpcError::Failed("Agent key changed".into()));
        }
        let mut published = Vec::new();
        let mut errors = Vec::new();
        for entry in entries {
            let outcome = async {
                self.still(&id, &owner, &profile).await?;
                let now = now()?;
                let timestamp = entry.after.map_or(now, |at| now.max(at.saturating_add(1)));
                let event = key.memory_event(&owner, &entry.slug, &entry.body, timestamp)?;
                let event_id = event["id"]
                    .as_str()
                    .ok_or("Invalid memory event")?
                    .to_owned();
                // Encryption and signing take time; check again right before
                // the send.
                self.still(&id, &owner, &profile).await?;
                publish(&self.http, &profile, &key, event).await?;
                Ok::<_, String>(event_id)
            }
            .await;
            match outcome {
                Ok(event_id) => published.push(RestoredMemory {
                    slug: entry.slug,
                    event_id,
                }),
                Err(error) => {
                    tracing::warn!(agent = %id, slug = %entry.slug, %error, "memory restore failed");
                    errors.push(format!(
                        "{}: memory restore failed; retry this agent",
                        entry.slug
                    ))
                }
            }
        }
        Ok(Response::MemoryRestored { published, errors })
    }
}

/// Publishes an agent-signed memory event and checks the relay accepted it
/// (`profile_http::publish_memory`).
async fn publish(
    client: &reqwest::Client,
    profile: &CreationProfile,
    key: &Secret,
    event: Value,
) -> Result<()> {
    const UNCONFIRMED: &str = "Memory was not confirmed; inspect the new agent before retrying";
    let expected = event
        .get("id")
        .and_then(Value::as_str)
        .ok_or("Invalid memory event")?
        .to_owned();
    let bytes = serde_json::to_vec(&event).map_err(|_| "Could not encode memory event")?;
    let response = client
        .post(&profile.url)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            authorization(profile.authenticate(key, &bytes)?)?,
        )
        .header("x-auth-tag", &profile.auth)
        .body(bytes)
        .send()
        .await
        .map_err(|_| "Memory publication unconfirmed; inspect the new agent before retrying")?;
    let receipt: Value = serde_json::from_slice(
        &body(response, 16 * 1024)
            .await
            .map_err(|_| UNCONFIRMED.to_string())?,
    )
    .map_err(|_| UNCONFIRMED)?;
    if receipt.get("accepted").and_then(Value::as_bool) != Some(true)
        || receipt.get("event_id").and_then(Value::as_str) != Some(&expected)
    {
        return Err(UNCONFIRMED.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(slug: &str, body: &str) -> MemoryRestore {
        MemoryRestore {
            slug: slug.into(),
            body: body.into(),
            after: None,
        }
    }

    #[test]
    fn entries_are_validated_like_the_reference() {
        assert!(validate(&[entry("core", "I am Scout."), entry("mem/notes", "tea")]).is_ok());
        assert_eq!(
            validate(&[entry("notes", "x")]).unwrap_err(),
            "Invalid snapshot memory slug"
        );
        assert_eq!(
            validate(&[entry("core", "a"), entry("core", "b")]).unwrap_err(),
            "Duplicate snapshot memory slug"
        );
        assert_eq!(
            validate(&[entry("core", "hidden\u{200b}text")]).unwrap_err(),
            "Snapshot memory contains hidden text or exceeds the readable limit"
        );
        let many: Vec<_> = (0..129).map(|i| entry(&format!("mem/m{i}"), "x")).collect();
        assert_eq!(
            validate(&many).unwrap_err(),
            "Snapshot memory exceeds the import limit"
        );
    }
}
