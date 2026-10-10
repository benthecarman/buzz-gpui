//! What the team dialogs do (`team-bundles.ts`, `team-import.ts`,
//! `team-deployment.ts`): import a team snapshot as fresh agents, capture
//! and export saved teams, and deploy a team to a channel.
//!
//! Agents may run on several hosts. A team's members are found on
//! whichever host runs them in this community; an import creates its
//! members on one host.

use super::catalog::community_agents;
use crate::community::{AppState, Community};
use crate::hosts::{HostConnection, Hosts};
use crate::runtime::Runtime;
use buzz_host::api::{
    Agent, AgentAction, AgentEdit, BundleMember, Harness, HarnessEdit, HarnessKind, MemberMemory,
    MemberSnapshot, MemoryEntry, MemoryRestore, ProcessStatus, Request, Response, TeamCatalogEntry,
    TeamMeta, TeamSnapshot,
};
use buzz_host_rpc::ConnectionState;
use buzz_identity::Identity;
use buzz_relay_client::Partition;
use buzz_relay_client::session::TeamKit;
use buzz_relay_client::teams::Team;
use gpui_kit::{AppContext, AsyncApp, Entity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const UNSUPPORTED: &str = "update it";

/// How much memory an export carries (`MemoryLevel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryLevel {
    None,
    Core,
    Everything,
}

impl MemoryLevel {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Core => "core",
            Self::Everything => "everything",
        }
    }
}

/// The harness kind a portable runtime name selects.
pub fn runtime_kind(runtime: &str) -> Option<HarnessKind> {
    match runtime {
        "buzz-agent" => Some(HarnessKind::BuzzAgent),
        "goose" => Some(HarnessKind::Goose),
        "pi" => Some(HarnessKind::Pi),
        "claude" => Some(HarnessKind::Claude),
        "hermes" => Some(HarnessKind::Hermes),
        _ => None,
    }
}

/// The create settings for one imported team member (`snapshotImportEdit`
/// with `teamMember`). Its runtime must be installed on the host; a preset
/// harness owns its provider, and Pi needs a model with a provider.
pub fn member_edit(member: &MemberSnapshot, harnesses: &[Harness]) -> Result<AgentEdit, String> {
    let d = &member.definition;
    let mut limits = Vec::new();
    if d.name != member.profile.display_name {
        limits.push("definition and profile names disagree");
    }
    if d.runtime
        .as_deref()
        .is_some_and(|r| runtime_kind(r).is_none())
    {
        limits.push("runtime (unsupported)");
    }
    if d.parallelism.is_some_and(|n| n > 32) {
        limits.push("parallelism (native supports 1–32 workers)");
    }
    if !limits.is_empty() {
        return Err(format!("Import is blocked: {}.", limits.join("; ")));
    }
    let runtime = d.runtime.as_deref().unwrap_or("buzz-agent");
    let kind = runtime_kind(runtime).expect("checked above");
    let command = harnesses
        .iter()
        .find(|harness| harness.kind == kind)
        .and_then(|harness| harness.command.clone())
        .ok_or_else(|| format!("{runtime} is unavailable. Install it in Settings first."))?;
    let model = d.model.clone().unwrap_or_default();
    let provider = d.provider.clone().unwrap_or_default();
    if matches!(kind, HarnessKind::Claude | HarnessKind::Hermes) && !provider.is_empty() {
        return Err("Import is blocked: external harness provider selector.".into());
    }
    if kind == HarnessKind::Pi && !provider.is_empty() && model.is_empty() {
        return Err("Import is blocked: Pi provider requires a model.".into());
    }
    Ok(AgentEdit {
        name: member.profile.display_name.clone(),
        picture: member
            .profile
            .avatar_data_url
            .clone()
            .or_else(|| member.profile.avatar_url.clone()),
        system_prompt: d.system_prompt.clone().unwrap_or_default(),
        session_policy: Some(Some(d.session_policy)),
        workspace: String::new(),
        harness: HarnessEdit {
            command,
            args: Vec::new(),
            model,
            provider,
            databricks: None,
        },
        environment: BTreeMap::new(),
        effort: d.effort.clone().map(Some),
    })
}

/// The resumable receipt of one import (`resumeTeamImport`): only public
/// operation IDs are kept, never instructions, snapshots or memories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub requests: Vec<String>,
}

/// Where an import's receipt is kept: the same destination, owner,
/// allowlist choice and snapshot resume the same import.
pub fn receipt_key(destination: &str, owner: &str, keep: bool, snapshot: &str) -> String {
    let input = serde_json::json!([destination, owner, "", keep, snapshot]).to_string();
    format!("buzz-team-import:{:x}", Sha256::digest(input))
}

fn uuid(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::parse_str(value).is_ok() && value == value.to_lowercase()
}

/// The saved receipt, or a new one for `members` members.
pub fn receipt(saved: Option<&str>, members: usize) -> Result<Receipt, String> {
    let receipt = match saved {
        Some(json) => serde_json::from_str::<Receipt>(json)
            .map_err(|_| "Saved team import receipt is invalid.".to_string())?,
        None => Receipt {
            id: uuid::Uuid::new_v4().to_string(),
            requests: (0..members)
                .map(|_| uuid::Uuid::new_v4().to_string())
                .collect(),
        },
    };
    if !uuid(&receipt.id)
        || receipt.requests.len() != members
        || !receipt.requests.iter().all(|r| uuid(r))
    {
        return Err("Saved team import receipt is invalid.".into());
    }
    Ok(receipt)
}

// ---------------------------------------------------------------------------
// Host requests

/// Runs a request on `host` and waits for its answer.
pub async fn request(
    host: &Entity<HostConnection>,
    request: Request,
    cx: &mut AsyncApp,
) -> Result<Response, String> {
    let task = host.update(cx, |host, cx| host.request(request, cx));
    task.await.map_err(|error| {
        let error = error.to_string();
        if error.contains(UNSUPPORTED) {
            format!("This host does not support teams yet. {error}")
        } else {
            error
        }
    })
}

/// The first connected host: the embedded one unless it is down.
pub fn import_host(cx: &gpui_kit::App) -> Option<Entity<HostConnection>> {
    cx.global::<Hosts>()
        .hosts
        .iter()
        .find(|host| matches!(host.read(cx).state, ConnectionState::Connected { .. }))
        .cloned()
}

/// Validates a team snapshot's JSON on `host` (`previewTeam`).
pub async fn preview(
    host: &Entity<HostConnection>,
    content: String,
    cx: &mut AsyncApp,
) -> Result<TeamSnapshot, String> {
    match request(host, Request::TeamPreview { content }, cx).await? {
        Response::TeamSnapshot(snapshot) => Ok(snapshot),
        _ => Err("The host sent an unexpected reply.".into()),
    }
}

/// The community's local agents with these keys, by host.
/// Team members grouped by the host that runs them.
type ByHost = Vec<(Entity<HostConnection>, Vec<String>)>;

fn by_host(
    community: &Entity<Community>,
    members: &[String],
    cx: &mut AsyncApp,
) -> Result<ByHost, String> {
    let agents = cx.update(|cx| community_agents(community, cx));
    let mut groups: ByHost = Vec::new();
    for pubkey in members {
        let (host, _) = agents
            .iter()
            .find(|(_, agent)| agent.pubkey == *pubkey)
            .ok_or("A team member is unavailable")?;
        match groups
            .iter_mut()
            .find(|(known, _)| known.entity_id() == host.entity_id())
        {
            Some((_, keys)) => keys.push(pubkey.clone()),
            None => groups.push((host.clone(), vec![pubkey.clone()])),
        }
    }
    Ok(groups)
}

/// Portable snapshots of the members' current saved settings, in order
/// (`captureTeam`), asked of whichever hosts run them.
pub async fn capture(
    community: &Entity<Community>,
    team: TeamMeta,
    members: &[String],
    cx: &mut AsyncApp,
) -> Result<TeamSnapshot, String> {
    let origin = cx.read_entity(community, |community, _| community.origin.http());
    let mut captured: HashMap<String, MemberSnapshot> = HashMap::new();
    for (host, keys) in by_host(community, members, cx)? {
        let response = request(
            &host,
            Request::TeamCapture {
                team: team.clone(),
                members: keys.clone(),
                community: origin.clone(),
            },
            cx,
        )
        .await?;
        let Response::TeamSnapshot(snapshot) = response else {
            return Err("The host sent an unexpected reply.".into());
        };
        if snapshot.members.len() != keys.len() {
            return Err("Portable team members do not match their definitions".into());
        }
        captured.extend(keys.into_iter().zip(snapshot.members));
    }
    Ok(TeamSnapshot {
        format: "buzz-team-snapshot".into(),
        version: 1,
        team,
        members: members
            .iter()
            .map(|key| captured.remove(key).ok_or("A team member is unavailable"))
            .collect::<Result<_, _>>()?,
    })
}

// ---------------------------------------------------------------------------
// Import

pub struct ImportOptions {
    /// The community's agent relay, `wss://…`.
    pub destination: String,
    pub owner: String,
    pub keep_allowlist: bool,
    pub restore_memory: bool,
}

pub struct Imported {
    /// The new team's ID.
    pub id: String,
    /// Members whose memories did not restore, as shown to the user.
    pub memory_failures: Vec<String>,
}

/// Reads the memories `agent` wrote to this identity, on tokio.
async fn read_memories(
    agent: &Agent,
    identity: &Arc<Identity>,
    cx: &mut AsyncApp,
) -> Result<crate::snapshot::memory::Listing, String> {
    let origin = buzz_relay_client::RelayOrigin::parse(&agent.relay_url)
        .map_err(|_| "Invalid agent community".to_string())?;
    let (identity, pubkey) = (identity.clone(), agent.pubkey.clone());
    cx.update(|cx| Runtime::handle(cx))
        .spawn(async move { crate::snapshot::memory::read(origin, identity, &pubkey).await })
        .await
        .unwrap_or_else(|_| Err("The request was cancelled.".into()))
}

/// Restores an imported member's snapshot memories under its new
/// identity (`writeSnapshotMemory`), and confirms each entry by reading
/// it back with the owner key. Answers how many are confirmed, and why
/// the others are not.
async fn restore_member_memory(
    host: &Entity<HostConnection>,
    agent: &Agent,
    entries: &[MemoryEntry],
    identity: &Arc<Identity>,
    cx: &mut AsyncApp,
) -> Result<(usize, Vec<String>), String> {
    let previous = read_memories(agent, identity, cx).await?;
    if previous.partial {
        return Err("Memory listing is incomplete; restore was not started".into());
    }
    // A retry keeps entries that are already current.
    let mut unchanged = 0;
    let mut writes = Vec::new();
    for entry in entries {
        match previous.get(&entry.slug) {
            Some(current) if current.body == entry.body => unchanged += 1,
            current => writes.push(MemoryRestore {
                slug: entry.slug.clone(),
                body: entry.body.clone(),
                after: current.map(|c| c.created_at),
            }),
        }
    }
    if writes.is_empty() {
        return Ok((unchanged, Vec::new()));
    }
    let response = request(
        host,
        Request::RestoreMemory {
            id: agent.id.clone(),
            owner: identity.pubkey_hex(),
            entries: writes,
        },
        cx,
    )
    .await?;
    let Response::MemoryRestored {
        published,
        mut errors,
    } = response
    else {
        return Err("The host sent an unexpected reply.".into());
    };
    let listing = read_memories(agent, identity, cx).await?;
    let mut confirmed = 0;
    for restored in published {
        if !listing.partial
            && listing
                .get(&restored.slug)
                .is_some_and(|current| current.event_id == restored.event_id)
        {
            confirmed += 1;
        } else {
            errors.push(format!(
                "{}: memory restore failed; retry this agent",
                restored.slug
            ));
        }
    }
    Ok((unchanged + confirmed, errors))
}

/// The one import path for files, direct shares and catalog adoption
/// (`importTeamSnapshot`): validates the snapshot, creates every member
/// with a fresh identity bound to the new team, publishes their profiles,
/// saves the portable team record, then restores memories if asked. An
/// interrupted import resumes from its saved receipt without new keys.
pub async fn import_team(
    host: Entity<HostConnection>,
    kit: TeamKit,
    community: Entity<Community>,
    identity: Arc<Identity>,
    snapshot: TeamSnapshot,
    options: ImportOptions,
    cx: &mut AsyncApp,
) -> Result<Imported, String> {
    let content = serde_json::to_string(&snapshot).map_err(|e| e.to_string())?;
    let validated = preview(&host, content, cx).await?;
    let canonical = serde_json::to_string(&validated).map_err(|e| e.to_string())?;
    let key = receipt_key(
        &options.destination,
        &options.owner,
        options.keep_allowlist,
        &canonical,
    );
    let partition: Partition = cx.read_entity(&community, |community, _| community.partition());
    let store = cx.update(|cx| cx.global::<AppState>().store.clone());
    let saved = store.view_state(&partition, &key).ok().flatten();
    let receipt = receipt(saved.as_deref(), validated.members.len())?;
    let _ = store.set_view_state(
        &partition,
        &key,
        Some(&serde_json::to_string(&receipt).expect("receipts serialize")),
    );
    // Validate every member before creating any copy.
    let harnesses = cx.read_entity(&host, |host, _| host.harnesses.clone().unwrap_or_default());
    let edits = validated
        .members
        .iter()
        .map(|member| member_edit(member, &harnesses))
        .collect::<Result<Vec<_>, _>>()?;
    let mut ids = Vec::with_capacity(edits.len());
    for ((member, edit), request_id) in validated.members.iter().zip(edits).zip(&receipt.requests) {
        let prepared = request(
            &host,
            Request::TeamMemberPrepare {
                request_id: request_id.clone(),
                relay_url: options.destination.clone(),
                owner: options.owner.clone(),
            },
            cx,
        )
        .await?;
        let Response::TeamMemberPrepared { id, pubkey, saved } = prepared else {
            return Err("The host sent an unexpected reply.".into());
        };
        if !saved {
            let auth = identity.attest_agent(&pubkey).map_err(|e| e.to_string())?;
            let bundle = BundleMember {
                team: receipt.id.clone(),
                member: MemberSnapshot {
                    memory: MemberMemory::default(),
                    ..member.clone()
                },
                instructions: validated.team.instructions.clone().unwrap_or_default(),
                keep_allowlist: options.keep_allowlist,
            };
            request(
                &host,
                Request::TeamMemberCommit {
                    request_id: request_id.clone(),
                    edit,
                    auth,
                    bundle: Box::new(bundle),
                },
                cx,
            )
            .await?;
        }
        ids.push(id);
    }
    let snapshot = match request(&host, Request::Agents, cx).await? {
        Response::Agents(snapshot) => snapshot,
        _ => return Err("The host sent an unexpected reply.".into()),
    };
    let agents: Vec<Agent> = ids
        .iter()
        .map(|id| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.id == *id)
                .cloned()
                .ok_or("An imported member is unavailable")
        })
        .collect::<Result<_, _>>()?;
    let mut failures = Vec::new();
    for agent in agents.iter().filter(|agent| agent.profile_pending) {
        if let Err(error) = request(
            &host,
            Request::PublishProfile {
                id: agent.id.clone(),
            },
            cx,
        )
        .await
        {
            failures.push(format!("{}: {error}", agent.name));
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }
    let entries = kit.reload().await?;
    let existing = entries
        .entries
        .iter()
        .find(|entry| entry.team.id == receipt.id);
    let saved = existing.is_some_and(|entry| {
        entry
            .team
            .portable
            .as_ref()
            .is_some_and(|manifest| manifest.revision == receipt.id)
    });
    if !saved {
        let team = Team {
            id: receipt.id.clone(),
            name: validated.team.name.clone(),
            agents: agents.iter().map(|agent| agent.pubkey.clone()).collect(),
            portable: None,
        };
        let value = serde_json::to_value(&validated).map_err(|e| e.to_string())?;
        kit.save_portable(
            team,
            value,
            existing.map(|entry| entry.event_id.clone()),
            receipt.id.clone(),
        )
        .await?;
    }
    let mut memory_failures = Vec::new();
    if options.restore_memory {
        for (index, agent) in agents.iter().enumerate() {
            let entries = &validated.members[index].memory.entries;
            if entries.is_empty() {
                continue;
            }
            match restore_member_memory(&host, agent, entries, &identity, cx).await {
                Ok((written, errors)) if errors.is_empty() => {
                    let _ = written;
                }
                Ok((written, errors)) => memory_failures.push(format!(
                    "{}: {written}/{} memories restored. {}",
                    agent.pubkey,
                    entries.len(),
                    errors.join("; ")
                )),
                Err(error) => memory_failures.push(format!(
                    "{}: 0/{} memories restored. {error}",
                    agent.pubkey,
                    entries.len()
                )),
            }
        }
    }
    if memory_failures.is_empty() {
        let _ = store.set_view_state(&partition, &key, None);
    }
    Ok(Imported {
        id: receipt.id,
        memory_failures,
    })
}

// ---------------------------------------------------------------------------
// Export

/// A saved portable team with its members' current settings and, when
/// asked, their memories (`exportTeam`), as validated snapshot JSON.
pub async fn export_team(
    kit: TeamKit,
    community: Entity<Community>,
    team: Team,
    memory: MemoryLevel,
    cx: &mut AsyncApp,
) -> Result<String, String> {
    let host = cx
        .update(|cx| import_host(cx))
        .ok_or("Team export is unavailable")?;
    let loaded = kit.load_team(&team).await?;
    let portable = preview(&host, loaded.to_string(), cx).await?;
    if portable.members.len() != team.agents.len() {
        return Err("Portable team members do not match their definitions".into());
    }
    // The members' current saved settings; the portable team owns the
    // team's own metadata.
    let mut snapshot = capture(&community, portable.team, &team.agents, cx).await?;
    if memory != MemoryLevel::None {
        for (member, pubkey) in snapshot.members.iter_mut().zip(&team.agents) {
            let listing = kit.agent_memories(pubkey).await?;
            if listing["partial"].as_bool() != Some(false) {
                return Err("Memory listing is incomplete; retry or export Team only".into());
            }
            let entries = listing["entries"]
                .as_array()
                .ok_or("Invalid memory listing")?;
            member.memory = MemberMemory {
                level: memory.name().into(),
                entries: entries
                    .iter()
                    .filter(|entry| memory == MemoryLevel::Everything || entry["slug"] == "core")
                    .map(|entry| {
                        Ok(MemoryEntry {
                            slug: entry["slug"]
                                .as_str()
                                .ok_or("Invalid memory listing")?
                                .into(),
                            body: entry["body"]
                                .as_str()
                                .ok_or("Invalid memory listing")?
                                .into(),
                        })
                    })
                    .collect::<Result<_, String>>()?,
            };
        }
    }
    let content = serde_json::to_string(&snapshot).map_err(|e| e.to_string())?;
    let validated = preview(&host, content, cx).await?;
    serde_json::to_string(&validated).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Deploy

fn alive(status: ProcessStatus) -> bool {
    matches!(
        status,
        ProcessStatus::Running | ProcessStatus::Waiting | ProcessStatus::Starting
    )
}

async fn find_member(
    community: &Entity<Community>,
    pubkey: &str,
    cx: &mut AsyncApp,
) -> Result<Option<(Entity<HostConnection>, Agent)>, String> {
    let hosts = cx.update(|cx| cx.global::<Hosts>().hosts.clone());
    // A fresh snapshot from every host, applied to its connection.
    for host in &hosts {
        let _ = request(host, Request::Agents, cx).await;
    }
    Ok(cx
        .update(|cx| community_agents(community, cx))
        .into_iter()
        .find(|(_, agent)| agent.pubkey == pubkey))
}

fn agent_in(response: Response, id: &str) -> Option<Agent> {
    match response {
        Response::Agents(snapshot) => snapshot.agents.into_iter().find(|agent| agent.id == id),
        _ => None,
    }
}

/// Deploys a saved team to a channel (`deployTeam`): reuses the exact
/// saved agents, applying a portable team's instructions (restarting a
/// member that runs stale settings), publishing pending profiles, adding
/// each member to the channel and starting it. Failures are per member.
pub async fn deploy_team(
    kit: TeamKit,
    community: Entity<Community>,
    team: Team,
    channel: String,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let snapshot = match &team.portable {
        Some(_) => {
            let host = cx
                .update(|cx| import_host(cx))
                .ok_or("Team preview is unavailable")?;
            let loaded = kit.load_team(&team).await?;
            Some(preview(&host, loaded.to_string(), cx).await?)
        }
        None => None,
    };
    let (session, origin) = cx.read_entity(&community, |community, _| {
        (community.session.clone(), community.origin.http())
    });
    let mut failures = Vec::new();
    for pubkey in &team.agents {
        let result: Result<(), String> = async {
            let mut found = find_member(&community, pubkey, cx).await?;
            if let Some(snapshot) = &snapshot {
                let unavailable = || "A portable team member is unavailable locally".to_string();
                let (host, agent) = found.clone().ok_or_else(unavailable)?;
                let teams: BTreeMap<String, TeamCatalogEntry> = kit
                    .current_members()
                    .await?
                    .into_iter()
                    .map(|(id, head)| {
                        (
                            id,
                            TeamCatalogEntry {
                                created_at: head.created_at,
                                event_id: head.event_id,
                                members: head.members,
                            },
                        )
                    })
                    .collect();
                let response = request(
                    &host,
                    Request::TeamInstructions {
                        id: agent.id.clone(),
                        revision: agent.revision,
                        instructions: snapshot.team.instructions.clone().unwrap_or_default(),
                        team: team.id.clone(),
                        community: origin.clone(),
                        owner: session.viewer().to_string(),
                        teams,
                    },
                    cx,
                )
                .await?;
                let agent = agent_in(response, &agent.id).ok_or_else(unavailable)?;
                if agent.status == ProcessStatus::Running
                    && agent.running_revision != Some(agent.revision)
                {
                    let response = request(
                        &host,
                        Request::Action {
                            id: agent.id.clone(),
                            action: AgentAction::Restart,
                            replay_floor: None,
                            owner: Some(session.viewer().to_string()),
                        },
                        cx,
                    )
                    .await?;
                    let current = agent_in(response, &agent.id);
                    if !current
                        .as_ref()
                        .is_some_and(|current| alive(current.status))
                    {
                        return Err(current
                            .and_then(|current| current.error)
                            .unwrap_or_else(|| "Team member did not restart".into()));
                    }
                }
                found = Some((host, agent));
            }
            if let Some((host, agent)) = &found
                && agent.profile_pending
            {
                request(
                    host,
                    Request::PublishProfile {
                        id: agent.id.clone(),
                    },
                    cx,
                )
                .await?;
            }
            let member = cx.read_entity(&community, |community, _| {
                community
                    .snapshot
                    .channels
                    .iter()
                    .any(|c| c.id == channel && c.members.contains(pubkey))
            });
            if !member {
                session.add_member(&channel, pubkey).await?;
            }
            // Only agents managed here start; others just join.
            if let Some((host, agent)) = find_member(&community, pubkey, cx).await?
                && !alive(agent.status)
            {
                let response = request(
                    &host,
                    Request::Action {
                        id: agent.id.clone(),
                        action: AgentAction::Start,
                        replay_floor: None,
                        owner: Some(session.viewer().to_string()),
                    },
                    cx,
                )
                .await?;
                if !agent_in(response, &agent.id).is_some_and(|agent| alive(agent.status)) {
                    return Err("Added, but the agent did not start. Retry to start it.".into());
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            failures.push(format!("{pubkey}: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_host::api::{MemberDefinition, MemberProfile, SessionPolicy};

    fn harness(kind: HarnessKind, command: Option<&str>) -> Harness {
        Harness {
            kind,
            label: format!("{kind:?}"),
            command: command.map(str::to_owned),
            missing: Vec::new(),
            setup_hint: String::new(),
            setup_url: None,
            status: None,
            install_supported: false,
            update_supported: false,
            login_command: None,
        }
    }

    fn member(runtime: Option<&str>) -> MemberSnapshot {
        MemberSnapshot {
            format: "buzz-agent-snapshot".into(),
            version: 1,
            definition: MemberDefinition {
                name: "Scout".into(),
                system_prompt: Some("Find.".into()),
                runtime: runtime.map(str::to_owned),
                model: Some("gpt-5".into()),
                session_policy: SessionPolicy::Thread,
                effort: Some("high".into()),
                ..Default::default()
            },
            profile: MemberProfile {
                display_name: "Scout".into(),
                avatar_url: Some("https://example.com/a.png".into()),
                ..Default::default()
            },
            memory: MemberMemory::default(),
        }
    }

    #[test]
    fn member_edits_use_the_installed_runtime() {
        let harnesses = [
            harness(HarnessKind::BuzzAgent, Some("/bin/buzz-agent")),
            harness(HarnessKind::Pi, None),
        ];
        let edit = member_edit(&member(None), &harnesses).unwrap();
        assert_eq!(edit.harness.command, "/bin/buzz-agent");
        assert_eq!(edit.harness.model, "gpt-5");
        assert_eq!(edit.session_policy, Some(Some(SessionPolicy::Thread)));
        assert_eq!(edit.effort, Some(Some("high".into())));
        assert_eq!(edit.picture.as_deref(), Some("https://example.com/a.png"));
        assert!(
            edit.environment.is_empty(),
            "team members keep worker defaults"
        );
        assert_eq!(
            member_edit(&member(Some("pi")), &harnesses).unwrap_err(),
            "pi is unavailable. Install it in Settings first."
        );
        assert_eq!(
            member_edit(&member(Some("codex")), &harnesses).unwrap_err(),
            "Import is blocked: runtime (unsupported)."
        );
        let mut external = member(Some("claude"));
        external.definition.provider = Some("anthropic".into());
        assert_eq!(
            member_edit(
                &external,
                &[harness(HarnessKind::Claude, Some("claude-agent-acp"))]
            )
            .unwrap_err(),
            "Import is blocked: external harness provider selector."
        );
        let mut renamed = member(None);
        renamed.profile.display_name = "Other".into();
        assert_eq!(
            member_edit(&renamed, &harnesses).unwrap_err(),
            "Import is blocked: definition and profile names disagree."
        );
    }

    #[test]
    fn receipts_resume_and_refuse_tampering() {
        let key = receipt_key("wss://relay", "owner", false, "{}");
        assert_eq!(key, receipt_key("wss://relay", "owner", false, "{}"));
        assert_ne!(key, receipt_key("wss://relay", "owner", true, "{}"));
        assert!(key.starts_with("buzz-team-import:"));
        let fresh = receipt(None, 2).unwrap();
        assert_eq!(fresh.requests.len(), 2);
        let saved = serde_json::to_string(&fresh).unwrap();
        assert_eq!(receipt(Some(&saved), 2).unwrap(), fresh);
        assert_eq!(
            receipt(Some(&saved), 3).unwrap_err(),
            "Saved team import receipt is invalid."
        );
        assert!(receipt(Some("{\"id\":\"x\",\"requests\":[]}"), 0).is_err());
    }
}
