//! Portable team payloads (`features/channel-templates/team-payload.ts`):
//! a team snapshot of up to 8 MiB split into 16 KiB chunks, each its own
//! private 30078 record, bound to a manifest by owner, team, revision,
//! index and a SHA-256 digest. A team's shared instructions are stored
//! the same way, as `v1:` and their UTF-8 bytes.

use super::{hex_key, identifier};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const TEAM_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const TEAM_CHUNK_BYTES: usize = 16 * 1024;
pub const TEAM_CHUNKS: usize = TEAM_PAYLOAD_BYTES / TEAM_CHUNK_BYTES;

const INVALID_MANIFEST: &str = "Invalid portable team manifest";
const INVALID_PAYLOAD: &str = "Invalid portable team payload";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamManifest {
    pub version: u8,
    pub owner: String,
    pub revision: String,
    pub digest: String,
    pub bytes: u64,
    pub chunks: u64,
}

/// One chunk of a portable team.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPayload {
    pub version: u8,
    pub community: String,
    pub owner: String,
    pub team_id: String,
    pub revision: String,
    pub index: u64,
    pub data: String,
}

impl TeamPayload {
    /// The record that stores this chunk (`PayloadRecord`).
    pub fn record(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("payloads serialize");
        value["type"] = json!("team-payload");
        value["id"] = json!(format!("{}-{}", self.revision, self.index));
        json!({"version": 1, "community": self.community, "deleted": false, "value": value})
    }

    pub fn coordinate(&self) -> String {
        payload_coordinate(
            &self.community,
            &self.owner,
            &self.team_id,
            &self.revision,
            self.index,
        )
    }
}

/// A lowercase UUID revision.
pub fn revision(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => matches!(b, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `parseTeamManifest`.
pub fn parse_team_manifest(raw: &Value) -> Result<TeamManifest, String> {
    let bytes = raw["bytes"].as_u64().unwrap_or(0);
    let valid = raw["version"] == 1
        && raw["owner"].as_str().is_some_and(hex_key)
        && raw["revision"].as_str().is_some_and(revision)
        && raw["digest"].as_str().is_some_and(hex_key)
        && (1..=TEAM_PAYLOAD_BYTES as u64).contains(&bytes)
        && raw["chunks"].as_u64() == Some(bytes.div_ceil(TEAM_CHUNK_BYTES as u64));
    if !valid {
        return Err(INVALID_MANIFEST.into());
    }
    Ok(TeamManifest {
        version: 1,
        owner: raw["owner"].as_str().unwrap_or_default().to_owned(),
        revision: raw["revision"].as_str().unwrap_or_default().to_owned(),
        digest: raw["digest"].as_str().unwrap_or_default().to_owned(),
        bytes,
        chunks: bytes.div_ceil(TEAM_CHUNK_BYTES as u64),
    })
}

/// `parseTeamPayload`.
pub fn parse_team_payload(raw: &Value, community: &str) -> Result<TeamPayload, String> {
    let index = raw["index"].as_u64();
    let data = raw["data"].as_str().unwrap_or_default();
    let valid = raw["version"] == 1
        && raw["community"].as_str() == Some(community)
        && raw["owner"].as_str().is_some_and(hex_key)
        && raw["teamId"].as_str().is_some_and(identifier)
        && raw["revision"].as_str().is_some_and(revision)
        && index.is_some_and(|index| index < TEAM_CHUNKS as u64)
        && raw["data"].is_string()
        && data.len() <= TEAM_CHUNK_BYTES.div_ceil(3) * 4
        && STANDARD
            .decode(data)
            .is_ok_and(|bytes| STANDARD.encode(bytes) == data);
    if !valid {
        return Err(INVALID_PAYLOAD.into());
    }
    Ok(TeamPayload {
        version: 1,
        community: community.to_owned(),
        owner: raw["owner"].as_str().unwrap_or_default().to_owned(),
        team_id: raw["teamId"].as_str().unwrap_or_default().to_owned(),
        revision: raw["revision"].as_str().unwrap_or_default().to_owned(),
        index: index.unwrap_or_default(),
        data: data.to_owned(),
    })
}

/// `parsePayloadRecord`: the chunk record, its id bound to its position.
pub fn parse_payload_record(raw: &Value, community: &str) -> Result<TeamPayload, String> {
    let value = &raw["value"];
    if raw["version"] != 1
        || raw["community"].as_str() != Some(community)
        || raw["deleted"] != false
        || value["type"] != "team-payload"
    {
        return Err("Invalid team payload record".into());
    }
    let payload = parse_team_payload(value, community)?;
    if value["id"].as_str() != Some(format!("{}-{}", payload.revision, payload.index).as_str()) {
        return Err("Invalid team payload ID".into());
    }
    // The native boundary accepts exactly these fields.
    buzz_desktop::relay::kit::coordinate(raw, community)?;
    Ok(payload)
}

pub fn payload_coordinate(
    community: &str,
    owner: &str,
    team_id: &str,
    revision: &str,
    index: u64,
) -> String {
    format!(
        "{}:{}:{owner}:{team_id}:{revision}:{index}",
        super::PAYLOAD_TAG,
        super::encode_component(community)
    )
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// `withoutTeamMemories`: only portable configuration is persisted.
pub fn without_team_memories(snapshot: &Value) -> Value {
    let mut snapshot = snapshot.clone();
    if let Some(members) = snapshot.get_mut("members").and_then(Value::as_array_mut) {
        for member in members {
            if let Some(member) = member.as_object_mut() {
                member.insert("memory".into(), json!({"level": "none", "entries": []}));
            }
        }
    }
    snapshot
}

/// `encodePayloadBytes`: splits bytes into immutable digest-bound chunks
/// for one revision.
fn encode_payload_bytes(
    bytes: &[u8],
    community: &str,
    owner: &str,
    team_id: &str,
    revision: &str,
    parse_manifest: fn(&Value) -> Result<TeamManifest, String>,
) -> Result<(TeamManifest, Vec<TeamPayload>), String> {
    let manifest = parse_manifest(&json!({
        "version": 1,
        "owner": owner,
        "revision": revision,
        "digest": digest(bytes),
        "bytes": bytes.len(),
        "chunks": bytes.len().div_ceil(TEAM_CHUNK_BYTES),
    }))?;
    let payloads = bytes
        .chunks(TEAM_CHUNK_BYTES)
        .enumerate()
        .map(|(index, chunk)| {
            parse_team_payload(
                &json!({
                    "version": 1,
                    "community": community,
                    "owner": owner,
                    "teamId": team_id,
                    "revision": revision,
                    "index": index,
                    "data": STANDARD.encode(chunk),
                }),
                community,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((manifest, payloads))
}

/// `decodePayloadBytes`: checks every immutable binding, exact chunk
/// length and the digest before returning the bytes.
fn decode_payload_bytes(
    manifest: &TeamManifest,
    payloads: &[TeamPayload],
    community: &str,
    owner: &str,
    team_id: &str,
) -> Result<Vec<u8>, String> {
    if manifest.owner != owner || payloads.len() as u64 != manifest.chunks {
        return Err("Incomplete portable team payload".into());
    }
    let mut bytes = Vec::with_capacity(manifest.bytes as usize);
    for (index, payload) in payloads.iter().enumerate() {
        let payload = parse_team_payload(
            &serde_json::to_value(payload).unwrap_or_default(),
            community,
        )?;
        if payload.owner != owner
            || payload.team_id != team_id
            || payload.revision != manifest.revision
            || payload.index != index as u64
        {
            return Err("Portable team revision mismatch".into());
        }
        let chunk = STANDARD
            .decode(&payload.data)
            .map_err(|_| "Invalid portable team chunk size".to_owned())?;
        let expected = TEAM_CHUNK_BYTES.min(manifest.bytes as usize - index * TEAM_CHUNK_BYTES);
        if chunk.len() != expected {
            return Err("Invalid portable team chunk size".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if digest(&bytes) != manifest.digest {
        return Err("Portable team integrity check failed".into());
    }
    Ok(bytes)
}

/// `encodeTeamPayload`: the manifest and its chunks.
pub fn encode_team_payload(
    snapshot: &Value,
    community: &str,
    owner: &str,
    team_id: &str,
    revision: &str,
) -> Result<(TeamManifest, Vec<TeamPayload>), String> {
    let bytes = serde_json::to_vec(&without_team_memories(snapshot))
        .map_err(|_| "Team snapshot exceeds the size limit".to_owned())?;
    if bytes.is_empty() || bytes.len() > TEAM_PAYLOAD_BYTES {
        return Err("Team snapshot exceeds the size limit".into());
    }
    encode_payload_bytes(
        &bytes,
        community,
        owner,
        team_id,
        revision,
        parse_team_manifest,
    )
}

/// `decodeTeamPayload`: every binding checked before the JSON is parsed.
pub fn decode_team_payload(
    manifest: &TeamManifest,
    payloads: &[TeamPayload],
    community: &str,
    owner: &str,
    team_id: &str,
) -> Result<Value, String> {
    let manifest = parse_team_manifest(&serde_json::to_value(manifest).unwrap_or_default())?;
    let bytes = decode_payload_bytes(&manifest, payloads, community, owner, team_id)?;
    let text =
        String::from_utf8(bytes).map_err(|_| "Portable team integrity check failed".to_owned())?;
    serde_json::from_str(&text).map_err(|_| "Portable team integrity check failed".into())
}

/// Largest team instructions, in UTF-8 bytes (`TEAM_TEXT_BYTES`). They
/// travel as ASCII `v1:` plus their raw UTF-8 bytes.
pub const TEAM_TEXT_BYTES: usize = 128 * 1024;
const TEXT_PREFIX: &str = "v1:";

/// `parseTextManifest`: a team manifest with exactly its six fields, as
/// native admission requires, sized for the prefix and 128 KiB of text.
pub fn parse_text_manifest(raw: &Value) -> Result<TeamManifest, String> {
    let manifest = parse_team_manifest(raw)?;
    if raw.as_object().map_or(0, |fields| fields.len()) != 6
        || manifest.bytes < TEXT_PREFIX.len() as u64
        || manifest.bytes > (TEXT_PREFIX.len() + TEAM_TEXT_BYTES) as u64
    {
        return Err("Invalid team instructions manifest".into());
    }
    Ok(manifest)
}

/// `teamTextBytes`: no NUL and at most 128 KiB before any trimming. A
/// Rust string is always valid UTF-8.
pub fn team_text_bytes(text: &str) -> Result<&[u8], String> {
    if text.contains('\0') {
        return Err("Team instructions cannot contain NUL characters".into());
    }
    if text.len() > TEAM_TEXT_BYTES {
        return Err("Team instructions exceed 128 KiB".into());
    }
    Ok(text.as_bytes())
}

/// `encodeTeamText`: the text's manifest and chunks for one revision.
pub fn encode_team_text(
    text: &str,
    community: &str,
    owner: &str,
    team_id: &str,
    revision: &str,
) -> Result<(TeamManifest, Vec<TeamPayload>), String> {
    let body = team_text_bytes(text)?;
    let mut bytes = Vec::with_capacity(TEXT_PREFIX.len() + body.len());
    bytes.extend_from_slice(TEXT_PREFIX.as_bytes());
    bytes.extend_from_slice(body);
    encode_payload_bytes(
        &bytes,
        community,
        owner,
        team_id,
        revision,
        parse_text_manifest,
    )
}

/// `decodeTeamText`: the text of a manifest's chunks, with every binding
/// and the digest checked first.
pub fn decode_team_text(
    manifest: &TeamManifest,
    payloads: &[TeamPayload],
    community: &str,
    owner: &str,
    team_id: &str,
) -> Result<String, String> {
    let manifest = parse_text_manifest(&serde_json::to_value(manifest).unwrap_or_default())?;
    let bytes = decode_payload_bytes(&manifest, payloads, community, owner, team_id)?;
    let decoded =
        String::from_utf8(bytes).map_err(|_| "Portable team integrity check failed".to_owned())?;
    let text = decoded
        .strip_prefix(TEXT_PREFIX)
        .ok_or("Unsupported team instructions format")?;
    team_text_bytes(text)?;
    Ok(text.to_owned())
}

/// The manifest's chunk coordinates, in order.
pub fn chunk_coordinates(manifest: &TeamManifest, community: &str, team_id: &str) -> Vec<String> {
    (0..manifest.chunks)
        .map(|index| {
            payload_coordinate(
                community,
                &manifest.owner,
                team_id,
                &manifest.revision,
                index,
            )
        })
        .collect()
}
