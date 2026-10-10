use super::*;
use core::prelude::v1::test;
use serde_json::json;

const CHANNEL: &str = "0b8e2a3c-1d4f-4a5b-8c6d-7e8f9a0b1c2d";

fn bot() -> String {
    "c".repeat(64)
}

fn event(id: &str, patch: impl FnOnce(&mut EventData)) -> Arc<EventData> {
    let mut event = EventData {
        id: format!("{id:0<64}"),
        pubkey: "b".repeat(64),
        created_at: 100,
        kind: 9,
        content: format!("hello {id}"),
        tags: vec![vec!["h".into(), CHANNEL.into()]],
    };
    patch(&mut event);
    Arc::new(event)
}

fn attention(objects: Value) -> BTreeMap<String, AttentionObject> {
    let record = AgentRecord::blank(&bot());
    let mut record = record;
    for (slug, value) in objects.as_object().unwrap() {
        record = set_attention(&record, slug, Some(value), 1).unwrap();
    }
    (*record.attention).clone()
}

fn kinds(triggers: &[Trigger]) -> Vec<&'static str> {
    triggers
        .iter()
        .map(|trigger| match trigger {
            Trigger::Mention { .. } => "mention",
            Trigger::Watch { .. } => "watch",
            Trigger::Timer { .. } => "timer",
        })
        .collect()
}

#[test]
fn the_queue_holds_32_and_admits_60_a_minute() {
    let start = Instant::now();
    let mut window = Window {
        start: None,
        admitted: 0,
    };
    for _ in 0..QUEUE_LIMIT {
        assert!(admit(&mut window, 0, start));
    }
    assert!(
        !admit(&mut window, QUEUE_LIMIT, start),
        "a full queue drops"
    );
    for _ in QUEUE_LIMIT..RATE_LIMIT as usize {
        assert!(admit(&mut window, 0, start));
    }
    assert!(!admit(&mut window, 0, start), "the 61st in a minute drops");
    assert!(
        !admit(&mut window, 0, start + Duration::from_secs(59)),
        "still the same minute"
    );
    assert!(
        admit(&mut window, 0, start + RATE_WINDOW),
        "a new minute admits again"
    );
    assert_eq!(window.admitted, 1);
}

#[test]
fn mentions_wake_and_addressed_events_skip_watches() {
    let agent = bot();
    let watches = compile(&attention(json!({
        "interest/default": {"type": "interest", "instructions": "Watch."},
        "watch/all": {"type": "event", "interest_id": "default", "enabled": true,
            "since": 1, "channels": "all", "kinds": []},
    })));
    let none = |_: &str| false;
    let mention = event("m", |e| e.tags.push(vec!["p".into(), agent.clone()]));
    assert_eq!(
        kinds(&heard(&agent, &mention, false, &none, &watches)),
        ["mention"]
    );
    // A reaction to its message is addressed: no watch, and no run.
    let mine = |id: &str| id == "mine";
    let reaction = event("r", |e| {
        e.kind = 7;
        e.tags = vec![vec!["e".into(), "mine".into()]];
    });
    assert!(heard(&agent, &reaction, false, &mine, &watches).is_empty());
    // Anything else reaches the watches, one run per match.
    let plain = event("p", |_| {});
    let triggers = heard(&agent, &plain, false, &none, &watches);
    assert_eq!(kinds(&triggers), ["watch"]);
    let Trigger::Watch { slug, interest, .. } = &triggers[0] else {
        panic!("watch");
    };
    assert_eq!(slug, "watch/all");
    assert_eq!(interest.as_ref().unwrap().instructions, "Watch.");
}

#[test]
fn an_owners_agents_do_not_answer_each_other_forever() {
    let agent = bot();
    let watches = compile(&attention(json!({
        "watch/all": {"type": "event", "interest_id": "default", "enabled": true,
            "since": 1, "channels": "all", "kinds": []},
    })));
    let mine = |id: &str| id == "mine";
    let named = event("n", |e| e.tags.push(vec!["p".into(), agent.clone()]));
    assert_eq!(
        kinds(&heard(&agent, &named, true, &mine, &watches)),
        ["mention"]
    );
    // A sibling's reply to it is not heard, even naming it.
    let reply = event("r", |e| {
        e.tags.push(vec!["p".into(), agent.clone()]);
        e.tags.push(vec![
            "e".into(),
            "mine".into(),
            String::new(),
            "reply".into(),
        ]);
    });
    assert!(heard(&agent, &reply, true, &mine, &watches).is_empty());
    // Nor anything a watch would match.
    assert!(heard(&agent, &event("w", |_| {}), true, &mine, &watches).is_empty());
}

#[test]
fn disabled_watches_and_bad_filters_never_match() {
    let watches = compile(&attention(json!({
        "watch/off": {"type": "event", "interest_id": "default", "enabled": false,
            "since": 1, "channels": "all", "kinds": []},
        "watch/go": {"type": "event", "interest_id": "default", "enabled": true,
            "since": 1, "channels": "all", "kinds": [9], "filter": "content == \"go\""},
    })));
    assert_eq!(watches.len(), 1);
    let none = |_: &str| false;
    assert!(heard(&bot(), &event("x", |_| {}), false, &none, &watches).is_empty());
    let go = event("g", |e| e.content = "go".into());
    assert_eq!(
        kinds(&heard(&bot(), &go, false, &none, &watches)),
        ["watch"]
    );
}

#[test]
fn due_timers_run_once_and_the_longest_waiting_first() {
    let objects = attention(json!({
        "watch/a": {"type": "timer", "interest_id": "default", "prompt": "a", "enabled": true,
            "interval_secs": 60, "armed_at": 1000, "max_occurrences": 2, "expires_at": null},
        "watch/b": {"type": "timer", "interest_id": "default", "prompt": "b", "enabled": true,
            "interval_secs": 30, "armed_at": 1000, "max_occurrences": null, "expires_at": null},
    }));
    let start = BTreeMap::from([
        (
            "watch/a".to_owned(),
            timer_state(
                match &objects["watch/a"].value {
                    AttentionValue::Timer(timer) => timer,
                    _ => unreachable!(),
                },
                None,
                1000,
            ),
        ),
        (
            "watch/b".to_owned(),
            TimerState {
                armed_at: 1000,
                next_due: 1030,
                used: 0,
            },
        ),
    ]);
    // Nothing is due before its first occurrence.
    let (_, none) = due_timer(&objects, &start, 1010);
    assert!(none.is_none());
    // b is due first (1030), then a (1060).
    let (timers, next) = due_timer(&objects, &start, 1061);
    assert_eq!(next.unwrap().0, "watch/b");
    assert_eq!(
        timers["watch/b"],
        TimerState {
            armed_at: 1000,
            next_due: 1091,
            used: 1
        },
        "one run when overdue, the next an interval after it"
    );
    let (timers, next) = due_timer(&objects, &timers, 1061);
    assert_eq!(next.unwrap().0, "watch/a");
    // Both overdue: b has waited longest, then a runs its second and last.
    let (timers, next) = due_timer(&objects, &timers, 1200);
    assert_eq!(next.unwrap().0, "watch/b");
    let (timers, next) = due_timer(&objects, &timers, 1200);
    assert_eq!(next.unwrap().0, "watch/a");
    assert_eq!(timers["watch/a"].used, 2);
    let (timers, next) = due_timer(&objects, &timers, 1200);
    assert!(next.is_none(), "one run each when overdue");
    let (_, next) = due_timer(&objects, &timers, 5000);
    assert_eq!(next.unwrap().0, "watch/b", "a is spent");
}

#[test]
fn a_timer_that_stops_after_one_run_runs_once() {
    let objects = attention(json!({
        "watch/t": {"type": "timer", "interest_id": "default", "prompt": "t", "enabled": true,
            "interval_secs": 60, "armed_at": 1000, "max_occurrences": 1, "expires_at": null},
    }));
    // Armed through the editor, which saves its state.
    let AttentionValue::Timer(timer) = &objects["watch/t"].value else {
        unreachable!()
    };
    let mut timers = BTreeMap::from([("watch/t".to_owned(), timer_state(timer, None, 1000))]);
    let mut runs = 0;
    for now in (1000..2000).step_by(5) {
        let (next, due) = due_timer(&objects, &timers, now);
        timers = next;
        runs += usize::from(due.is_some());
    }
    assert_eq!(runs, 1);
    assert_eq!(timers["watch/t"].used, 1);
}

#[test]
fn a_timer_without_state_does_not_run_for_what_it_missed() {
    let objects = attention(json!({
        "watch/t": {"type": "timer", "interest_id": "default", "prompt": "t", "enabled": true,
            "interval_secs": 10, "armed_at": 100, "max_occurrences": 1, "expires_at": null},
    }));
    // Saved state is gone: the occurrences due by now count as used.
    let (timers, next) = due_timer(&objects, &BTreeMap::new(), 135);
    assert!(next.is_none());
    assert_eq!(timers["watch/t"].used, 3);
}

#[test]
fn type_defaults_become_the_first_record() {
    struct Kind;
    impl registry::AgentType for Kind {
        fn id(&self) -> &'static str {
            "kind"
        }
        fn title(&self) -> gpui_kit::SharedString {
            "Kind".into()
        }
        fn defaults(&self) -> registry::TypeDefaults {
            registry::TypeDefaults {
                config: json!({"model": "x"}),
                attention: [(
                    "interest/default".to_owned(),
                    AttentionValue::Interest(attention::Interest {
                        instructions: "Help.".into(),
                    }),
                )]
                .into(),
            }
        }
    }
    let kind = RegisteredType {
        key: "p/kind".into(),
        kind: Arc::new(Kind),
    };
    let record = defaults(&bot(), &kind).unwrap();
    assert_eq!(record.config, json!({"model": "x"}));
    assert!(record.attention.contains_key("interest/default"));
    assert!(registry::validate("p", &Kind).is_ok());
    assert!(registry::validate("P!", &Kind).is_err());
}
