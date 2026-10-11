//! The community catalog and saved teams against the local test relay,
//! with throwaway coordinates. Skipped unless `BUZZ_TEST_RELAY` points at
//! `.test-relay`.

use buzz_identity::Identity;
use buzz_relay_client::catalog::{
    CatalogDelivery, CatalogKind, CatalogStatus, CatalogView, PersonaSource, Publication,
    SessionPolicy, TeamSource, agent_catalog_content, team_catalog_content,
};
use buzz_relay_client::session::{CommunityCatalog, Resume, TeamKit, TeamsStatus, TeamsView};
use buzz_relay_client::teams::Team;
use buzz_relay_client::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

fn fixture() -> Option<(RelayOrigin, Value)> {
    let dir = std::env::var("BUZZ_TEST_RELAY").ok()?;
    let identities: Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/identities.json")).unwrap())
            .unwrap();
    let origin = RelayOrigin::parse(identities["relay_ws_url"].as_str().unwrap()).unwrap();
    Some((origin, identities))
}

fn open(origin: &RelayOrigin, identities: &Value, who: &str) -> Session {
    let identity = Arc::new(
        Identity::from_nsec(identities["identities"][who]["nsec"].as_str().unwrap()).unwrap(),
    );
    Session::open(
        &tokio::runtime::Handle::current(),
        origin.clone(),
        identity,
        Store::in_memory().unwrap(),
    )
}

async fn until<T: Clone>(
    receiver: &mut watch::Receiver<T>,
    what: &str,
    predicate: impl Fn(&T) -> bool,
) -> T {
    tokio::time::timeout(
        Duration::from_secs(30),
        receiver.wait_for(|value| predicate(value)),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    .unwrap()
    .clone()
}

async fn ready(catalog: &CommunityCatalog) -> Arc<CatalogView> {
    let mut view = catalog.view();
    until(&mut view, "the catalog and outbox", |v| {
        v.status == CatalogStatus::Ready && v.writable
    })
    .await
}

fn throwaway() -> String {
    format!("test-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_and_unshares_through_the_outbox() {
    let Some((origin, identities)) = fixture() else {
        eprintln!("BUZZ_TEST_RELAY not set; skipping");
        return;
    };
    let alice = open(&origin, &identities, "alice");
    let bob = open(&origin, &identities, "bob");
    let catalog = alice.community_catalog();
    catalog.retain();
    ready(&catalog).await;
    // A throwaway agent "key" as the coordinate.
    let d = key_of(&throwaway());
    let agent = PersonaSource {
        pubkey: d.clone(),
        name: "Catalog test agent".into(),
        system_prompt: "Answer questions.".into(),
        harness_command: "buzz-agent".into(),
        respond_to: Some("allowlist".into()),
        ..Default::default()
    };
    let content = agent_catalog_content(&agent, Some(SessionPolicy::Channel)).unwrap();
    catalog
        .publish(CatalogKind::Agent, &d, true, Some(content))
        .await
        .unwrap();
    let mut view = catalog.view();
    let shared = until(&mut view, "the share to be confirmed", |v| {
        let state = v.state(CatalogKind::Agent, &d);
        state.shared
            && state
                .change
                .as_ref()
                .is_some_and(|c| c.delivery == CatalogDelivery::Accepted)
    })
    .await;
    assert!(shared.agents.iter().any(|a| a.d == d));

    // Another member finds it.
    let other = bob.community_catalog();
    other.retain();
    let listed = ready(&other).await;
    let entry = listed
        .agents
        .iter()
        .find(|a| a.d == d)
        .expect("listed for bob");
    assert_eq!(entry.agent.display_name, "Catalog test agent");
    assert_eq!(entry.owner, alice.viewer());
    assert!(!listed.state(CatalogKind::Agent, &d).shared, "not bob's");

    // Unsharing is a newer head that reuses the content.
    catalog
        .publish(CatalogKind::Agent, &d, false, None)
        .await
        .unwrap();
    until(&mut view, "the unshare to be confirmed", |v| {
        let state = v.state(CatalogKind::Agent, &d);
        !state.shared
            && state
                .change
                .as_ref()
                .is_some_and(|c| !c.shared && c.delivery == CatalogDelivery::Accepted)
    })
    .await;
    let operation = catalog
        .snapshot()
        .state(CatalogKind::Agent, &d)
        .change
        .unwrap()
        .operation;
    catalog.dismiss(operation).await.unwrap();
    until(&mut view, "the notice to clear", |v| {
        v.state(CatalogKind::Agent, &d).change.is_none()
    })
    .await;
    other.refresh();
    let mut others = other.view();
    until(&mut others, "the agent to leave bob's catalog", |v| {
        v.status == CatalogStatus::Ready && !v.agents.iter().any(|a| a.d == d)
    })
    .await;
}

/// A throwaway 64-hex key derived from `text`.
fn key_of(text: &str) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_team_is_listed_and_rechecked_before_adoption() {
    let Some((origin, identities)) = fixture() else {
        return;
    };
    let alice = open(&origin, &identities, "alice");
    let bob = open(&origin, &identities, "bob");
    let catalog = alice.community_catalog();
    catalog.retain();
    ready(&catalog).await;
    let d = throwaway();
    let member = key_of(&d);
    let content = team_catalog_content(
        &TeamSource {
            id: d.clone(),
            name: "Catalog test team".into(),
            agents: vec![member.clone()],
            description: Some("Throwaway".into()),
            instructions: Some("Work together.".into()),
        },
        &[PersonaSource {
            pubkey: member,
            name: "Member".into(),
            system_prompt: "Help.".into(),
            harness_command: "goose".into(),
            ..Default::default()
        }],
        Some(SessionPolicy::Thread),
    )
    .unwrap();
    catalog
        .publish(CatalogKind::Team, &d, true, Some(content))
        .await
        .unwrap();
    let other = bob.community_catalog();
    other.retain();
    let mut view = other.view();
    let listed = loop {
        let snapshot = until(&mut view, "bob's catalog", |v| {
            v.status == CatalogStatus::Ready
        })
        .await;
        if let Some(team) = snapshot.teams.iter().find(|t| t.d == d) {
            break team.clone();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        other.refresh();
    };
    assert_eq!(listed.members.len(), 1);
    assert_eq!(listed.members[0].agent.runtime.as_deref(), Some("goose"));
    assert_eq!(listed.instructions.as_deref(), Some("Work together."));
    assert_eq!(
        Publication::Team(listed.clone()).unsupported_transport(),
        None
    );
    let current = other.current_team(&listed).await.unwrap();
    assert_eq!(current.event_id, listed.event_id);
    // Withdrawn: adoption refuses the stale listing.
    catalog
        .publish(CatalogKind::Team, &d, false, None)
        .await
        .unwrap();
    let mut own = catalog.view();
    until(&mut own, "the team unshare", |v| {
        v.state(CatalogKind::Team, &d)
            .change
            .is_some_and(|c| !c.shared && c.delivery == CatalogDelivery::Accepted)
    })
    .await;
    // The relay withholds others' unshared heads.
    let error = other.current_team(&listed).await.unwrap_err();
    assert_eq!(error, "This team is no longer available in the catalog.");
}

async fn teams_ready(kit: &TeamKit) -> Arc<TeamsView> {
    kit.refresh();
    let mut view = kit.view();
    tokio::time::sleep(Duration::from_millis(50)).await;
    until(&mut view, "the team list", |v| {
        v.status == TeamsStatus::Ready || v.status == TeamsStatus::Error
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_loads_and_deletes_a_portable_team() {
    let Some((origin, identities)) = fixture() else {
        return;
    };
    let alice = open(&origin, &identities, "alice");
    let mut snapshot = alice.snapshot();
    until(&mut snapshot, "the session", |s| {
        s.status == SessionStatus::Ready
    })
    .await;
    let kit = alice.team_kit();
    let listed = teams_ready(&kit).await;
    assert_eq!(listed.status, TeamsStatus::Ready, "{:?}", listed.error);
    let id = throwaway();
    let member = key_of(&id);
    let team = Team {
        id: id.clone(),
        name: "Throwaway team".into(),
        agents: vec![member.clone()],
        portable: None,
    };
    let first = kit.save(team.clone(), None, false).await.unwrap();
    let saved = teams_ready(&kit).await;
    let entry = saved.teams().find(|e| e.team.id == id).expect("saved");
    assert_eq!(entry.event_id, first);
    assert_eq!(entry.team.agents, vec![member.clone()]);
    // A stale expected head is refused.
    let stale = kit.save(team.clone(), None, false).await.unwrap_err();
    assert!(stale.starts_with("This saved recipe changed."), "{stale}");

    let portable = json!({
        "format": "buzz-team-snapshot",
        "version": 1,
        "team": { "name": "Throwaway team", "instructions": "Pair up." },
        "members": [{
            "format": "buzz-agent-snapshot",
            "version": 1,
            "definition": { "name": "Member", "systemPrompt": "Help.", "sessionPolicy": "channel" },
            "profile": { "displayName": "Member" },
            "memory": { "level": "none", "entries": [] },
        }],
    });
    // Deleting the plain team, then saving a portable one: version 2
    // records live at their own coordinate.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    kit.save(team.clone(), Some(first), true).await.unwrap();
    let id = throwaway();
    let team = Team {
        id: id.clone(),
        ..team
    };
    let revision = uuid::Uuid::new_v4().to_string();
    let second = kit
        .save_portable(team.clone(), portable.clone(), None, revision)
        .await
        .unwrap();
    let saved = teams_ready(&kit).await;
    let entry = saved.teams().find(|e| e.team.id == id).expect("portable");
    assert_eq!(entry.event_id, second);
    let manifest = entry.team.portable.clone().expect("a manifest");
    assert_eq!(manifest.owner, alice.viewer());
    assert_eq!(kit.load_team(&entry.team).await.unwrap(), portable);
    let heads = kit.current_members().await.unwrap();
    assert_eq!(heads[&id].members, vec![member]);
    assert_eq!(heads[&id].event_id, second);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    kit.save(entry.team.clone(), Some(second), true)
        .await
        .unwrap();
    let saved = teams_ready(&kit).await;
    assert!(saved.teams().all(|e| e.team.id != id));
    assert!(kit.current_members().await.unwrap()[&id].members.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_reads_and_retires_team_text_in_its_own_record() {
    let Some((origin, identities)) = fixture() else {
        return;
    };
    let alice = open(&origin, &identities, "alice");
    let mut snapshot = alice.snapshot();
    until(&mut snapshot, "the session", |s| {
        s.status == SessionStatus::Ready
    })
    .await;
    let kit = alice.team_kit();
    let listed = teams_ready(&kit).await;
    assert_eq!(listed.status, TeamsStatus::Ready, "{:?}", listed.error);
    let id = throwaway();
    let team = Team {
        id: id.clone(),
        name: "Throwaway team".into(),
        agents: vec![key_of(&id)],
        portable: None,
    };
    let team_head = kit.save(team.clone(), None, false).await.unwrap();
    // No head yet: a fresh read proves it.
    assert_eq!(kit.read_text(&id).await.unwrap(), None);

    // A live head only over the team's current live head.
    let manifest = kit
        .prepare_text(&id, "SHARED", &uuid::Uuid::new_v4().to_string())
        .await
        .unwrap();
    let stale = kit
        .publish_text(
            &id,
            Some(manifest.clone()),
            None,
            Some("00".repeat(32)),
            Resume::new(),
        )
        .await
        .unwrap_err();
    assert!(stale.contains("changed or was deleted"), "{stale}");
    let resume = Resume::new();
    let head = kit
        .publish_text(
            &id,
            Some(manifest.clone()),
            None,
            Some(team_head.clone()),
            resume.clone(),
        )
        .await
        .unwrap();
    assert_eq!(resume.id().as_deref(), Some(head.as_str()));
    let read = kit.read_text(&id).await.unwrap().expect("a text head");
    assert_eq!(
        (read.text.as_str(), read.head.as_str()),
        ("SHARED", head.as_str())
    );
    // A retry confirms the event the earlier attempt enqueued.
    let again = kit
        .publish_text(&id, Some(manifest), None, Some(team_head.clone()), resume)
        .await
        .unwrap();
    assert_eq!(again, head);
    // The team record keeps its format; the head is not a catalog entry.
    let saved = teams_ready(&kit).await;
    assert_eq!(saved.entries.iter().filter(|e| e.team.id == id).count(), 1);

    // A stale text head, and a revision reused with other text, are refused.
    let revision = uuid::Uuid::new_v4().to_string();
    let second = kit.prepare_text(&id, "SECOND", &revision).await.unwrap();
    let stale = kit
        .publish_text(
            &id,
            Some(second),
            None,
            Some(team_head.clone()),
            Resume::new(),
        )
        .await
        .unwrap_err();
    assert!(stale.contains("changed"), "{stale}");
    let reused = kit.prepare_text(&id, "OTHER", &revision).await.unwrap_err();
    assert!(reused.contains("different content"), "{reused}");

    // Retiring the head leaves a tombstone, read as empty text.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let tombstone = kit
        .publish_text(&id, None, Some(head), None, Resume::new())
        .await
        .unwrap();
    let read = kit.read_text(&id).await.unwrap().expect("a tombstone");
    assert_eq!(
        (read.text.as_str(), read.head.as_str()),
        ("", tombstone.as_str())
    );
    let current = kit.read_text_head(&id).await.unwrap().unwrap();
    assert_eq!((current.head, current.deleted), (tombstone, true));

    tokio::time::sleep(Duration::from_millis(1100)).await;
    kit.save(team, Some(team_head), true).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_portable_record_supersedes_the_later_retired_ordinary_one() {
    let Some((origin, identities)) = fixture() else {
        return;
    };
    let alice = open(&origin, &identities, "alice");
    let mut snapshot = alice.snapshot();
    until(&mut snapshot, "the session", |s| {
        s.status == SessionStatus::Ready
    })
    .await;
    let kit = alice.team_kit();
    teams_ready(&kit).await;
    let id = throwaway();
    let member = key_of(&id);
    let team = Team {
        id: id.clone(),
        name: "Throwaway team".into(),
        agents: vec![member.clone()],
        portable: None,
    };
    let ordinary = kit.save(team.clone(), None, false).await.unwrap();
    let portable = json!({
        "format": "buzz-team-snapshot",
        "version": 1,
        "team": { "name": "Throwaway team" },
        "members": [{
            "format": "buzz-agent-snapshot",
            "version": 1,
            "definition": { "name": "Member", "systemPrompt": "Help.", "sessionPolicy": "channel" },
            "profile": { "displayName": "Member" },
            "memory": { "level": "none", "entries": [] },
        }],
    });
    let revision = uuid::Uuid::new_v4().to_string();
    let head = kit
        .save_portable(team.clone(), portable, None, revision)
        .await
        .unwrap();
    // Retiring the ordinary record is the later write.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    kit.save(team, Some(ordinary), true).await.unwrap();
    let listed = teams_ready(&kit).await;
    let entries: Vec<_> = listed.entries.iter().filter(|e| e.team.id == id).collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event_id, head);
    assert!(!entries[0].deleted);
    let heads = kit.current_members().await.unwrap();
    assert_eq!(heads[&id].event_id, head);
    assert_eq!(heads[&id].members, vec![member]);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    kit.save(entries[0].team.clone(), Some(head), true)
        .await
        .unwrap();
}
