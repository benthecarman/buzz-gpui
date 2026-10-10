//! Importing agents from an earlier Buzz install on this host.
//!
//! The controller does the import (`Imports`); this adds what a host needs
//! around it. Old Buzz records may lack an owner attestation, without which
//! the controller refuses to start an agent, and only the GUI holds the
//! human key to sign one. For those, the source is copied to a private
//! staging directory with the GUI's attestations added, and the import runs
//! from the copy. The original is never written.

use crate::api::{HostAnswer, HostAsk, ImportSource};
use buzz_agent_controller::LegacySource;
use buzz_host_rpc::RpcError;
use futures::future::BoxFuture;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Asks the GUI on the other end of a connection.
pub trait Asker: Send + Sync {
    fn ask(&self, ask: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>>;

    /// Resolves when the GUI behind this asker disconnects; never, for a
    /// caller without a connection.
    fn closed(&self) -> BoxFuture<'static, ()> {
        Box::pin(std::future::pending())
    }
}

impl Asker for buzz_host_rpc::Peer<crate::api::HostProtocol> {
    fn ask(&self, ask: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        let peer = self.clone();
        Box::pin(async move { peer.request(ask).await })
    }

    fn closed(&self) -> BoxFuture<'static, ()> {
        let peer = self.clone();
        Box::pin(async move { peer.closed().await })
    }
}

pub fn legacy(source: ImportSource) -> LegacySource {
    match source {
        ImportSource::Installed => LegacySource::Installed,
        ImportSource::Development => LegacySource::Development,
    }
}

/// The preview a commit must match.
pub struct Previewed {
    pub token: String,
    pub source: ImportSource,
    pub destination: String,
    pub parent: PathBuf,
    /// Of every source file, to notice changes before staging a copy.
    pub digest: String,
    /// Candidate ID to pubkey, for those without an attestation.
    pub unattested: BTreeMap<String, String>,
}

const MAX_FILE: u64 = 8 * 1024 * 1024;
const LIBRARY: &str = "agents/managed-agents.json";

/// The source files the controller reads, by path relative to the app
/// directory. Like the controller, refuses links and oversized files.
fn read_source(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let mut files = Vec::new();
    let mut read = |relative: PathBuf, required: bool| -> Result<(), String> {
        let path = root.join(&relative);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(_) => return Err("Could not read the Buzz agent library".into()),
        };
        if !meta.is_file() || meta.len() > MAX_FILE {
            return Err("Import files must be bounded regular files".into());
        }
        let bytes = std::fs::read(&path).map_err(|_| "Could not read the Buzz agent library")?;
        files.push((relative, bytes));
        Ok(())
    };
    read(LIBRARY.into(), true)?;
    read("agents/global-agent-config.json".into(), false)?;
    read("agents/teams.json".into(), false)?;
    match std::fs::read_dir(root.join("custom_harnesses")) {
        Ok(entries) => {
            let mut names = BTreeSet::new();
            for entry in entries {
                let entry = entry.map_err(|_| "Could not read source harness definitions")?;
                let name = entry.file_name();
                if Path::new(&name).extension().is_some_and(|e| e == "json") {
                    names.insert(name);
                }
                if names.len() > 128 {
                    return Err("Too many source harness definitions".into());
                }
            }
            for name in names {
                read(Path::new("custom_harnesses").join(name), true)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("Could not read source harness definitions".into()),
    }
    Ok(files)
}

fn digest(files: &[(PathBuf, Vec<u8>)]) -> String {
    let mut hash = Sha256::new();
    for (path, bytes) in files {
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}

fn records(files: &[(PathBuf, Vec<u8>)]) -> Result<Vec<Value>, String> {
    let (_, bytes) = files
        .iter()
        .find(|(path, _)| path == Path::new(LIBRARY))
        .ok_or("Could not read the Buzz agent library")?;
    serde_json::from_slice(bytes).map_err(|_| "The Buzz agent library is malformed".into())
}

fn attested(record: &Value) -> bool {
    record
        .get("auth_tag")
        .and_then(Value::as_str)
        .is_some_and(|tag| !tag.trim().is_empty())
}

/// What the commit needs to know about the source: its digest and which
/// pubkeys lack an attestation.
pub fn scan(root: &Path) -> Result<(String, BTreeSet<String>), String> {
    let files = read_source(root)?;
    let unattested = records(&files)?
        .iter()
        .filter(|record| !attested(record))
        .filter_map(|record| record.get("pubkey").and_then(Value::as_str))
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .collect();
    Ok((digest(&files), unattested))
}

/// Copies the source into `staging`, adding the attestations to records
/// that have none. Fails if the source changed since `expected`.
pub fn stage(
    root: &Path,
    staging: &Path,
    expected: &str,
    attestations: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut files = read_source(root)?;
    if digest(&files) != expected {
        return Err("The Buzz agent library changed after preview; preview it again".into());
    }
    let mut library = records(&files)?;
    for record in &mut library {
        let key = record.get("pubkey").and_then(Value::as_str).unwrap_or("");
        if let Some(tag) = attestations.get(key)
            && !attested(record)
            && let Some(record) = record.as_object_mut()
        {
            record.insert("auth_tag".into(), Value::String(tag.clone()));
        }
    }
    let bytes = serde_json::to_vec(&library).map_err(|_| "Could not stage the import")?;
    for (path, contents) in &mut files {
        if path == Path::new(LIBRARY) {
            *contents = bytes.clone();
        }
    }
    for (path, contents) in files {
        let target = staging.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "Could not stage the import")?;
        }
        std::fs::write(target, contents).map_err(|_| "Could not stage the import")?;
    }
    Ok(())
}

/// Checks the GUI's NIP-OA `auth` tag really attests `agent`, before the
/// host stores it.
pub fn verify_attestation(tag: &str, agent: &str) -> Result<(), String> {
    use secp256k1::{Secp256k1, XOnlyPublicKey, schnorr::Signature};
    let invalid = || "The owner attestation is invalid".to_string();
    let parts: Vec<String> = serde_json::from_str(tag).map_err(|_| invalid())?;
    let [kind, owner, conditions, signature] = parts.as_slice() else {
        return Err(invalid());
    };
    if kind != "auth" || !conditions.is_empty() || owner == agent {
        return Err(invalid());
    }
    let owner: XOnlyPublicKey = owner.parse().map_err(|_| invalid())?;
    let signature: Signature = signature.parse().map_err(|_| invalid())?;
    let digest: [u8; 32] = Sha256::digest(format!("nostr:agent-auth:{agent}:")).into();
    Secp256k1::verification_only()
        .verify_schnorr(&signature, &digest, &owner)
        .map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(dir: &Path, records: Value) {
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(dir.join(LIBRARY), records.to_string()).unwrap();
    }

    #[test]
    fn staging_adds_attestations_without_touching_the_source() {
        let source = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        library(
            source.path(),
            serde_json::json!([
                { "pubkey": a, "name": "A" },
                { "pubkey": b, "name": "B", "auth_tag": "kept" },
            ]),
        );
        let original = std::fs::read(source.path().join(LIBRARY)).unwrap();
        let (digest, unattested) = scan(source.path()).unwrap();
        assert_eq!(unattested, BTreeSet::from([a.clone()]));

        let tags = BTreeMap::from([(a.clone(), "new".into()), (b.clone(), "ignored".into())]);
        stage(source.path(), staging.path(), &digest, &tags).unwrap();
        let staged: Vec<Value> =
            serde_json::from_slice(&std::fs::read(staging.path().join(LIBRARY)).unwrap()).unwrap();
        assert_eq!(staged[0]["auth_tag"], "new");
        assert_eq!(staged[1]["auth_tag"], "kept", "an existing tag wins");
        assert_eq!(
            std::fs::read(source.path().join(LIBRARY)).unwrap(),
            original
        );

        library(source.path(), serde_json::json!([]));
        assert!(stage(source.path(), staging.path(), &digest, &tags).is_err());
    }

    #[test]
    fn attestations_must_sign_the_agent_key() {
        use secp256k1::{Keypair, Secp256k1};
        let secp = Secp256k1::new();
        let owner = Keypair::new(&secp, &mut secp256k1::rand::rng());
        let agent = "c".repeat(64);
        let sign = |agent: &str| {
            let digest: [u8; 32] = Sha256::digest(format!("nostr:agent-auth:{agent}:")).into();
            serde_json::to_string(&[
                "auth",
                &owner.x_only_public_key().0.to_string(),
                "",
                &secp.sign_schnorr_no_aux_rand(&digest, &owner).to_string(),
            ])
            .unwrap()
        };
        verify_attestation(&sign(&agent), &agent).unwrap();
        assert!(verify_attestation(&sign(&"d".repeat(64)), &agent).is_err());
        assert!(verify_attestation("[\"auth\"]", &agent).is_err());
    }
}
