//! buzz-app's `transcript.test.ts`.

use super::*;
use crate::agent_plugins::process::{Process, ProcessControl, Spawn, Spawned};
use base64::Engine;
use buzz_host::api::ProcessOutput;
use core::prelude::v1::test;
use futures::future::BoxFuture;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const ROOT: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn conversation() -> Conversation {
    Conversation {
        session_id: "session-1".into(),
        channel_id: "channel".into(),
        root: Some(ROOT.into()),
    }
}

fn prompt_text(content: &str) -> String {
    format!(
        "<context>\nScope: thread\n</context>\n\n<buzz-event type=\"mention\">\nEvent ID: e\nFrom: Sam (npub: npub1x, hex: {})\nContent: {content}\nTags: []\n</buzz-event>",
        "a".repeat(64)
    )
}

/// Session lines, timestamped a second apart from `clock`.
struct Lines {
    clock: u64,
}

impl Lines {
    fn new(clock: u64) -> Self {
        Self { clock }
    }
    fn line(&mut self, kind: &str, content: Value, extra: Value) -> String {
        self.clock += 1;
        let mut entry = json!({
            "type": kind,
            "uuid": format!("u{}", self.clock),
            "timestamp": chrono::DateTime::from_timestamp(self.clock as i64, 0)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "message": { "role": kind, "content": content },
        });
        if let (Value::Object(entry), Value::Object(extra)) = (&mut entry, extra) {
            entry.extend(extra);
        }
        entry.to_string()
    }
}

fn file() -> String {
    let mut l = Lines::new(0);
    [
        json!({ "type": "queue-operation" }).to_string(),
        l.line("user", json!(prompt_text("fix the tests")), json!({})),
        l.line(
            "assistant",
            json!([{ "type": "thinking", "thinking": "Run them first" }]),
            json!({}),
        ),
        l.line(
            "assistant",
            json!([{ "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "pnpm test" } }]),
            json!({}),
        ),
        l.line(
            "user",
            json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "1 failed", "is_error": true }]),
            json!({}),
        ),
        l.line("user", json!("<command-caveat>"), json!({ "isMeta": true })),
        l.line(
            "assistant",
            json!([{ "type": "text", "text": "from a subagent" }]),
            json!({ "isSidechain": true }),
        ),
        l.line(
            "user",
            json!(format!(
                "<new-message-arrived-while-you-were-working>\n{}\n</new-message-arrived-while-you-were-working>\n\nNote: A new message arrived.",
                prompt_text("also lint")
            )),
            json!({}),
        ),
        l.line(
            "assistant",
            json!([
                { "type": "tool_use", "id": "t2", "name": "Edit", "input": { "file_path": "/w/a.ts" } },
                { "type": "text", "text": "Fixed." },
            ]),
            json!({}),
        ),
        "not json".into(),
        l.line(
            "user",
            json!([{ "type": "text", "text": prompt_text("thanks") }]),
            json!({}),
        ),
        l.line(
            "assistant",
            json!([{ "type": "text", "text": "Any time." }]),
            json!({}),
        ),
    ]
    .join("\n")
}

fn kinds(turn: &TranscriptTurn) -> Vec<&'static str> {
    turn.items
        .iter()
        .map(|item| match item.kind {
            ItemKind::Prompt { .. } => "prompt",
            ItemKind::Thought { .. } => "thought",
            ItemKind::Tool(_) => "tool",
            ItemKind::Message { .. } => "message",
            ItemKind::Status(_) => "status",
            ItemKind::Plan(_) => "plan",
            ItemKind::System(_) => "system",
        })
        .collect()
}

fn tool(item: &Item) -> &ToolItem {
    match &item.kind {
        ItemKind::Tool(tool) => tool,
        other => panic!("not a tool: {other:?}"),
    }
}

fn prompt_of(item: &Item) -> (&str, Option<&str>, bool) {
    match &item.kind {
        ItemKind::Prompt {
            text,
            author,
            steer,
            ..
        } => (text, author.as_deref(), *steer),
        other => panic!("not a prompt: {other:?}"),
    }
}

#[test]
fn groups_a_session_file_into_prompted_turns_with_steering_tools_and_replies() {
    let turns = claude_turns(&file(), &conversation(), false);
    assert_eq!(
        turns.iter().map(kinds).collect::<Vec<_>>(),
        [
            vec!["prompt", "thought", "tool", "prompt", "tool", "message"],
            vec!["prompt", "message"],
        ]
    );
    let (first, second) = (&turns[0], &turns[1]);
    assert_eq!(first.turn_id, "u1");
    assert_eq!(first.channel_id.as_deref(), Some("channel"));
    assert_eq!(first.thread_root, Some(Some(ROOT.into())));
    assert_eq!(first.session_id.as_deref(), Some("session-1"));
    assert_eq!((first.started_at, first.ended_at), (1000, Some(8000)));
    assert!(!first.partial);
    assert_eq!(
        prompt_of(&first.items[0]),
        ("fix the tests", Some("Sam"), false)
    );
    let bash = tool(&first.items[2]);
    assert_eq!(
        (
            bash.title.as_str(),
            bash.kind.as_str(),
            bash.status.as_str(),
            bash.output.as_str(),
            bash.completed_at
        ),
        ("pnpm test", "execute", "failed", "1 failed", Some(4000))
    );
    assert_eq!(prompt_of(&first.items[3]), ("also lint", Some("Sam"), true));
    let edit = tool(&first.items[4]);
    assert_eq!(
        (
            edit.title.as_str(),
            edit.kind.as_str(),
            edit.status.as_str()
        ),
        ("Edit", "edit", "in_progress")
    );
    assert_eq!(edit.paths, ["/w/a.ts"]);
    assert_eq!(second.turn_id, "u9");
    assert_eq!((second.started_at, second.ended_at), (9000, Some(10_000)));
    // A working session's last turn has not ended.
    let working = claude_turns(&file(), &conversation(), true);
    assert_eq!(working.last().unwrap().ended_at, None);
}

#[test]
fn keeps_steps_before_the_first_loaded_prompt_as_a_partial_turn() {
    let line = Lines::new(0).line(
        "assistant",
        json!([{ "type": "text", "text": "resumed" }]),
        json!({}),
    );
    let turns = claude_turns(
        &line,
        &Conversation {
            session_id: "s".into(),
            channel_id: "dm".into(),
            root: None,
        },
        false,
    );
    assert!(turns[0].partial);
    assert_eq!(turns[0].thread_root, Some(None));
}

#[test]
fn names_projects_as_claude_does() {
    assert_eq!(project_name("/Users/me/.buzz"), "-Users-me--buzz");
    assert_eq!(project_name("/Users/me/my_repo"), "-Users-me-my-repo");
}

/// `workspace` prints the resolved folder; `read` serves `files` as
/// base64.
#[derive(Clone, Default)]
struct Host {
    files: Arc<Mutex<HashMap<String, String>>>,
    spawns: Arc<Mutex<Vec<(String, SpawnOptions)>>>,
}

struct Done;

impl ProcessControl for Done {
    fn write(&self, _: String, _: bool) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn kill(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

impl Host {
    fn with(files: &[(&str, String)]) -> Self {
        let host = Self::default();
        host.files.lock().unwrap().extend(
            files
                .iter()
                .map(|(path, content)| (path.to_string(), content.clone())),
        );
        host
    }
    fn spawner(&self) -> Spawner {
        Arc::new(self.clone())
    }
    fn spawned(&self, id: &str) -> Vec<SpawnOptions> {
        self.spawns
            .lock()
            .unwrap()
            .iter()
            .filter(|(spawned, _)| spawned == id)
            .map(|(_, options)| options.clone())
            .collect()
    }
}

impl Spawn for Host {
    fn spawn(
        &self,
        id: &str,
        options: SpawnOptions,
    ) -> BoxFuture<'static, Result<Spawned, String>> {
        self.spawns
            .lock()
            .unwrap()
            .push((id.into(), options.clone()));
        let (output, receiver) = mpsc::unbounded_channel();
        let path = options.args.first().cloned().unwrap_or_default();
        let code = match id {
            "workspace" => {
                let _ = output.send(ProcessOutput::Stdout("/Users/me/.buzz\n".into()));
                0
            }
            "read" => match self.files.lock().unwrap().get(&path) {
                Some(content) => {
                    let text = base64::engine::general_purpose::STANDARD.encode(content);
                    let _ = output.send(ProcessOutput::Stdout(text));
                    0
                }
                None => {
                    let _ = output.send(ProcessOutput::Stderr(format!(
                        "base64: {path}: No such file or directory\n"
                    )));
                    1
                }
            },
            _ => 1,
        };
        let _ = output.send(ProcessOutput::Exit(Some(code)));
        Box::pin(async move {
            Ok(Spawned {
                process: Process(Arc::new(Done)),
                output: receiver,
            })
        })
    }
}

#[tokio::test]
async fn reads_the_session_file_from_the_project_of_the_agents_workspace() {
    let host = Host::with(&[("./-Users-me--buzz/session-1.jsonl", file())]);
    let read = read_transcript(
        &host.spawner(),
        "~/.buzz",
        &conversation(),
        false,
        &Signal::default(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(host.spawned("workspace")[0].cwd.as_deref(), Some("~/.buzz"));
    let reads = host.spawned("read");
    assert_eq!(reads[0].cwd.as_deref(), Some("~/.claude/projects"));
    assert_eq!(reads[0].args, ["./-Users-me--buzz/session-1.jsonl"]);
    assert!(!read.more);
    assert_eq!(read.transcript.unknown_thread, 0);
    assert_eq!(read.transcript.turns.len(), 2);
}

#[tokio::test]
async fn treats_a_missing_session_file_as_no_transcript_not_a_failure() {
    let host = Host::default();
    let read = read_transcript(
        &host.spawner(),
        "~/.buzz",
        &conversation(),
        false,
        &Signal::default(),
    )
    .await;
    assert_eq!(read, Ok(None));
}

/// The path the reader gets is one `base64 -i` reads as a file, not an
/// option, though project names start with `-`.
#[test]
fn passes_base64_a_session_path_it_reads_as_a_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("-Users-me--buzz")).unwrap();
    std::fs::write(dir.path().join("-Users-me--buzz/session-1.jsonl"), "{}\n").unwrap();
    let output = std::process::Command::new("base64")
        .args(["-i", "./-Users-me--buzz/session-1.jsonl"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let text: String = String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .collect();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap(),
        b"{}\n"
    );
}
