//! A conversation's Claude session, read from the transcript Claude keeps
//! on the agent's host and shown in the activity transcript model
//! (buzz-app's `transcript.ts`). Claude owns the file; nothing here writes
//! or keeps it.

use crate::agent_activity::transcript_model::{
    Item, ItemKind, ToolItem, Transcript, TranscriptTurn, prompt_message, prompt_sections,
};
use crate::agent_activity::turns::parse_time;
use crate::agent_plugins::process::{SpawnOptions, Spawner, collect};
use crate::buzz_mcp::app_client::{Signal, read_file};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// The latest turns shown; the file keeps the rest.
pub const TURN_LIMIT: usize = 50;
const STEER: &str = "new-message-arrived-while-you-were-working";
/// Longer project names get a hash suffix this does not reproduce.
const PROJECT_NAME_LIMIT: usize = 200;

/// One session file's conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub session_id: String,
    pub channel_id: String,
    pub root: Option<String>,
}

/// The latest turns of a read, and whether the file has earlier ones.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    pub transcript: Transcript,
    pub more: bool,
}

fn str_of(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}

/// JavaScript's truthiness, for flags Claude writes.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn blocks(value: Option<&Value>) -> Vec<&Map<String, Value>> {
    value
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default()
}

/// Message content as text: a string, or its text blocks joined.
fn text(value: Option<&Value>) -> String {
    if let Some(text) = value.and_then(Value::as_str) {
        return text.to_string();
    }
    blocks(value)
        .iter()
        .map(|block| str_of(block.get("text")))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Claude's tools by ACP tool kind.
fn kind(name: &str) -> &'static str {
    match name {
        "Bash" => "execute",
        "Read" => "read",
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "edit",
        "Grep" | "Glob" => "search",
        "WebFetch" | "WebSearch" => "fetch",
        _ => "other",
    }
}

/// A Buzz prompt, or a steer wrapping one, as the turn's opening item.
fn prompt(id: &str, at: u64, raw: &str) -> Item {
    let mut sections = prompt_sections(raw);
    let steer = sections
        .iter()
        .find(|section| section.tag == STEER)
        .map(|section| section.body.clone());
    if let Some(body) = &steer {
        sections = prompt_sections(body);
    }
    let (text, author) = prompt_message(&sections);
    Item {
        id: id.into(),
        at,
        kind: ItemKind::Prompt {
            text: if text.is_empty() { raw.into() } else { text },
            author,
            sections,
            steer: steer.is_some(),
        },
    }
}

/// Transcript lines as turns, each opened by a prompt. Claude's own
/// entries (metadata, subagents, attachments) are left out.
pub fn claude_turns(file: &str, conversation: &Conversation, working: bool) -> Vec<TranscriptTurn> {
    let mut turns: Vec<TranscriptTurn> = Vec::new();
    // Tool calls by id: their turn and item.
    let mut tools: HashMap<String, (usize, usize)> = HashMap::new();
    let mut last = 0;
    let start = |turns: &mut Vec<TranscriptTurn>, last: u64, id: &str, at: u64, partial: bool| {
        if let Some(current) = turns.last_mut() {
            current.ended_at = Some(last.max(current.started_at));
        }
        turns.push(TranscriptTurn {
            turn_id: id.into(),
            channel_id: Some(conversation.channel_id.clone()),
            thread_root: Some(conversation.root.clone()),
            source: None,
            triggering_event_ids: Vec::new(),
            session_id: Some(conversation.session_id.clone()),
            new_session: false,
            config: Vec::new(),
            started_at: at,
            ended_at: None,
            error: None,
            stop_reason: None,
            context: None,
            partial,
            items: Vec::new(),
        });
    };
    for line in file.split('\n') {
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if truthy(entry.get("isSidechain")) || truthy(entry.get("isMeta")) {
            continue;
        }
        let message = entry.get("message").and_then(Value::as_object);
        let id = str_of(entry.get("uuid"));
        let at = parse_time(str_of(entry.get("timestamp")))
            .filter(|at| *at != 0)
            .unwrap_or(last);
        let kind_of = str_of(entry.get("type"));
        let Some(message) = message else { continue };
        if id.is_empty() || (kind_of != "user" && kind_of != "assistant") {
            continue;
        }
        let content = message.get("content");
        let results: Vec<_> = blocks(content)
            .into_iter()
            .filter(|block| str_of(block.get("type")) == "tool_result")
            .collect();
        if kind_of == "user" && results.is_empty() {
            let item = prompt(id, at, &text(content));
            let steer = matches!(item.kind, ItemKind::Prompt { steer: true, .. });
            if !steer || turns.is_empty() {
                start(&mut turns, last, id, at, false);
            }
            if let Some(current) = turns.last_mut() {
                current.items.push(item);
            }
            last = at;
            continue;
        }
        if turns.is_empty() {
            start(&mut turns, last, id, at, true);
        }
        last = at;
        for result in results {
            let Some(&(turn, index)) = tools.get(str_of(result.get("tool_use_id"))) else {
                continue;
            };
            if let ItemKind::Tool(tool) = &mut turns[turn].items[index].kind {
                tool.output = text(result.get("content"));
                tool.status = if truthy(result.get("is_error")) {
                    "failed"
                } else {
                    "completed"
                }
                .into();
                tool.completed_at = Some(at);
            }
        }
        let turn = turns.len() - 1;
        for (index, block) in blocks(content).into_iter().enumerate() {
            let key = format!("{id}:{index}");
            let item = match str_of(block.get("type")) {
                "text" if !str_of(block.get("text")).trim().is_empty() => ItemKind::Message {
                    text: str_of(block.get("text")).into(),
                    message_id: None,
                },
                "thinking" if !str_of(block.get("thinking")).trim().is_empty() => {
                    ItemKind::Thought {
                        text: str_of(block.get("thinking")).into(),
                        message_id: None,
                    }
                }
                "tool_use" => {
                    let name = str_of(block.get("name"));
                    let input = block
                        .get("input")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let path = Some(str_of(input.get("file_path")))
                        .filter(|path| !path.is_empty())
                        .unwrap_or(str_of(input.get("path")));
                    let command = str_of(input.get("command"));
                    let call = str_of(block.get("id")).to_string();
                    tools.insert(call.clone(), (turn, turns[turn].items.len()));
                    ItemKind::Tool(ToolItem {
                        tool_call_id: call,
                        title: if command.is_empty() { name } else { command }.into(),
                        kind: kind(name).into(),
                        status: "in_progress".into(),
                        input: serde_json::to_string_pretty(&Value::Object(input.clone()))
                            .unwrap_or_default(),
                        output: String::new(),
                        diffs: Vec::new(),
                        paths: if path.is_empty() {
                            Vec::new()
                        } else {
                            vec![path.into()]
                        },
                        completed_at: None,
                        truncated: false,
                        permission: None,
                    })
                }
                _ => continue,
            };
            turns[turn].items.push(Item {
                id: key,
                at,
                kind: item,
            });
        }
    }
    if let Some(current) = turns.last_mut()
        && !working
    {
        current.ended_at = Some(last.max(current.started_at));
    }
    turns
}

/// Claude's project folder name for a working directory.
pub fn project_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The session's transcript from the folder Claude keeps for the agent's
/// workspace, or `None` when the host has no file for it: deleted, or
/// started in a workspace the agent no longer uses.
pub async fn read_transcript(
    spawn: &Spawner,
    workspace: &str,
    conversation: &Conversation,
    working: bool,
    signal: &Signal,
) -> Result<Option<Read>, String> {
    let mut spawned = spawn
        .spawn(
            "workspace",
            SpawnOptions {
                cwd: Some(workspace.into()),
                ..Default::default()
            },
        )
        .await?;
    let (code, directory, _) = collect(&mut spawned.output).await;
    if code != Some(0) {
        return Err("Could not find the agent's workspace".into());
    }
    signal.check()?;
    let project = project_name(directory.trim());
    if project.len() > PROJECT_NAME_LIMIT {
        return Ok(None);
    }
    // Project names start with `-`; base64 would read one as options.
    let path = format!("./{project}/{}.jsonl", conversation.session_id);
    let bytes = match read_file(spawn, "~/.claude/projects", &path, Some(signal)).await {
        Ok(bytes) => bytes,
        Err(error) => {
            signal.check()?;
            if error.to_lowercase().contains("no such file") {
                return Ok(None);
            }
            return Err(error);
        }
    };
    let mut turns = claude_turns(&String::from_utf8_lossy(&bytes), conversation, working);
    let more = turns.len() > TURN_LIMIT;
    turns.drain(..turns.len().saturating_sub(TURN_LIMIT));
    Ok(Some(Read {
        transcript: Transcript {
            turns,
            unknown_thread: 0,
        },
        more,
    }))
}

#[cfg(test)]
mod tests;
