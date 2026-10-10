use super::*;
use crate::testing::{Signer, T0};
use nostr::prelude::{Keys, Kind, Tag, Timestamp};

fn source(pubkey: &str) -> PersonaSource {
    PersonaSource {
        pubkey: pubkey.to_owned(),
        name: "Scout".into(),
        system_prompt: "Find things.\n\tCarefully.".into(),
        picture: Some("https://example.com/a.png".into()),
        harness_command: "/usr/local/bin/buzz-agent".into(),
        model: " gpt-5 ".into(),
        provider: "openai".into(),
        acp_command: Some("/opt/buzz/bin/buzz-acp".into()),
        respond_to: Some("allowlist".into()),
        session_policy: None,
    }
}

fn shared_tags(d: &str) -> Vec<Vec<&str>> {
    vec![vec!["d", d], vec!["shared", "true"]]
}

fn publish(
    signer: &Signer,
    keys: &Keys,
    kind: u16,
    at: u64,
    content: &str,
    tags: Vec<Vec<&str>>,
) -> Verified {
    let tags: Vec<&[&str]> = tags.iter().map(Vec::as_slice).collect();
    signer.sign(keys, kind, at, content, &tags)
}

#[test]
fn visible_text_refuses_concealed_characters() {
    assert!(visible_text("Scout 🤖", false));
    assert!(visible_text("a\tb\nc", true));
    assert!(!visible_text("a\tb", false));
    assert!(!visible_text("hidden\u{200B}space", true));
    assert!(!visible_text("bidi\u{202E}override", true));
    assert!(!visible_text("bell\u{7}", true));
    // Emoji ZWJ sequences and VS16 after an emoji stay legal.
    assert!(visible_text("👨\u{200D}👩\u{200D}👧", false));
    assert!(visible_text("❤\u{FE0F}", false));
    assert!(!visible_text("a\u{FE0F}", false));
    assert!(!visible_text("a\u{200D}b", false));
}

#[test]
fn transports_and_runtimes_are_portable_names() {
    assert!(portable_acp_command("buzz-acp"));
    assert!(portable_acp_command("buzz-goose-acp"));
    assert!(!portable_acp_command("/usr/bin/buzz-acp"));
    assert!(!portable_acp_command("other-acp"));
    assert_eq!(runtime_of("/x/buzz-agent"), Some("buzz-agent"));
    assert_eq!(runtime_of("C:\\x\\Buzz-Agent.exe"), Some("buzz-agent"));
    assert_eq!(runtime_of("/x/goose"), Some("goose"));
    assert_eq!(runtime_of("buzz-pi-acp"), Some("pi"));
    assert_eq!(runtime_of("/x/claude-agent-acp"), Some("claude"));
    assert_eq!(runtime_of("/x/hermes-acp"), Some("hermes"));
    assert_eq!(runtime_of("/local/codex-acp"), None);
}

#[test]
fn an_agent_projection_round_trips_and_downgrades_allowlists() {
    let signer = Signer::new();
    let owner = signer.viewer_hex();
    let content = agent_catalog_content(&source(&owner), Some(SessionPolicy::Thread)).unwrap();
    let body: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(body["acp_command"], "buzz-acp");
    assert_eq!(body["runtime"], "buzz-agent");
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(body["respond_to"], "owner-only");
    assert_eq!(body["session_policy"], "thread");
    assert!(body.get("env_vars").is_none());
    let event = publish(
        &signer,
        &signer.viewer,
        30175,
        T0,
        &content,
        shared_tags(&owner),
    );
    let Some(Publication::Agent(listed)) = parse_publication(&event) else {
        panic!("not an agent")
    };
    assert_eq!(listed.agent.display_name, "Scout");
    assert_eq!(listed.agent.respond_to, Some(RespondTo::OwnerOnly));
    assert_eq!(listed.agent.session_policy, SessionPolicy::Thread);
    assert_eq!(
        listed.agent.avatar_url.as_deref(),
        Some("https://example.com/a.png")
    );
    // Without the opt-in tag it is not listed.
    let unshared = publish(
        &signer,
        &signer.viewer,
        30175,
        T0,
        &content,
        vec![vec!["d", &owner]],
    );
    assert_eq!(parse_publication(&unshared), None);
}

#[test]
fn projection_refuses_what_cannot_be_shared() {
    let mut agent = source(&crate::testing::key(1));
    agent.harness_command = "/local/codex-acp".into();
    assert!(
        agent_catalog_content(&agent, Some(SessionPolicy::Channel))
            .unwrap_err()
            .contains("runtime that cannot be shared")
    );
    let mut agent = source(&crate::testing::key(1));
    agent.system_prompt = "zero\u{200B}width".into();
    assert_eq!(
        agent_catalog_content(&agent, Some(SessionPolicy::Channel)).unwrap_err(),
        "Agent contains prohibited invisible or formatting characters"
    );
    let mut agent = source(&crate::testing::key(1));
    agent.name = "x".repeat(129);
    assert_eq!(
        agent_catalog_content(&agent, Some(SessionPolicy::Channel)).unwrap_err(),
        "Display name is too long (129 characters, max 128)"
    );
    // An inheriting agent needs the defaults it runs with.
    let agent = source(&crate::testing::key(1));
    assert!(
        agent_catalog_content(&agent, None)
            .unwrap_err()
            .contains("defaults")
    );
    // Only HTTPS artwork travels.
    let mut agent = source(&crate::testing::key(1));
    agent.picture = Some("data:image/png;base64,AAAA".into());
    let content = agent_catalog_content(&agent, Some(SessionPolicy::Channel)).unwrap();
    assert!(!content.contains("avatar_url"));
}

#[test]
fn a_team_projection_round_trips_with_opaque_member_keys() {
    let signer = Signer::new();
    let (a, b) = (crate::testing::key(1), crate::testing::key(2));
    let mut second = source(&b);
    second.name = "Writer".into();
    second.session_policy = Some(SessionPolicy::Thread);
    let team = TeamSource {
        id: "crew".into(),
        name: "Crew".into(),
        agents: vec![a.clone(), b.clone()],
        description: Some("Two agents".into()),
        instructions: Some("  ".into()),
    };
    let content =
        team_catalog_content(&team, &[source(&a), second], Some(SessionPolicy::Channel)).unwrap();
    let body: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(body["v"], 1);
    assert!(body.get("instructions").is_none());
    assert_eq!(body["members"][0]["member_key"], member_key(&a));
    assert_ne!(body["members"][0]["member_key"], a.as_str());
    // The team schema omits the default boundary.
    assert!(body["members"][0].get("session_policy").is_none());
    assert_eq!(body["members"][1]["session_policy"], "thread");
    let event = publish(
        &signer,
        &signer.viewer,
        30178,
        T0,
        &content,
        shared_tags("crew"),
    );
    let Some(Publication::Team(listed)) = parse_publication(&event) else {
        panic!("not a team")
    };
    assert_eq!(listed.name, "Crew");
    assert_eq!(listed.description.as_deref(), Some("Two agents"));
    assert_eq!(listed.members.len(), 2);
    assert_eq!(
        listed.members[1].agent.session_policy,
        SessionPolicy::Thread
    );
    let snapshot = catalog_team_snapshot(&listed);
    assert_eq!(snapshot["format"], "buzz-team-snapshot");
    assert_eq!(
        snapshot["members"][0]["definition"]["runtime"],
        "buzz-agent"
    );
    assert_eq!(
        snapshot["members"][0]["definition"]["respondTo"],
        "owner-only"
    );
    assert_eq!(snapshot["members"][0]["memory"]["level"], "none");
    // A missing member or an oversized text refuses the projection.
    let missing = TeamSource {
        agents: vec![crate::testing::key(3)],
        ..team.clone()
    };
    assert!(
        team_catalog_content(&missing, &[], None)
            .unwrap_err()
            .contains("not found")
    );
    let long = TeamSource {
        description: Some("x".repeat(4097)),
        ..team
    };
    assert_eq!(
        team_catalog_content(&long, &[source(&a)], Some(SessionPolicy::Channel)).unwrap_err(),
        "team too large to share: the team description is 4097 bytes (limit 4096)"
    );
}

#[test]
fn invalid_fields_reject_the_whole_publication() {
    let signer = Signer::new();
    let keys = &signer.viewer;
    let agent = |content: &str| publish(&signer, keys, 30175, T0, content, shared_tags("scout"));
    assert!(parse_publication(&agent(r#"{"display_name":"Scout"}"#)).is_some());
    for bad in [
        r#"{"display_name":""}"#,
        r#"{"display_name":"Scout","acp_command":"/bin/sh"}"#,
        r#"{"display_name":"Scout","description":7}"#,
        r#"{"display_name":"Scout","model":"a\u200Bb"}"#,
        r#"[1]"#,
    ] {
        assert_eq!(parse_publication(&agent(bad)), None, "{bad}");
    }
    let slug = publish(
        &signer,
        keys,
        30175,
        T0,
        r#"{"display_name":"S"}"#,
        shared_tags("Scout"),
    );
    assert_eq!(parse_publication(&slug), None);
    let team = |content: &str| publish(&signer, keys, 30178, T0, content, shared_tags("crew"));
    assert!(parse_publication(&team(r#"{"v":1,"name":"Crew","members":[]}"#)).is_some());
    for bad in [
        r#"{"v":2,"name":"Crew","members":[]}"#,
        r#"{"v":1,"name":"Crew"}"#,
        r#"{"v":1,"name":"Crew","members":[{"display_name":"A"}]}"#,
        r#"{"v":1,"name":"Crew","members":[{"member_key":"k","display_name":"A"},{"member_key":"k","display_name":"B"}]}"#,
    ] {
        assert_eq!(parse_publication(&team(bad)), None, "{bad}");
    }
    let content = r#"{"v":1,"name":"Crew","members":[{"member_key":"k","display_name":"A","acp_command":"buzz-goose-acp"}]}"#;
    let listed = parse_publication(&team(content)).unwrap();
    assert_eq!(listed.unsupported_transport(), Some("buzz-goose-acp"));
    assert_eq!(
        unsupported_transport_message("Crew", "buzz-goose-acp"),
        "Crew uses the buzz-goose-acp transport, which this app can't run yet, so it can't be added."
    );
}

#[test]
fn an_unshared_head_hides_older_shared_ones() {
    let signer = Signer::new();
    let content = r#"{"display_name":"Scout"}"#;
    let old = publish(
        &signer,
        &signer.viewer,
        30175,
        T0,
        content,
        shared_tags("scout"),
    );
    let new = publish(
        &signer,
        &signer.viewer,
        30175,
        T0 + 1,
        content,
        vec![vec!["d", "scout"]],
    );
    let mut model = CatalogModel::new(signer.viewer_hex());
    model.loaded(vec![old, new]);
    let view = model.build(&[], true);
    assert!(view.agents.is_empty());
    assert!(!view.state(CatalogKind::Agent, "scout").shared);
}

fn op(keys: &Keys, d: &str, at: u64, shared: bool, delivery: Delivery) -> Arc<Operation> {
    let mut tags = vec![Tag::parse(["d", d]).unwrap()];
    if shared {
        tags.push(Tag::parse(["shared", "true"]).unwrap());
    }
    let mut event = nostr::prelude::UnsignedEvent::new(
        keys.public_key(),
        Timestamp::from_secs(at),
        Kind::from_u16(30175),
        tags,
        r#"{"display_name":"Scout"}"#,
    );
    event.ensure_id();
    Arc::new(Operation {
        event,
        signed: None,
        recovery: None,
        acknowledged: false,
        guarded: false,
        delivery,
        error: None,
    })
}

#[test]
fn share_state_follows_delivery_and_verification() {
    let signer = Signer::new();
    let keys = &signer.viewer;
    let mut model = CatalogModel::new(signer.viewer_hex());
    model.loaded(Vec::new());
    // Queued while sending, and while accepted but not yet the proven head.
    let sending = op(keys, "scout", T0, true, Delivery::Sending);
    let view = model.build(std::slice::from_ref(&sending), true);
    let state = view.state(CatalogKind::Agent, "scout");
    assert!(!state.shared);
    let change = state.change.unwrap();
    assert_eq!(change.delivery, CatalogDelivery::Queued);
    assert!(change.shared && !change.stalled);
    let accepted = op(keys, "scout", T0, true, Delivery::Accepted);
    assert_eq!(model.to_verify(std::slice::from_ref(&accepted)).len(), 1);
    let view = model.build(std::slice::from_ref(&accepted), true);
    assert_eq!(
        view.state(CatalogKind::Agent, "scout")
            .change
            .unwrap()
            .delivery,
        CatalogDelivery::Queued
    );
    // A strong read returning it as the head publishes it at once.
    let head = Head::from_unsigned(&accepted.event).unwrap();
    model.confirmed_read(&head, Some(head.clone()));
    let view = model.build(std::slice::from_ref(&accepted), true);
    let state = view.state(CatalogKind::Agent, "scout");
    assert!(state.shared);
    assert_eq!(state.change.unwrap().delivery, CatalogDelivery::Accepted);
    assert_eq!(view.agents.len(), 1);
    // A read that finds another head leaves it stalled with the reason.
    let unshare = op(keys, "scout", T0 + 5, false, Delivery::Accepted);
    let other =
        Head::from_unsigned(&op(keys, "scout", T0 + 9, true, Delivery::Seen).event).unwrap();
    model.confirmed_read(&Head::from_unsigned(&unshare.event).unwrap(), Some(other));
    let view = model.build(&[accepted, unshare], true);
    // The newer relay head outranks the unshare: it is history now.
    assert!(view.state(CatalogKind::Agent, "scout").change.is_none());
    assert!(view.state(CatalogKind::Agent, "scout").shared);
    // A refused change shows as rejected; unknown ones are stalled.
    let refused = op(keys, "other", T0, true, Delivery::Failed);
    let unknown = op(keys, "third", T0, false, Delivery::Unknown);
    let view = model.build(&[refused, unknown], true);
    assert_eq!(
        view.state(CatalogKind::Agent, "other")
            .change
            .unwrap()
            .delivery,
        CatalogDelivery::Rejected
    );
    assert!(
        view.state(CatalogKind::Agent, "third")
            .change
            .unwrap()
            .stalled
    );
}

#[test]
fn share_notices_follow_the_reference() {
    assert_eq!(
        share_notice("Scout", true, CatalogDelivery::Queued),
        "Sharing Scout is queued. It will appear after the relay accepts the update."
    );
    assert_eq!(
        share_notice("Scout", false, CatalogDelivery::Queued),
        "Removing Scout is queued. It may remain discoverable until the relay accepts the update."
    );
    assert_eq!(
        share_notice("Scout", true, CatalogDelivery::Accepted),
        "Published Scout to the community catalog."
    );
    assert_eq!(
        share_notice("Scout", false, CatalogDelivery::Accepted),
        "Scout is no longer discoverable in the community catalog."
    );
}

#[test]
fn drafts_are_the_owner_to_self_envelope() {
    let draft = catalog_draft(CatalogKind::Team, "crew", "{}".into(), true);
    assert_eq!(draft.kind, 30178);
    assert_eq!(draft.tags, vec![vec!["d", "crew"], vec!["shared", "true"]]);
    let draft = catalog_draft(CatalogKind::Agent, "scout", "{}".into(), false);
    assert_eq!(draft.tags, vec![vec!["d", "scout"]]);
    assert!(catalog_slug("ABC").is_err());
}
