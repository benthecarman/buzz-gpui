use super::*;
use core::prelude::v1::test;
use serde_json::json;

fn pubkey() -> String {
    "a".repeat(64)
}

fn record() -> AgentRecord {
    AgentRecord {
        config: json!({"word": "hi"}),
        ..AgentRecord::blank(&pubkey())
    }
}

fn stored(storage: &MemoryStorage) -> Value {
    serde_json::from_str(&storage.read().unwrap()).unwrap()
}

#[test]
fn round_trips_records_and_keeps_objects_a_reader_would_ignore() {
    let storage = MemoryStorage::default();
    let saved = set_attention(
        &record(),
        "interest/default",
        Some(&json!({"type": "interest", "instructions": "Be brief."})),
        7,
    )
    .unwrap();
    let records = BTreeMap::from([(pubkey(), saved.clone())]);
    write_records(&storage, &records).unwrap();
    assert_eq!(read_records(&storage), records);

    let mut raw = stored(&storage);
    raw["agents"][pubkey()]["attention"]["watch/bad"] =
        json!({"slug": "watch/bad", "value": {"type": "event"}, "modifiedAt": 1});
    raw["agents"]["nonsense"] = json!({"pubkey": "nonsense", "attention": {}});
    storage.write(&raw.to_string()).unwrap();
    let read = read_records(&storage);
    assert_eq!(read[&pubkey()].attention, saved.attention);
    assert!(!read.contains_key("nonsense"));
    // The invalid object is reported, and written back as it was.
    let bad = &read[&pubkey()].skipped["watch/bad"];
    assert_eq!(bad.value, json!({"type": "event"}));
    assert!(bad.problem.contains("Missing"), "{}", bad.problem);
    write_records(&storage, &read).unwrap();
    assert_eq!(
        stored(&storage)["agents"][pubkey()]["attention"]["watch/bad"],
        json!({"slug": "watch/bad", "value": {"type": "event"}, "modifiedAt": 1})
    );
    // Removing it clears it from the skipped set.
    let fixed = set_attention(&read[&pubkey()], "watch/bad", None, 2).unwrap();
    assert!(fixed.skipped.is_empty());
}

#[test]
fn skips_objects_over_the_count_limit_in_slug_order_and_refuses_more() {
    let storage = MemoryStorage::default();
    let attention: Map<String, Value> = (0..101)
        .map(|n| {
            let slug = format!("interest/i{n:03}");
            (
                slug.clone(),
                json!({"slug": slug, "value": {"type": "interest", "instructions": "x"}, "modifiedAt": 1}),
            )
        })
        .collect();
    storage
        .write(
            &json!({"version": 1, "agents": {pubkey(): {"pubkey": pubkey(), "attention": attention}}})
                .to_string(),
        )
        .unwrap();
    let read = read_records(&storage).remove(&pubkey()).unwrap();
    assert_eq!(read.attention.len(), 100);
    assert_eq!(
        read.skipped.keys().collect::<Vec<_>>(),
        vec!["interest/i100"]
    );
    let interest = json!({"type": "interest", "instructions": "x"});
    let error = set_attention(&read, "interest/new", Some(&interest), 1).unwrap_err();
    assert!(error.contains("at most 100"), "{error}");
    let mut full = (*read.attention).clone();
    full.remove("interest/i099");
    let room = AgentRecord {
        attention: Arc::new(full),
        ..read.clone()
    };
    assert!(set_attention(&room, "interest/new", Some(&interest), 1).is_ok());
    // Replacing an existing object is not adding one.
    assert!(set_attention(&read, "interest/i000", Some(&interest), 1).is_ok());
}

#[test]
fn keeps_a_timer_run_state_only_while_its_schedule_is_unchanged() {
    let timer = json!({
        "type": "timer", "interest_id": "x", "prompt": "tick", "enabled": true,
        "interval_secs": 10, "armed_at": 100, "max_occurrences": null, "expires_at": null,
    });
    let armed = set_attention(&record(), "watch/t", Some(&timer), 100).unwrap();
    assert_eq!(
        armed.timers["watch/t"],
        TimerState {
            armed_at: 100,
            next_due: 110,
            used: 0
        }
    );
    let ran = AgentRecord {
        timers: Arc::new(BTreeMap::from([(
            "watch/t".to_owned(),
            TimerState {
                armed_at: 100,
                next_due: 125,
                used: 1,
            },
        )])),
        ..armed
    };
    let with = |key: &str, value: Value| {
        let mut timer = timer.clone();
        timer[key] = value;
        timer
    };
    // A new prompt keeps the count; pausing, or a new interval, recounts.
    assert_eq!(
        set_attention(&ran, "watch/t", Some(&with("prompt", json!("tock"))), 140)
            .unwrap()
            .timers,
        ran.timers
    );
    assert_eq!(
        set_attention(&ran, "watch/t", Some(&with("enabled", json!(false))), 140)
            .unwrap()
            .timers["watch/t"],
        TimerState {
            armed_at: 100,
            next_due: 150,
            used: 4
        }
    );
    assert_eq!(
        set_attention(
            &ran,
            "watch/t",
            Some(&with("interval_secs", json!(20))),
            140
        )
        .unwrap()
        .timers["watch/t"],
        TimerState {
            armed_at: 100,
            next_due: 160,
            used: 2
        }
    );
    // Timer state of a removed timer goes with it.
    assert!(
        set_attention(&ran, "watch/t", None, 141)
            .unwrap()
            .timers
            .is_empty()
    );
}

#[test]
fn validates_replaces_and_deletes_one_object_at_a_time() {
    let error = set_attention(
        &record(),
        "interest/x",
        Some(&json!({"type": "interest", "instructions": " "})),
        1,
    )
    .unwrap_err();
    assert!(error.contains("empty"), "{error}");
    let one = set_attention(
        &record(),
        "interest/x",
        Some(&json!({"type": "interest", "instructions": "a"})),
        1,
    )
    .unwrap();
    let two = set_attention(
        &one,
        "interest/x",
        Some(&json!({"type": "interest", "instructions": "b"})),
        2,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&two.attention["interest/x"]).unwrap(),
        json!({"slug": "interest/x", "value": {"type": "interest", "instructions": "b"}, "modifiedAt": 2})
    );
    assert!(
        set_attention(&two, "interest/x", None, 3)
            .unwrap()
            .attention
            .is_empty()
    );
}

#[test]
fn reads_nothing_from_missing_or_malformed_storage() {
    let storage = MemoryStorage::default();
    assert!(read_records(&storage).is_empty());
    storage.write("{").unwrap();
    assert!(read_records(&storage).is_empty());
    storage.write(r#"{"version": 2, "agents": {}}"#).unwrap();
    assert!(read_records(&storage).is_empty());
}

#[test]
fn the_agents2_records_move_to_the_agents_file() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("buzz.agents2.v1.json");
    let new = dir.path().join("buzz.agents.v1.json");
    // Nothing to move.
    migrate_storage(dir.path());
    assert!(!new.exists());
    std::fs::write(&old, "{\"version\":1}").unwrap();
    migrate_storage(dir.path());
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "{\"version\":1}");
    assert!(!old.exists());
    // The same data again: the old file goes.
    std::fs::write(&old, "{\"version\":1}").unwrap();
    migrate_storage(dir.path());
    assert!(!old.exists());
    // Different data: neither is touched.
    std::fs::write(&old, "{\"other\":true}").unwrap();
    migrate_storage(dir.path());
    assert_eq!(std::fs::read_to_string(&old).unwrap(), "{\"other\":true}");
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "{\"version\":1}");
}
