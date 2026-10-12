//! Saved agent teams (`channel-templates/model.ts`, `team-payload.ts`):
//! private kind-30078 records encrypted to the viewer, one per team, in
//! the `buzz-channel-kit-v1` family. A portable team (record version 2)
//! also names a manifest of its full snapshot, stored as separate
//! 16 KiB payload chunks. buzz-app's native admission
//! (`buzz_desktop::relay::kit`) fixes each record's coordinate and tags.
//! The payload, its manifest and the record cipher live in [`crate::kit`].

use buzz_desktop::relay::kit;
use serde_json::{Value, json};

pub use crate::kit::TeamManifest;
pub use kit::{KIT_TAG, MANIFEST_TAG, ME_KIT_TAG, PAYLOAD_TAG, TEXT_TAG};

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
    } else if raw["value"]["type"] == "team-text" {
        TEXT_TAG
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
        assert_eq!(parse_team(&raw), Some((team.clone(), false)));
        let (manifest, _) = crate::kit::payload::encode_team_payload(
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
}
