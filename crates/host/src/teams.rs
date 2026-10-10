//! Team requests (buzz-app's `agent_control_team_*` commands and the team
//! half of create): portable team snapshots of saved agents, team file
//! previews, team instructions before a deploy, and creating the members
//! of an imported team. The GUI reads and saves the team records with
//! the human key and passes the owner's current team heads in.

use crate::api::{Request, Response};
use crate::{AgentHost, failed, prepared_response};
use buzz_agent_controller::{BundleMember, NewAgent, TeamCatalogEntry, TeamMeta, TeamSnapshot};
use buzz_host_rpc::RpcError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Converts through JSON field names, the controller's own contract.
fn convert<T: DeserializeOwned>(value: &impl Serialize, what: &str) -> Result<T, String> {
    let json = serde_json::to_value(value).map_err(|_| format!("Invalid {what}"))?;
    serde_json::from_value(json).map_err(|error| format!("Invalid {what}: {error}"))
}

/// The agents' relay for a community origin (`https://` or `wss://`).
fn relay(community: &str) -> String {
    community
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
}

impl AgentHost {
    pub(crate) async fn team_request(&self, request: Request) -> Result<Response, RpcError> {
        match request {
            Request::TeamCapture {
                team,
                members,
                community,
            } => {
                self.run(move |owner| {
                    let team: TeamMeta = convert(&team, "team")?;
                    let snapshot =
                        owner
                            .controller
                            .export_team(team, &members, &relay(&community))?;
                    convert(&snapshot, "team snapshot").map(Response::TeamSnapshot)
                })
                .await
            }
            Request::TeamPreview { content } => TeamSnapshot::decode(content.as_bytes())
                .and_then(|snapshot| convert(&snapshot, "team snapshot"))
                .map(Response::TeamSnapshot)
                .map_err(RpcError::Failed),
            Request::TeamInstructions {
                id,
                revision,
                instructions,
                team,
                community,
                owner: human,
                teams,
            } => {
                self.run(move |owner| {
                    let teams: BTreeMap<String, TeamCatalogEntry> =
                        convert(&teams, "team catalog")?;
                    let agent = owner
                        .controller
                        .snapshot()?
                        .agents
                        .into_iter()
                        .find(|agent| agent.id == id)
                        .ok_or("Agent no longer exists")?;
                    if !teams
                        .get(&team)
                        .is_some_and(|head| head.members.contains(&agent.pubkey))
                    {
                        return Err(
                            "Team no longer contains this member; refresh before deploying".into(),
                        );
                    }
                    owner
                        .controller
                        .reconcile_team_bindings(&community, &human, &teams)?;
                    owner.controller.apply_team_instructions(
                        &id,
                        revision,
                        &instructions,
                        &human,
                        (&team, &community),
                    )?;
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            Request::TeamMemberPrepare {
                request_id,
                relay_url,
                owner: human,
            } => {
                self.run(move |owner| {
                    if uuid::Uuid::parse_str(&request_id).is_err() {
                        return Err("Invalid create request".into());
                    }
                    if let Some(agent) =
                        owner
                            .controller
                            .created_request(&request_id, &relay_url, &human)?
                    {
                        return Ok(Response::TeamMemberPrepared {
                            id: agent.id,
                            pubkey: agent.pubkey,
                            saved: true,
                        });
                    }
                    if let Some((id, prepared)) = &owner.creating
                        && *id == request_id
                    {
                        if !prepared.matches(&relay_url, &human)? {
                            return Err("Create destination or owner changed".into());
                        }
                    } else {
                        let prepared = Arc::new(NewAgent::prepare(&relay_url, &human)?);
                        owner.creating = Some((request_id, prepared));
                    }
                    let (_, prepared) = owner.creating.as_ref().ok_or("Create request expired")?;
                    let Response::Prepared { id, pubkey, .. } = prepared_response(prepared) else {
                        unreachable!("prepared responses are Prepared")
                    };
                    Ok(Response::TeamMemberPrepared {
                        id,
                        pubkey,
                        saved: false,
                    })
                })
                .await
            }
            Request::TeamMemberCommit {
                request_id,
                edit,
                auth,
                bundle,
            } => {
                self.run(move |owner| {
                    let bundle: BundleMember = convert(&bundle, "team member")?;
                    bundle.validate()?;
                    let prepared = owner
                        .creating
                        .as_ref()
                        .filter(|(id, _)| *id == request_id)
                        .map(|(_, prepared)| prepared.clone())
                        .ok_or("Create request expired; start the import again")?;
                    let edit = owner.draft(&edit)?;
                    prepared.validate(edit.clone(), &auth)?;
                    prepared.save_key(owner.credentials.as_ref())?;
                    owner.controller.create_bundle_member(
                        &prepared,
                        edit,
                        &auth,
                        &request_id,
                        &bundle,
                    )?;
                    owner.creating = None;
                    owner.snapshot().map(Response::Agents)
                })
                .await
            }
            _ => Err(failed("Not a team request")),
        }
    }
}
