//! Saved agent teams (`channel-templates/model.ts`, `team-payload.ts`):
//! private kind-30078 records encrypted to the viewer, one per team, in
//! the `buzz-channel-kit-v1` family. A portable team (record version 2)
//! also names a manifest of its full snapshot, stored as separate
//! 16 KiB payload chunks. buzz-app's native admission
//! (`buzz_desktop::relay::kit`) fixes each record's coordinate and tags.

use crate::event::Verified;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use buzz_desktop::relay::kit;
use buzz_identity::Identity;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub use kit::{KIT_TAG, MANIFEST_TAG, ME_KIT_TAG, PAYLOAD_TAG};

pub const TEAM_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
pub const TEAM_CHUNK_BYTES: usize = 16 * 1024;
/// Encrypted records larger than this are refused before decrypting.
const MAX_CIPHERTEXT: usize = 64 * 1024;
const MAX_PLAINTEXT: usize = 48 * 1024;

/// The manifest of a portable team's saved snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamManifest {
    pub version: u64,
    pub owner: String,
    pub revision: String,
    pub digest: String,
    pub bytes: u64,
    pub chunks: u64,
}

/// One saved team. `agents` are member pubkeys, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub agents: Vec<String>,
    pub portable: Option<TeamManifest>,
}

/// A team record as read: the relay head of its coordinate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamEntry {
    pub team: Team,
    pub deleted: bool,
    pub event_id: String,
    pub created_at: u64,
}

/// A team's current head for the host's binding reconciliation
/// (`current_team_members`). Deleted teams have no members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamHead {
    pub created_at: u64,
    pub event_id: String,
    pub members: Vec<String>,
}

/// `encodeURIComponent` of an HTTP origin.
pub fn encode_community(community: &str) -> String {
    community
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Whether a record coordinate belongs to `community`'s recipe catalog.
pub fn in_catalog(d: &str, community: &str) -> bool {
    let encoded = encode_community(community);
    [KIT_TAG, MANIFEST_TAG, ME_KIT_TAG]
        .iter()
        .any(|tag| d.starts_with(&format!("{tag}:{encoded}:")))
}

/// The plaintext record of a team.
pub fn team_record(team: &Team, community: &str, deleted: bool) -> Value {
    let mut value = json!({
        "type": "team",
        "id": team.id,
        "name": team.name,
        "agents": team.agents,
    });
    if let Some(manifest) = &team.portable {
        value["portable"] = json!(manifest);
    }
    json!({
        "version": if team.portable.is_some() { 2 } else { 1 },
        "community": community,
        "deleted": deleted,
        "value": value,
    })
}

/// The `t` tag a record is filed under.
pub fn record_tag(raw: &Value) -> &'static str {
    if raw["value"]["type"] == "team-payload" {
        PAYLOAD_TAG
    } else if raw["version"] == 2 {
        MANIFEST_TAG
    } else if (raw["value"]["type"] == "groups" && raw["value"]["id"] == "me")
        || (raw["value"]["type"] == "template"
            && raw["value"]["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("me-section-")))
    {
        ME_KIT_TAG
    } else {
        KIT_TAG
    }
}

/// The `d` coordinate and `t` tag a record is stored under, after
/// validating it for `community`.
pub fn record_tags(raw: &Value, community: &str) -> Result<Vec<Vec<String>>, String> {
    let coordinate = kit::coordinate(raw, community)?;
    Ok(vec![
        vec!["d".into(), coordinate],
        vec!["t".into(), record_tag(raw).into()],
    ])
}

/// A team from a validated record, `None` for other record types.
pub fn parse_team(raw: &Value) -> Option<(Team, bool)> {
    let value = &raw["value"];
    if value["type"] != "team" {
        return None;
    }
    let portable = match value.get("portable") {
        Some(manifest) => Some(serde_json::from_value(manifest.clone()).ok()?),
        None => None,
    };
    Some((
        Team {
            id: value["id"].as_str()?.to_owned(),
            name: value["name"].as_str()?.to_owned(),
            agents: serde_json::from_value(value["agents"].clone()).ok()?,
            portable,
        },
        raw["deleted"] == true,
    ))
}

/// Decrypts and admits one of the viewer's own records
/// (`validate_ciphertext`): the record must name this community, match
/// the event's tags, and a portable one must be the viewer's.
pub fn decode_record(
    identity: &Identity,
    event: &Verified,
    community: &str,
) -> Result<Value, String> {
    let invalid = || "Invalid channel recipe".to_string();
    if event.kind_u16() != 30078
        || event.pubkey != identity.public_key()
        || event.content.len() > MAX_CIPHERTEXT
    {
        return Err(invalid());
    }
    let plaintext = identity.decrypt_from_self(event).map_err(|_| invalid())?;
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(invalid());
    }
    let raw: Value = serde_json::from_str(&plaintext).map_err(|_| invalid())?;
    let tags: Vec<Vec<String>> = event
        .tags
        .iter()
        .map(|tag| tag.as_slice().to_vec())
        .collect();
    kit::admission(&json!({ "tags": tags }), &raw, community)?;
    if raw["value"]["type"] == "team-payload" || raw["version"] == 2 {
        let owner = if raw["version"] == 2 {
            &raw["value"]["portable"]["owner"]
        } else {
            &raw["value"]["owner"]
        };
        if owner.as_str() != Some(identity.pubkey_hex().as_str()) {
            return Err("Portable team belongs to another viewer".into());
        }
    }
    Ok(raw)
}

/// The newest head per coordinate (`selectedHead`): newer `created_at`,
/// then the lower id.
pub fn newest<'a>(events: impl IntoIterator<Item = &'a Verified>) -> Option<&'a Verified> {
    events.into_iter().min_by(|a, b| {
        b.created_at_secs()
            .cmp(&a.created_at_secs())
            .then_with(|| a.id.to_hex().cmp(&b.id.to_hex()))
    })
}

/// Only portable configuration is stored; memories are restored apart.
pub fn without_memories(snapshot: &Value) -> Value {
    let mut snapshot = snapshot.clone();
    if let Some(members) = snapshot["members"].as_array_mut() {
        for member in members {
            member["memory"] = json!({ "level": "none", "entries": [] });
        }
    }
    snapshot
}

fn hex_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
        && value.len() == 36
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-'))
}

pub fn check_manifest(manifest: &TeamManifest) -> Result<(), String> {
    let chunks = (manifest.bytes as usize).div_ceil(TEAM_CHUNK_BYTES) as u64;
    if manifest.version != 1
        || !hex_key(&manifest.owner)
        || !uuid(&manifest.revision)
        || !hex_key(&manifest.digest)
        || manifest.bytes < 1
        || manifest.bytes as usize > TEAM_PAYLOAD_BYTES
        || manifest.chunks != chunks
    {
        return Err("Invalid portable team manifest".into());
    }
    Ok(())
}

/// The plaintext payload record of chunk `index`.
pub fn payload_record(
    community: &str,
    owner: &str,
    team: &str,
    revision: &str,
    index: usize,
    data: String,
) -> Value {
    json!({
        "version": 1,
        "community": community,
        "deleted": false,
        "value": {
            "type": "team-payload",
            "id": format!("{revision}-{index}"),
            "version": 1,
            "community": community,
            "owner": owner,
            "teamId": team,
            "revision": revision,
            "index": index,
            "data": data,
        },
    })
}

/// Splits a team snapshot, without memories, into its manifest and
/// payload records.
pub fn encode_payload(
    snapshot: &Value,
    community: &str,
    owner: &str,
    team: &str,
    revision: &str,
) -> Result<(TeamManifest, Vec<Value>), String> {
    let bytes = serde_json::to_vec(&without_memories(snapshot)).map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > TEAM_PAYLOAD_BYTES {
        return Err("Team snapshot exceeds the size limit".into());
    }
    let manifest = TeamManifest {
        version: 1,
        owner: owner.to_owned(),
        revision: revision.to_owned(),
        digest: format!("{:x}", Sha256::digest(&bytes)),
        bytes: bytes.len() as u64,
        chunks: bytes.len().div_ceil(TEAM_CHUNK_BYTES) as u64,
    };
    check_manifest(&manifest)?;
    let payloads = bytes
        .chunks(TEAM_CHUNK_BYTES)
        .enumerate()
        .map(|(index, chunk)| {
            payload_record(
                community,
                owner,
                team,
                revision,
                index,
                STANDARD.encode(chunk),
            )
        })
        .collect();
    Ok((manifest, payloads))
}

/// Joins and checks a portable team's payload records (each the
/// `value` of a decoded record) back into its snapshot.
pub fn decode_payload(
    manifest: &TeamManifest,
    payloads: &[Value],
    community: &str,
    owner: &str,
    team: &str,
) -> Result<Value, String> {
    check_manifest(manifest)?;
    if manifest.owner != owner || payloads.len() as u64 != manifest.chunks {
        return Err("Incomplete portable team payload".into());
    }
    let mut bytes = Vec::with_capacity(manifest.bytes as usize);
    for (index, payload) in payloads.iter().enumerate() {
        if payload["community"] != community
            || payload["owner"] != owner
            || payload["teamId"] != team
            || payload["revision"] != manifest.revision.as_str()
            || payload["index"].as_u64() != Some(index as u64)
        {
            return Err("Portable team revision mismatch".into());
        }
        let data = payload["data"].as_str().unwrap_or_default();
        let chunk = STANDARD
            .decode(data)
            .map_err(|_| "Invalid portable team chunk size".to_string())?;
        let expected = TEAM_CHUNK_BYTES.min(manifest.bytes as usize - index * TEAM_CHUNK_BYTES);
        if chunk.len() != expected {
            return Err("Invalid portable team chunk size".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if format!("{:x}", Sha256::digest(&bytes)) != manifest.digest {
        return Err("Portable team integrity check failed".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid portable team payload".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMUNITY: &str = "https://relay.test";

    fn owner() -> String {
        crate::testing::key(1)
    }

    fn snapshot(prompt: &str) -> Value {
        json!({
            "format": "buzz-team-snapshot",
            "version": 1,
            "team": { "name": "Crew", "instructions": "Work together." },
            "members": [{
                "format": "buzz-agent-snapshot",
                "version": 1,
                "definition": { "name": "Scout", "systemPrompt": prompt },
                "profile": { "displayName": "Scout" },
                "memory": { "level": "core", "entries": [{ "slug": "core", "body": "secret" }] },
            }],
        })
    }

    #[test]
    fn records_use_the_native_coordinates() {
        let mut team = Team {
            id: "crew".into(),
            name: "Crew".into(),
            agents: vec![owner()],
            portable: None,
        };
        let raw = team_record(&team, COMMUNITY, false);
        let tags = record_tags(&raw, COMMUNITY).unwrap();
        assert_eq!(
            tags[0][1],
            "buzz-channel-kit-v1:https%3A%2F%2Frelay.test:team:crew"
        );
        assert_eq!(tags[1][1], KIT_TAG);
        assert!(in_catalog(&tags[0][1], COMMUNITY));
        assert!(!in_catalog(&tags[0][1], "https://other.test"));
        assert_eq!(parse_team(&raw), Some((team.clone(), false)));
        let (manifest, _) = encode_payload(
            &snapshot("Find"),
            COMMUNITY,
            &owner(),
            "crew",
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
        team.portable = Some(manifest);
        let raw = team_record(&team, COMMUNITY, true);
        let tags = record_tags(&raw, COMMUNITY).unwrap();
        assert!(tags[0][1].starts_with("buzz-channel-kit-v2:"));
        assert_eq!(parse_team(&raw), Some((team, true)));
        // A version-2 record needs members.
        let empty = Team {
            id: "crew".into(),
            name: "Crew".into(),
            agents: vec![],
            portable: parse_team(&raw).unwrap().0.portable,
        };
        assert!(record_tags(&team_record(&empty, COMMUNITY, false), COMMUNITY).is_err());
    }

    #[test]
    fn payloads_round_trip_without_memories() {
        let revision = uuid::Uuid::new_v4().to_string();
        // Big enough for three chunks.
        let prompt = "x".repeat(40_000);
        let (manifest, payloads) =
            encode_payload(&snapshot(&prompt), COMMUNITY, &owner(), "crew", &revision).unwrap();
        assert_eq!(manifest.chunks, 3);
        assert_eq!(payloads.len(), 3);
        for payload in &payloads {
            let tags = record_tags(payload, COMMUNITY).unwrap();
            assert!(tags[0][1].starts_with("buzz-team-payload-v1:"));
            assert_eq!(tags[1][1], PAYLOAD_TAG);
        }
        let values: Vec<Value> = payloads.iter().map(|p| p["value"].clone()).collect();
        let decoded = decode_payload(&manifest, &values, COMMUNITY, &owner(), "crew").unwrap();
        assert_eq!(decoded["members"][0]["memory"]["level"], "none");
        assert_eq!(
            decoded["members"][0]["definition"]["systemPrompt"],
            prompt.as_str()
        );
        // A missing chunk, another team or a tampered chunk is refused.
        assert!(decode_payload(&manifest, &values[..2], COMMUNITY, &owner(), "crew").is_err());
        assert_eq!(
            decode_payload(&manifest, &values, COMMUNITY, &owner(), "other").unwrap_err(),
            "Portable team revision mismatch"
        );
        let mut tampered = values.clone();
        let data = tampered[2]["data"].as_str().unwrap().replacen('e', "f", 1);
        tampered[2]["data"] = json!(data);
        assert!(decode_payload(&manifest, &tampered, COMMUNITY, &owner(), "crew").is_err());
    }

    #[test]
    fn records_decrypt_only_for_their_owner_and_tags() {
        let identity = Identity::ephemeral();
        let team = Team {
            id: "crew".into(),
            name: "Crew".into(),
            agents: vec![owner()],
            portable: None,
        };
        let raw = team_record(&team, COMMUNITY, false);
        let mut tags = record_tags(&raw, COMMUNITY).unwrap();
        let sign = |tags: Vec<Vec<String>>| {
            let content = identity.encrypt_to_self(&raw.to_string()).unwrap();
            let unsigned = identity
                .template(buzz_identity::Purpose::AppData, 30078, 1, tags, content)
                .unwrap();
            let event = identity
                .sign(buzz_identity::Purpose::AppData, unsigned)
                .unwrap();
            crate::event::Verifier::default().verify(event).unwrap()
        };
        let event = sign(tags.clone());
        assert_eq!(decode_record(&identity, &event, COMMUNITY).unwrap(), raw);
        assert!(decode_record(&identity, &event, "https://other.test").is_err());
        assert!(decode_record(&Identity::ephemeral(), &event, COMMUNITY).is_err());
        tags[1][1] = MANIFEST_TAG.into();
        assert!(decode_record(&identity, &sign(tags), COMMUNITY).is_err());
    }
}
