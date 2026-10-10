use super::payload::*;
use super::setup::*;
use super::*;
use crate::event::Verifier;
use buzz_identity::Purpose;
use core::prelude::v1::test;

const COMMUNITY: &str = "https://relay.test";

fn key(i: u8) -> String {
    format!("{i:02x}").repeat(32)
}

fn template_value() -> Value {
    json!({"version": 1, "community": COMMUNITY, "deleted": false, "value": {
        "type": "template", "id": "t-1", "name": "Launch", "description": "",
        "teamIds": ["team-a"], "agents": [key(1)], "canvas": "# Goal"
    }})
}

fn signed(identity: &Identity, record: &KitRecord, extra: Vec<Vec<String>>) -> Verified {
    let mut draft = draft(identity, record).unwrap();
    draft.tags.extend(extra);
    let unsigned = identity
        .template(
            Purpose::AppData,
            draft.kind,
            1_700_000_000,
            draft.tags,
            draft.content,
        )
        .unwrap();
    let event = identity.sign(Purpose::AppData, unsigned).unwrap();
    Verifier::default().verify(event).unwrap()
}

#[test]
fn templates_parse_with_their_native_coordinate() {
    let record = parse_kit_record(&template_value(), COMMUNITY).unwrap();
    assert_eq!(
        record.coordinate(),
        "buzz-channel-kit-v1:https%3A%2F%2Frelay.test:template:t-1"
    );
    assert_eq!(record.tag(), KIT_TAG);
    let KitValue::Template(template) = &record.value else {
        panic!("a template")
    };
    assert_eq!(template.lineup().team_ids, ["team-a"]);
    // Round trip through serde keeps the exact fields the native rules allow.
    assert_eq!(
        parse_kit_record(&record.to_json(), COMMUNITY).unwrap(),
        record
    );
}

#[test]
fn records_from_another_community_or_with_bad_selections_are_refused() {
    assert!(parse_kit_record(&template_value(), "https://other.test").is_err());
    let mut extra = template_value();
    extra["value"]["color"] = json!("red");
    // The model ignores unknown fields, as the TypeScript one does; the
    // native cipher boundary refuses them (see the cipher test).
    assert!(
        !parse_kit_record(&extra, COMMUNITY).unwrap().to_json()["value"]
            .as_object()
            .unwrap()
            .contains_key("color")
    );
    let mut bad = template_value();
    bad["value"]["agents"] = json!(["nope"]);
    assert_eq!(
        parse_kit_record(&bad, COMMUNITY).unwrap_err(),
        "Invalid channel recipe selection"
    );
    let mut duplicate = template_value();
    duplicate["value"]["agents"] = json!([key(1), key(1)]);
    assert!(parse_kit_record(&duplicate, COMMUNITY).is_err());
    let mut blank = template_value();
    blank["value"]["name"] = json!("  ");
    assert!(parse_kit_record(&blank, COMMUNITY).is_err());
}

#[test]
fn canvas_and_record_budgets_have_their_messages() {
    let mut large = template_value();
    large["value"]["canvas"] = json!("é".repeat(13 * 1024));
    assert_eq!(
        parse_kit_record(&large, COMMUNITY).unwrap_err(),
        "Canvas is too large (24 KiB maximum)"
    );
    let mut over = template_value();
    over["value"]["canvas"] = json!("x".repeat(17 * 1024));
    assert_eq!(parse_kit_record(&over, COMMUNITY).unwrap_err(), TOO_LARGE);
}

#[test]
fn groups_records_follow_the_personal_and_me_rules() {
    let channel = "11111111-1111-4111-8111-111111111111";
    let personal = json!({"version": 1, "community": COMMUNITY, "deleted": false, "value": {
        "type": "groups", "id": "personal",
        "groups": [{"id": "g1", "name": "Work", "defaultTemplateId": "t-1"}],
        "assignments": {channel: "g1"}
    }});
    let record = parse_kit_record(&personal, COMMUNITY).unwrap();
    assert_eq!(record.tag(), KIT_TAG);
    let mut me = personal.clone();
    me["value"]["id"] = json!("me");
    me["value"]["channels"] = json!([channel]);
    let me = parse_kit_record(&me, COMMUNITY).unwrap();
    assert_eq!(me.tag(), ME_KIT_TAG);
    let mut placed = personal.clone();
    placed["value"]["channels"] = json!([channel]);
    assert_eq!(
        parse_kit_record(&placed, COMMUNITY).unwrap_err(),
        "Messages groups cannot contain Me placement"
    );
    let mut stray = personal.clone();
    stray["value"]["assignments"] = json!({channel: "missing"});
    assert_eq!(
        parse_kit_record(&stray, COMMUNITY).unwrap_err(),
        "Invalid personal group placement"
    );
    let entries = [KitEntry {
        record,
        event_id: EventId::from_hex(&"0".repeat(64)).unwrap(),
        created_at: 1,
    }];
    assert_eq!(personal_groups(&entries).unwrap().1.groups[0].name, "Work");
}

#[test]
fn portable_teams_need_version_two_and_a_manifest() {
    let manifest = json!({"version": 1, "owner": key(9), "revision": "11111111-1111-4111-8111-111111111111",
        "digest": key(8), "bytes": 20000, "chunks": 2});
    let team = json!({"version": 2, "community": COMMUNITY, "deleted": false, "value": {
        "type": "team", "id": "team-a", "name": "Squad", "agents": [key(1)], "portable": manifest
    }});
    let record = parse_kit_record(&team, COMMUNITY).unwrap();
    assert_eq!(record.tag(), MANIFEST_TAG);
    let mut v1 = team.clone();
    v1["version"] = json!(1);
    assert_eq!(
        parse_kit_record(&v1, COMMUNITY).unwrap_err(),
        "Portable teams require version 2"
    );
    let mut empty = team.clone();
    empty["value"]["agents"] = json!([]);
    assert_eq!(
        parse_kit_record(&empty, COMMUNITY).unwrap_err(),
        "Unsupported portable recipe"
    );
    let mut wrong = team;
    wrong["value"]["portable"]["chunks"] = json!(3);
    assert_eq!(
        parse_kit_record(&wrong, COMMUNITY).unwrap_err(),
        "Invalid portable team manifest"
    );
}

#[test]
fn the_cipher_round_trips_only_the_viewers_admitted_records() {
    let alice = Identity::ephemeral();
    let record = parse_kit_record(&template_value(), COMMUNITY).unwrap();
    let event = signed(&alice, &record, vec![vec!["client-id".into(), "x".into()]]);
    let entry = decode_entry(&alice, &event, COMMUNITY).unwrap();
    assert_eq!(entry.record, record);
    // Another viewer cannot open it, and an extra tag fails admission.
    let bob = Identity::ephemeral();
    assert!(open(&bob, &event, COMMUNITY).is_err());
    let tagged = signed(&alice, &record, vec![vec!["p".into(), key(2)]]);
    assert!(open(&alice, &tagged, COMMUNITY).is_err());
    // Nothing the native rules refuse is ever encrypted.
    let mut extra = template_value();
    extra["value"]["x"] = json!(1);
    assert!(seal(&alice, &extra, COMMUNITY).is_err());
}

#[test]
fn catalog_heads_keep_the_newest_per_coordinate_in_this_community() {
    let alice = Identity::ephemeral();
    let record = parse_kit_record(&template_value(), COMMUNITY).unwrap();
    let older = signed(&alice, &record, vec![]);
    let draft = draft(&alice, &record).unwrap();
    let newer = {
        let unsigned = alice
            .template(
                Purpose::AppData,
                draft.kind,
                1_700_000_100,
                draft.tags,
                draft.content,
            )
            .unwrap();
        Verifier::default()
            .verify(alice.sign(Purpose::AppData, unsigned).unwrap())
            .unwrap()
    };
    let other = parse_kit_record(
        &json!({"version": 1, "community": "https://other.test", "deleted": false, "value": {
            "type": "team", "id": "x", "name": "X", "agents": []}}),
        "https://other.test",
    )
    .unwrap();
    let foreign = signed(&alice, &other, vec![]);
    let events = vec![older, newer.clone(), foreign];
    let heads = catalog_heads(&events, COMMUNITY);
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].id, newer.id);
}

#[test]
fn lineups_resolve_teams_and_report_unavailable_members() {
    let team = KitEntry {
        record: KitRecord::new(
            KitValue::Team(Team {
                id: "team-a".into(),
                name: "A".into(),
                agents: vec![key(1), key(2)],
                portable: None,
            }),
            COMMUNITY,
            false,
        ),
        event_id: EventId::from_hex(&"0".repeat(64)).unwrap(),
        created_at: 1,
    };
    let choice = |i: u8| AgentChoice {
        pubkey: key(i),
        name: format!("Agent {i}"),
        avatar: None,
    };
    let lineup = Lineup {
        team_ids: vec!["team-a".into()],
        agents: vec![key(2), key(3)],
        canvas: String::new(),
    };
    let agents = [choice(1), choice(2), choice(3)];
    let resolved = resolve_lineup(&lineup, std::slice::from_ref(&team), &agents).unwrap();
    let keys: Vec<_> = resolved.iter().map(|a| a.pubkey.clone()).collect();
    assert_eq!(keys, [key(2), key(3), key(1)], "distinct, agents first");
    assert!(
        resolve_lineup(&lineup, &[], &agents)
            .unwrap_err()
            .starts_with("A selected team is unavailable (team-a)")
    );
    let missing = resolve_lineup(&lineup, &[team], &agents[..2]).unwrap_err();
    assert!(missing.starts_with("Agent npub1"), "{missing}");
}

#[test]
fn team_payloads_split_and_rejoin_with_their_digest() {
    let owner = key(9);
    let revision = "11111111-1111-4111-8111-111111111111";
    let snapshot = json!({
        "format": "buzz-team-snapshot", "version": 1,
        "team": {"name": "Squad"},
        "members": [{"name": "a", "prompt": "x".repeat(40_000), "memory": {"level": "all", "entries": [1]}}]
    });
    let (manifest, payloads) =
        encode_team_payload(&snapshot, COMMUNITY, &owner, "team-a", revision).unwrap();
    assert_eq!(manifest.chunks, 3);
    assert_eq!(payloads.len(), 3);
    let decoded = decode_team_payload(&manifest, &payloads, COMMUNITY, &owner, "team-a").unwrap();
    assert_eq!(
        decoded["members"][0]["memory"],
        json!({"level": "none", "entries": []})
    );
    assert_eq!(
        decoded["members"][0]["prompt"],
        snapshot["members"][0]["prompt"]
    );
    // Each chunk record parses back to itself under its coordinate.
    let record = payloads[1].record();
    assert_eq!(
        parse_payload_record(&record, COMMUNITY).unwrap(),
        payloads[1]
    );
    assert_eq!(
        buzz_desktop::relay::kit::coordinate(&record, COMMUNITY).unwrap(),
        payloads[1].coordinate()
    );
    // A swapped chunk or a changed byte fails.
    let mut swapped = payloads.clone();
    swapped.swap(0, 1);
    assert!(decode_team_payload(&manifest, &swapped, COMMUNITY, &owner, "team-a").is_err());
    let mut tampered = payloads.clone();
    tampered[2].data = STANDARD_ENCODE(b"{}");
    assert!(decode_team_payload(&manifest, &tampered, COMMUNITY, &owner, "team-a").is_err());
    assert!(decode_team_payload(&manifest, &payloads[..2], COMMUNITY, &owner, "team-a").is_err());
    assert!(
        decode_team_payload(&manifest, &payloads, COMMUNITY, &key(7), "team-a").is_err(),
        "another owner"
    );
}

#[allow(non_snake_case)]
fn STANDARD_ENCODE(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn creation_input_checks_details_and_setup() {
    let channel = crate::lifecycle::NewChannel {
        name: " #launch ".into(),
        description: " ".into(),
        private: true,
        ttl: Some(3600),
    };
    let input = ChannelCreationInput::from_new(&channel, Some(ChannelSetup::default()));
    assert_eq!(input.name, "launch");
    assert_eq!(input.description, None);
    assert_eq!(input.setup, None, "an empty setup is no setup");
    assert!(input.check().is_ok());
    let mut bad = input.clone();
    bad.setup = Some(ChannelSetup {
        template_id: "bad id".into(),
        ..ChannelSetup::default()
    });
    assert!(bad.check().is_err());
    bad.setup = Some(ChannelSetup {
        agents: vec![key(1)],
        canvas: "x".repeat(CANVAS_BYTES + 1),
        ..ChannelSetup::default()
    });
    assert!(bad.check().is_err());
    let mut session = input;
    session.description = Some("Buzz session (x)".into());
    assert!(session.check().is_err());
}

#[test]
fn receipts_round_trip_within_their_budget() {
    let store = crate::Store::in_memory().unwrap();
    let partition = crate::store::Partition {
        origin: COMMUNITY.into(),
        viewer: key(1),
    };
    let input = ChannelCreationInput::from_new(
        &crate::lifecycle::NewChannel {
            name: "launch".into(),
            description: String::new(),
            private: false,
            ttl: None,
        },
        None,
    );
    let mut progress = Progress::new(input);
    Receipts::write(&store, &partition, &progress).unwrap();
    assert!(
        !Receipts::load(&store, &partition)
            .unwrap()
            .admitted(&progress.id)
    );
    progress.created = true;
    Receipts::write(&store, &partition, &progress).unwrap();
    assert!(
        Receipts::load(&store, &partition)
            .unwrap()
            .admitted(&progress.id)
    );
    let mut huge = progress.clone();
    huge.added = (0..2000).map(|_| key(3)).collect();
    assert_eq!(
        Receipts::write(&store, &partition, &huge).unwrap_err(),
        "Channel setup exceeds its local receipt budget"
    );
    Receipts::remove(&store, &partition, &progress.id).unwrap();
    assert!(Receipts::load(&store, &partition).unwrap().0.is_empty());
}

#[test]
fn creations_restore_only_from_this_apps_command_shape() {
    let identity = Identity::ephemeral();
    let id = "11111111-1111-4111-8111-111111111111";
    let channel = crate::lifecycle::NewChannel {
        name: "launch".into(),
        description: "Plans".into(),
        private: false,
        ttl: Some(600),
    };
    let draft = crate::lifecycle::create(&channel, id).unwrap();
    let mut tags = draft.tags;
    tags.push(vec!["client-id".into(), "abc".into()]);
    let op = |tags: Vec<Vec<String>>| crate::outbox::Operation {
        event: identity
            .template(Purpose::Content, draft.kind, 1, tags, String::new())
            .unwrap(),
        signed: None,
        recovery: None,
        acknowledged: false,
        guarded: false,
        delivery: crate::outbox::Delivery::Unknown,
        error: None,
    };
    let pending = pending_creation(&op(tags.clone())).unwrap();
    assert_eq!(pending.id, id);
    assert_eq!(pending.input.description.as_deref(), Some("Plans"));
    assert_eq!(pending.input.ttl_seconds, Some(600));
    let mut reordered = tags.clone();
    reordered.swap(4, 5);
    assert!(pending_creation(&op(reordered)).is_none());
    let mut session = tags;
    session[4][1] = "Buzz session (x)".into();
    assert!(pending_creation(&op(session)).is_none());
}
