//! buzz-app's `transcript.test.ts`.

use super::*;
use crate::agent_plugins::codex::rpc::testing::FakeCodex;
use core::prelude::v1::test;
use std::sync::Arc;

const ROOT: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn place() -> Place {
    Place {
        channel_id: "channel".into(),
        root: Some(ROOT.into()),
    }
}

const INPUT: &str = "<context>\n{\"channel_id\":\"channel\"}\n</context>\n<buzz-event>\n{\"id\":\"e\",\"pubkey\":\"sam\"}\nRequest: \"fix the \\u003ctests\\u003e\"\n</buzz-event>";

fn turn(patch: Value) -> Value {
    let mut turn = json!({
        "id": "turn-1",
        "status": "completed",
        "error": null,
        "startedAt": 100,
        "completedAt": 160,
        "items": [
            { "type": "userMessage", "id": "u1", "content": [{ "type": "text", "text": INPUT }] },
            { "type": "reasoning", "id": "r1", "summary": ["Looking at tests"] },
            {
                "type": "commandExecution",
                "id": "c1",
                "command": "pnpm test",
                "aggregatedOutput": "1 failed",
                "status": "failed",
                "durationMs": 2000,
            },
            {
                "type": "userMessage",
                "id": "u2",
                "content": [{
                    "type": "text",
                    "text": "<new-message-arrived-while-you-were-working>\n{}\nRequest: \"also lint\"\n</new-message-arrived-while-you-were-working>",
                }],
            },
            {
                "type": "fileChange",
                "id": "f1",
                "status": "completed",
                "changes": [{ "path": "a.ts", "kind": "update", "diff": "-a\n+b" }],
            },
            { "type": "agentMessage", "id": "a1", "text": "Fixed." },
            { "type": "contextCompaction", "id": "x1" },
        ],
    });
    if let (Value::Object(turn), Value::Object(patch)) = (&mut turn, patch) {
        turn.extend(patch);
    }
    turn
}

#[test]
fn shows_a_codex_turns_request_steering_tools_and_reply() {
    let mapped = &codex_turns(&[turn(json!({}))], &place())[0];
    assert_eq!(mapped.turn_id, "turn-1");
    assert_eq!(mapped.channel_id.as_deref(), Some("channel"));
    assert_eq!(mapped.thread_root, Some(Some(ROOT.into())));
    assert_eq!(
        (mapped.started_at, mapped.ended_at),
        (100_000, Some(160_000))
    );
    let items = &mapped.items;
    assert_eq!(items.len(), 6);
    match &items[0].kind {
        ItemKind::Prompt {
            text,
            sections,
            steer,
            ..
        } => {
            assert_eq!(text, "fix the <tests>");
            assert!(!steer);
            let tags: Vec<_> = sections.iter().map(|s| s.tag.as_str()).collect();
            assert_eq!(tags, ["context", "buzz-event"]);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&items[1].kind, ItemKind::Thought { text, .. } if text == "Looking at tests"));
    match &items[2].kind {
        ItemKind::Tool(tool) => {
            assert_eq!(
                (
                    tool.title.as_str(),
                    tool.kind.as_str(),
                    tool.status.as_str(),
                    tool.output.as_str()
                ),
                ("pnpm test", "execute", "failed", "1 failed")
            );
            assert_eq!(tool.completed_at, Some(102_000));
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(&items[3].kind, ItemKind::Prompt { text, steer: true, .. } if text == "also lint")
    );
    match &items[4].kind {
        ItemKind::Tool(tool) => {
            assert_eq!(tool.kind, "edit");
            assert_eq!(tool.paths, ["a.ts"]);
            assert_eq!(tool.output, "-a\n+b");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&items[5].kind, ItemKind::Message { text, .. } if text == "Fixed."));
    let channel = &codex_turns(
        &[turn(
            json!({ "status": "interrupted", "completedAt": null, "items": [] }),
        )],
        &Place {
            channel_id: "channel".into(),
            root: None,
        },
    )[0];
    assert_eq!(channel.thread_root, Some(None));
    assert_eq!(channel.stop_reason.as_deref(), Some("interrupted"));
    assert_eq!(channel.ended_at, None);
}

fn answer(
    fake: &FakeCodex,
    method: &str,
    answer: impl Fn(&Value) -> Result<Value, String> + Send + Sync + 'static,
) {
    fake.state().answers.insert(method.into(), Arc::new(answer));
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_the_latest_turns_oldest_first_and_closes_its_app_server() {
    let fake = FakeCodex::default();
    answer(&fake, "thread/turns/list", |_| {
        Ok(json!({
            "data": [turn(json!({ "id": "turn-2", "status": "inProgress", "completedAt": null })), turn(json!({}))],
            "nextCursor": "more",
        }))
    });
    let read = read_transcript(&fake.spawner(), "thread-1", &place(), &Signal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fake.spawned("app-server").len(), 1);
    assert_eq!(
        fake.calls("thread/turns/list")[0]["params"],
        json!({ "threadId": "thread-1", "limit": TURN_LIMIT, "sortDirection": "desc", "itemsView": "full" })
    );
    let ids: Vec<_> = read
        .transcript
        .turns
        .iter()
        .map(|turn| turn.turn_id.as_str())
        .collect();
    assert_eq!(ids, ["turn-1", "turn-2"]);
    assert!(read.more && read.working);
    assert_eq!(fake.state().ended, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn treats_a_thread_codex_no_longer_has_as_missing_and_other_failures_as_errors() {
    let gone = FakeCodex::default();
    answer(&gone, "thread/turns/list", |_| {
        Err("thread not loaded: thread-1".into())
    });
    assert_eq!(
        read_transcript(&gone.spawner(), "thread-1", &place(), &Signal::default()).await,
        Ok(None)
    );
    let broken = FakeCodex::default();
    answer(&broken, "thread/turns/list", |_| {
        Err("database is locked".into())
    });
    assert_eq!(
        read_transcript(&broken.spawner(), "thread-1", &place(), &Signal::default()).await,
        Err("database is locked".into())
    );
}
