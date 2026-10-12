//! Team instructions (`team-instructions.ts`): each saved team's shared
//! text, kept in its own record. Saves, exports and shares read it here,
//! and every team save or delete, and the first time teams load in a
//! session, delivers it into the members' settings on the hosts that run
//! them: each member gets its team's text after its own instructions, and
//! agents no team with text lists lose it. Delivery never restarts an
//! agent; a running member shows Restart required instead.

use super::catalog::community_agents;
use super::team_ops::{import_host, preview};
use super::teams::TeamsState;
use crate::community::{AppState, Community};
use crate::hosts::control::Status;
use crate::hosts::{HostConnection, Hosts};
use buzz_host::api::{Agent, Request, Response, TeamCatalogEntry, TeamMeta};
use buzz_host_rpc::{ConnectionState, RpcError};
use buzz_relay_client::session::{RosterStatus, Session, TeamKit, TeamsStatus};
use buzz_relay_client::teams::Team;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// App-start attempts before the failure is shown and retries stop.
pub const STARTUP_SYNC_ATTEMPTS: u32 = 4;

/// A team and its current text, trimmed for delivery; `None` when it
/// can't be read now.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamText {
    pub team: Team,
    pub text: Option<String>,
}

/// A portable team's saved metadata, from its bundle.
async fn portable_meta(
    kit: &TeamKit,
    team: &Team,
    cx: &mut AsyncApp,
) -> Result<Option<TeamMeta>, String> {
    if team.portable.is_none() {
        return Ok(None);
    }
    let host = cx
        .update(|cx| import_host(cx))
        .ok_or("Team preview is unavailable")?;
    let loaded = kit.load_team(team).await?;
    Ok(Some(preview(&host, loaded.to_string(), cx).await?.team))
}

/// The one reader of a live team's current instructions, as saved
/// (untrimmed), and the head they came from (`resolveTeamText`). A text
/// head wins, even when empty or a tombstone. Only a fresh read that
/// proves there is no head falls back to a portable bundle's text.
pub async fn resolve_team_text(
    kit: &TeamKit,
    team: &Team,
    cx: &mut AsyncApp,
) -> Result<(String, Option<String>), String> {
    if let Some(current) = kit.read_text(&team.id).await? {
        return Ok((current.text, Some(current.head)));
    }
    let meta = portable_meta(kit, team, cx).await?;
    Ok((
        meta.and_then(|meta| meta.instructions).unwrap_or_default(),
        None,
    ))
}

/// The team metadata exports and shares carry (`teamExportMeta`): the
/// current name and text, and a portable team's retained description.
pub async fn team_export_meta(
    kit: &TeamKit,
    team: &Team,
    cx: &mut AsyncApp,
) -> Result<TeamMeta, String> {
    let current = kit.read_text(&team.id).await?;
    let meta = portable_meta(kit, team, cx).await?;
    let text = match current {
        Some(current) => current.text,
        None => meta
            .as_ref()
            .and_then(|meta| meta.instructions.clone())
            .unwrap_or_default(),
    };
    Ok(TeamMeta {
        name: team.name.clone(),
        description: meta
            .and_then(|meta| meta.description)
            .filter(|description| !description.is_empty()),
        instructions: Some(text),
    })
}

/// The current text of every saved team (`readTeamTexts`), from a fresh
/// list read so rosters are never stale. A deleted team has no members
/// and no text, so the sync releases it without loading anything.
pub async fn read_team_texts(kit: &TeamKit, cx: &mut AsyncApp) -> Result<Vec<TeamText>, String> {
    let view = kit
        .reload()
        .await
        .map_err(|_| "Teams can't be read right now".to_string())?;
    let mut texts = Vec::with_capacity(view.entries.len());
    for entry in &view.entries {
        if entry.deleted {
            texts.push(TeamText {
                team: Team {
                    agents: Vec::new(),
                    ..entry.team.clone()
                },
                text: Some(String::new()),
            });
            continue;
        }
        let text = resolve_team_text(kit, &entry.team, cx)
            .await
            .ok()
            .map(|(text, _)| text.trim().to_owned());
        texts.push(TeamText {
            team: entry.team.clone(),
            text,
        });
    }
    Ok(texts)
}

/// Refuses a save that would give one agent two different team texts
/// (`teamTextConflict`). Teams without text never conflict; an unreadable
/// overlapping team means the check can't complete.
pub fn team_text_conflict(
    others: &[TeamText],
    id: &str,
    agents: &[String],
    text: &str,
) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let overlapping: Vec<&TeamText> = others
        .iter()
        .filter(|other| {
            other.team.id != id
                && other.text.as_deref() != Some("")
                && other.team.agents.iter().any(|agent| agents.contains(agent))
        })
        .collect();
    if let Some(unreadable) = overlapping.iter().find(|other| other.text.is_none()) {
        return Some(format!(
            "An agent on this team is also on \"{}\", whose instructions can't be read right now, so the save can't be checked. Try again.",
            unreadable.team.name
        ));
    }
    overlapping
        .iter()
        .find(|other| other.text.as_deref() != Some(text.trim()))
        .map(|other| {
            format!(
                "An agent on this team is also on \"{}\", which has different team instructions. Remove the agent from one team or use the same instructions.",
                other.team.name
            )
        })
}

/// The agent and two teams that make a delivery impossible: one of the
/// owner's agents is on two teams with different text. Names them so the
/// user knows which team to edit.
fn delivery_conflict(texts: &[TeamText], agents: &[Agent]) -> Option<String> {
    for agent in agents {
        let teams: Vec<&TeamText> = texts
            .iter()
            .filter(|t| {
                t.text.as_deref().is_some_and(|text| !text.is_empty())
                    && t.team.agents.contains(&agent.pubkey)
            })
            .collect();
        let Some(first) = teams.first() else {
            continue;
        };
        if let Some(other) = teams.iter().find(|t| t.text != first.text) {
            return Some(format!(
                "{} is on teams \"{}\" and \"{}\", which have different instructions. Give both teams the same instructions or remove the agent from one.",
                agent.name, first.team.name, other.team.name
            ));
        }
    }
    None
}

fn connected(host: &Entity<HostConnection>, cx: &App) -> bool {
    matches!(host.read(cx).state, ConnectionState::Connected { .. })
}

/// Writes every team's current text into its members' settings and
/// clears it for agents no team with text lists (`deliverTeamTexts`).
/// Runs after each team save or delete. Never restarts anyone. The error
/// is unworded: each caller says what was saved or deleted.
pub async fn deliver_team_texts(
    teams: &Entity<TeamsState>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    queue_delivery(teams, cx).await
}

/// Deliveries for one community run one at a time, each from its own
/// fresh read, so an older pass can never land after a newer one.
async fn queue_delivery(teams: &Entity<TeamsState>, cx: &mut AsyncApp) -> Result<(), String> {
    let lane = cx.read_entity(teams, |teams, _| teams.delivery.clone());
    let _turn = lane.lock().await;
    deliver_now(teams, cx).await
}

async fn deliver_now(teams: &Entity<TeamsState>, cx: &mut AsyncApp) -> Result<(), String> {
    let (kit, community) = cx.read_entity(teams, |teams, _| {
        (teams.kit.clone(), teams.community.clone())
    });
    let (origin, owner) = cx.read_entity(&community, |community, _| {
        (
            community.origin.http(),
            community.session.viewer().to_string(),
        )
    });
    // The hosts that run this community's agents, as reachable now.
    let (agents, hosts) = cx.update(|cx| {
        let agents: Vec<(Entity<HostConnection>, Agent)> = community_agents(&community, cx)
            .into_iter()
            .filter(|(host, _)| connected(host, cx))
            .collect();
        let mut hosts: Vec<Entity<HostConnection>> = Vec::new();
        for (host, _) in &agents {
            if !hosts
                .iter()
                .any(|known| known.entity_id() == host.entity_id())
            {
                hosts.push(host.clone());
            }
        }
        (agents, hosts)
    });
    if hosts.is_empty() {
        return Ok(());
    }
    let texts = read_team_texts(&kit, cx).await?;
    let agents: Vec<Agent> = agents.into_iter().map(|(_, agent)| agent).collect();
    if let Some(conflict) = delivery_conflict(&texts, &agents) {
        return Err(conflict);
    }
    let heads: BTreeMap<String, TeamCatalogEntry> = kit
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
    // A team left out could not be read now and keeps its members' text.
    let texts: BTreeMap<String, String> = texts
        .into_iter()
        .filter_map(|t| Some((t.team.id, t.text?)))
        .collect();
    for host in hosts {
        let label = cx.read_entity(&host, |host, _| host.label.clone());
        let request = Request::TeamSync {
            community: origin.clone(),
            owner: owner.clone(),
            teams: heads.clone(),
            texts: texts.clone(),
        };
        match host.update(cx, |host, cx| host.request(request, cx)).await {
            Ok(Response::Agents(_)) => {}
            Ok(_) => return Err("The host sent an unexpected reply.".into()),
            // A host from before team text records.
            Err(RpcError::Failed(message)) if message.contains("does not support this request") => {
                return Err(format!("Update {label} to sync team instructions"));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    teams.update(cx, |teams, cx| {
        teams.sync_error = None;
        cx.notify();
    });
    Ok(())
}

/// Delivers team text once per relay session (`bindTeamTextSync`), after
/// its teams and the local agents have both loaded and no agent operation
/// is running. It reads the teams once the community's channels are ready,
/// so it also runs when the app opens on a page without teams. A failed
/// delivery retries with backoff; after the last attempt the error shows on
/// the teams list until a later delivery succeeds.
pub struct StartupSync {
    watched: Option<Arc<Session>>,
    community: Option<Entity<Community>>,
    synced: bool,
    running: bool,
    failures: u32,
    retry: Option<Task<()>>,
    hosts: Vec<EntityId>,
    _session: Vec<Subscription>,
    _hosts: Vec<Subscription>,
    _app: Vec<Subscription>,
}

struct Startup(#[allow(dead_code)] Entity<StartupSync>);

impl Global for Startup {}

/// Starts the app-start sync.
pub fn init(cx: &mut App) {
    let sync = cx.new(|cx| {
        let mut sync = StartupSync {
            watched: None,
            community: None,
            synced: false,
            running: false,
            failures: 0,
            retry: None,
            hosts: Vec::new(),
            _session: Vec::new(),
            _hosts: Vec::new(),
            _app: vec![
                cx.observe_global::<AppState>(|this: &mut StartupSync, cx| this.update(cx)),
                cx.observe_global::<Hosts>(|this: &mut StartupSync, cx| this.update(cx)),
            ],
        };
        sync.update(cx);
        sync
    });
    cx.set_global(Startup(sync));
}

impl StartupSync {
    fn update(&mut self, cx: &mut Context<Self>) {
        let hosts = cx.global::<Hosts>().hosts.clone();
        let ids: Vec<EntityId> = hosts.iter().map(Entity::entity_id).collect();
        if ids != self.hosts {
            self._hosts = hosts
                .iter()
                .map(|host| cx.observe(host, |this, _, cx| this.update(cx)))
                .collect();
            self.hosts = ids;
        }
        let community = AppState::current(cx);
        let session = community
            .as_ref()
            .map(|community| community.read(cx).session.clone());
        let same = match (&session, &self.watched) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.retry = None;
            self.failures = 0;
            self.running = false;
            self.synced = false;
            self.watched = session;
            self._session = match &community {
                Some(community) => {
                    let teams = TeamsState::of(community, cx);
                    vec![
                        cx.observe(community, |this, _, cx| this.update(cx)),
                        cx.observe(&teams, |this, _, cx| this.update(cx)),
                    ]
                }
                None => Vec::new(),
            };
            self.community = community;
        }
        let Some(community) = self.community.clone() else {
            return;
        };
        if self.watched.is_none() || self.synced || self.running || self.retry.is_some() {
            return;
        }
        let teams = TeamsState::of(&community, cx);
        let snapshot = community.read(cx).snapshot.clone();
        if snapshot.roster == RosterStatus::Ready && !snapshot.cached {
            teams.read(cx).kit.ensure();
        }
        let (busy, ready) = agents_state(cx);
        // A retry re-reads the hosts and the list itself, since a failed
        // delivery or read leaves them unready; the first attempt waits for
        // both.
        if busy
            || (self.failures == 0 && (teams.read(cx).view.status != TeamsStatus::Ready || !ready))
        {
            return;
        }
        self.running = true;
        let (failures, session) = (self.failures, self.watched.clone());
        cx.spawn(async move |this, cx| {
            let result = attempt(&teams, failures, cx).await;
            let _ = this.update(cx, |this, cx| {
                let current = match (&this.watched, &session) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                };
                if !current {
                    return;
                }
                this.running = false;
                match result {
                    Ok(true) => this.synced = true,
                    // An idle notice that arrived while this attempt ran
                    // was dropped, so recheck admission now.
                    Ok(false) => this.update(cx),
                    Err(error) => {
                        this.failures += 1;
                        tracing::warn!(%error, "Team instruction sync failed");
                        if this.failures >= STARTUP_SYNC_ATTEMPTS {
                            this.synced = true;
                            teams.update(cx, |teams, cx| {
                                teams.sync_error = Some(format!(
                                    "Team members' instructions weren't updated: {error}"
                                ));
                                cx.notify();
                            });
                            return;
                        }
                        let delay = Duration::from_secs(1 << (this.failures - 1));
                        this.retry = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(delay).await;
                            let _ = this.update(cx, |this, cx| {
                                this.retry = None;
                                this.update(cx);
                            });
                        }));
                    }
                }
            });
        })
        .detach();
    }
}

/// Whether an agent operation is running on a connected host, and whether
/// every connected host's agents have loaded.
fn agents_state(cx: &App) -> (bool, bool) {
    let hosts: Vec<Entity<HostConnection>> = cx
        .global::<Hosts>()
        .hosts
        .iter()
        .filter(|host| connected(host, cx))
        .cloned()
        .collect();
    let busy = hosts.iter().any(|host| host.read(cx).control.busy);
    let ready = !hosts.is_empty()
        && hosts
            .iter()
            .all(|host| host.read(cx).status() == Status::Ready);
    (busy, ready)
}

/// One app-start attempt. `Ok(false)` when an agent operation started
/// during a retry's re-read, so it waits for idle without counting as a
/// failure.
async fn attempt(
    teams: &Entity<TeamsState>,
    failures: u32,
    cx: &mut AsyncApp,
) -> Result<bool, String> {
    if failures > 0 {
        let hosts = cx.update(|cx| cx.global::<Hosts>().hosts.clone());
        for host in hosts {
            if cx.update(|cx| connected(&host, cx)) {
                host.update(cx, |host, cx| host.refresh_status(cx)).await;
            }
        }
        let (busy, ready) = cx.update(|cx| agents_state(cx));
        if busy {
            return Ok(false);
        }
        if !ready {
            return Err("Local agents can't be read right now".into());
        }
    }
    queue_delivery(teams, cx).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn team(id: &str, name: &str, agents: &[&str]) -> Team {
        Team {
            id: id.into(),
            name: name.into(),
            agents: agents.iter().map(|a| (*a).to_owned()).collect(),
            portable: None,
        }
    }

    fn text(team: Team, text: Option<&str>) -> TeamText {
        TeamText {
            team,
            text: text.map(Into::into),
        }
    }

    fn agent(pubkey: &str, name: &str) -> Agent {
        Agent {
            pubkey: pubkey.into(),
            name: name.into(),
            ..Agent::default()
        }
    }

    #[test]
    fn names_the_other_team_when_a_shared_member_would_get_different_text() {
        let others = [
            text(team("other", "Reviewers", &["a"]), Some("THEIRS")),
            text(team("quiet", "Quiet", &["a"]), Some("")),
        ];
        let agents = ["a".to_owned()];
        let conflict = team_text_conflict(&others, "team", &agents, "OURS").unwrap();
        assert!(
            conflict.contains("\"Reviewers\", which has different team instructions"),
            "{conflict}"
        );
        // The same text after trimming, no text, or no shared member is fine.
        assert_eq!(
            team_text_conflict(&others, "team", &agents, " THEIRS\n"),
            None
        );
        assert_eq!(team_text_conflict(&others, "team", &agents, "  "), None);
        assert_eq!(
            team_text_conflict(&others, "team", &["b".to_owned()], "OURS"),
            None
        );
        // The team itself never conflicts.
        assert_eq!(team_text_conflict(&others, "other", &agents, "OURS"), None);
    }

    #[test]
    fn cant_certify_a_save_when_an_overlapping_team_is_unreadable() {
        let others = [text(team("other", "Reviewers", &["a"]), None)];
        let conflict = team_text_conflict(&others, "team", &["a".to_owned()], "OURS").unwrap();
        assert!(conflict.contains("can't be read right now"), "{conflict}");
        // Without text of its own, a save never conflicts.
        assert_eq!(
            team_text_conflict(&others, "team", &["a".to_owned()], ""),
            None
        );
    }

    #[test]
    fn delivery_names_the_agent_on_two_differently_instructed_teams() {
        let texts = [
            text(team("one", "Builders", &["a", "b"]), Some("BUILD")),
            text(team("two", "Reviewers", &["a"]), Some("REVIEW")),
            text(team("three", "Silent", &["b"]), Some("")),
            text(team("four", "Unknown", &["b"]), None),
        ];
        let conflict = delivery_conflict(&texts, &[agent("b", "Bee"), agent("a", "Ann")]).unwrap();
        assert_eq!(
            conflict,
            "Ann is on teams \"Builders\" and \"Reviewers\", which have different instructions. Give both teams the same instructions or remove the agent from one."
        );
        // Once one team is edited to match, delivery goes ahead.
        let mut texts = texts.to_vec();
        texts[1].text = Some("BUILD".into());
        assert_eq!(delivery_conflict(&texts, &[agent("a", "Ann")]), None);
    }
}
