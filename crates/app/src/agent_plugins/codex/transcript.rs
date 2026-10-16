//! A conversation's Codex thread, read back through a short-lived
//! `codex app-server` and shown in the activity transcript model
//! (buzz-app's `transcript.ts`). Codex owns the history; nothing here is
//! saved.

use super::rpc::AppServer;
use crate::agent_activity::transcript_model::{
    Item, ItemKind, ToolItem, Transcript, TranscriptTurn, prompt_sections,
};
use crate::agent_plugins::process::Spawner;
use crate::buzz_mcp::app_client::Signal;
use serde_json::{Map, Value, json};

/// The latest turns shown; earlier ones stay in Codex's own history.
pub const TURN_LIMIT: usize = 20;

/// A thread's latest turns, whether it has earlier ones, and whether its
/// latest turn is still running.
#[derive(Clone, Debug, PartialEq)]
pub struct CodexRead {
    pub transcript: Transcript,
    pub more: bool,
    pub working: bool,
}

/// The conversation a thread is for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub channel_id: String,
    pub root: Option<String>,
}

fn str_of(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}

/// A value as text: strings as they are, anything else as JSON.
fn display(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn array(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Codex item statuses as ACP tool statuses.
fn status(value: &str) -> &'static str {
    match value {
        "inProgress" => "in_progress",
        "failed" | "declined" => "failed",
        _ => "completed",
    }
}

/// The owner's request inside a Buzz turn input; other framing stays in
/// sections.
fn request(text: &str) -> String {
    let quoted = text
        .split('\n')
        .filter_map(|line| line.strip_prefix("Request: "))
        .rfind(|quoted| quoted.len() >= 2 && quoted.starts_with('"') && quoted.ends_with('"'));
    quoted
        .and_then(|quoted| serde_json::from_str::<String>(quoted).ok())
        .unwrap_or_else(|| text.to_string())
}

/// A user message's text parts, joined.
fn user_text(value: &Map<String, Value>) -> String {
    array(value.get("content"))
        .iter()
        .map(|part| str_of(part.get("text")))
        .collect::<Vec<_>>()
        .join("\n")
}

fn item(value: &Map<String, Value>, at: u64, steer: bool) -> Option<Item> {
    let id = str_of(value.get("id")).to_string();
    let tool = |patch: ToolItem| ToolItem {
        tool_call_id: id.clone(),
        status: if patch.status.is_empty() {
            status(str_of(value.get("status"))).into()
        } else {
            patch.status
        },
        completed_at: value
            .get("durationMs")
            .and_then(Value::as_f64)
            .map(|duration| at + duration as u64),
        ..patch
    };
    let blank = ToolItem {
        tool_call_id: String::new(),
        title: String::new(),
        kind: "other".into(),
        status: String::new(),
        input: String::new(),
        output: String::new(),
        diffs: Vec::new(),
        paths: Vec::new(),
        completed_at: None,
        truncated: false,
        permission: None,
    };
    let kind = match str_of(value.get("type")) {
        "userMessage" => {
            let raw = user_text(value);
            ItemKind::Prompt {
                text: request(&raw),
                author: None,
                sections: prompt_sections(&raw),
                steer,
            }
        }
        "agentMessage" => ItemKind::Message {
            text: str_of(value.get("text")).into(),
            message_id: None,
        },
        "reasoning" => {
            let text = array(value.get("summary"))
                .iter()
                .map(|part| part.as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n\n");
            if text.is_empty() {
                return None;
            }
            ItemKind::Thought {
                text,
                message_id: None,
            }
        }
        "plan" => ItemKind::Status(str_of(value.get("text")).into()),
        "commandExecution" => ItemKind::Tool(tool(ToolItem {
            title: str_of(value.get("command")).into(),
            kind: "execute".into(),
            output: str_of(value.get("aggregatedOutput")).into(),
            ..blank
        })),
        "fileChange" => {
            let changes = array(value.get("changes"));
            ItemKind::Tool(tool(ToolItem {
                title: "Edit files".into(),
                kind: "edit".into(),
                paths: changes
                    .iter()
                    .map(|change| str_of(change.get("path")).to_string())
                    .collect(),
                output: changes
                    .iter()
                    .map(|change| str_of(change.get("diff")))
                    .collect::<Vec<_>>()
                    .join("\n"),
                ..blank
            }))
        }
        "mcpToolCall" => ItemKind::Tool(tool(ToolItem {
            title: format!(
                "{}.{}",
                str_of(value.get("server")),
                str_of(value.get("tool"))
            ),
            input: display(value.get("arguments")),
            output: display(
                value
                    .get("error")
                    .filter(|error| !error.is_null())
                    .or(value.get("result")),
            ),
            ..blank
        })),
        "dynamicToolCall" => ItemKind::Tool(tool(ToolItem {
            title: [value.get("namespace"), value.get("tool")]
                .into_iter()
                .map(str_of)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("."),
            input: display(value.get("arguments")),
            output: array(value.get("contentItems"))
                .iter()
                .map(|part| str_of(part.get("text")))
                .collect::<Vec<_>>()
                .join("\n"),
            status: if value.get("success") == Some(&Value::Bool(false)) {
                "failed".into()
            } else {
                String::new()
            },
            ..blank
        })),
        "webSearch" => ItemKind::Tool(tool(ToolItem {
            title: "Web search".into(),
            kind: "fetch".into(),
            input: str_of(value.get("query")).into(),
            ..blank
        })),
        _ => return None,
    };
    Some(Item { id, at, kind })
}

/// Codex turns, oldest first, as transcript turns for one conversation.
pub fn codex_turns(turns: &[Value], place: &Place) -> Vec<TranscriptTurn> {
    turns
        .iter()
        .map(|turn| {
            let started_at = turn
                .get("startedAt")
                .and_then(Value::as_f64)
                .map_or(0, |at| (at * 1000.) as u64);
            let mut prompts = 0;
            let items = array(turn.get("items"))
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|value| {
                    let prompt = str_of(value.get("type")) == "userMessage";
                    let steer = prompt && prompts > 0;
                    if prompt {
                        prompts += 1;
                    }
                    item(value, started_at, steer)
                })
                .collect();
            TranscriptTurn {
                turn_id: str_of(turn.get("id")).into(),
                channel_id: Some(place.channel_id.clone()),
                thread_root: Some(place.root.clone()),
                source: None,
                triggering_event_ids: Vec::new(),
                session_id: None,
                new_session: false,
                config: Vec::new(),
                started_at,
                ended_at: turn
                    .get("completedAt")
                    .and_then(Value::as_f64)
                    .filter(|at| *at != 0.)
                    .map(|at| (at * 1000.) as u64),
                error: Some(str_of(turn.pointer("/error/message")).to_string())
                    .filter(|error| !error.is_empty()),
                stop_reason: (str_of(turn.get("status")) == "interrupted")
                    .then(|| "interrupted".to_string()),
                context: None,
                partial: false,
                items,
            }
        })
        .collect()
}

/// A missing rollout (deleted, or never written) is not a read failure.
fn missing(error: &str) -> bool {
    let error = error.to_lowercase();
    ["not loaded", "no rollout", "not found"]
        .iter()
        .any(|text| error.contains(text))
}

/// The thread's latest turns from a short-lived app-server, or `None`
/// when Codex has no history for it on the agent's host. Must run inside
/// tokio.
pub async fn read_transcript(
    spawn: &Spawner,
    thread_id: &str,
    place: &Place,
    signal: &Signal,
) -> Result<Option<CodexRead>, String> {
    let rpc = AppServer::new(None);
    let closer = {
        let (rpc, signal) = (rpc.clone(), signal.clone());
        tokio::spawn(async move {
            signal.wait().await;
            rpc.close().await;
        })
    };
    let result = async {
        signal.check()?;
        rpc.open(spawn, None).await?;
        signal.check()?;
        let page = match rpc
            .request(
                "thread/turns/list",
                json!({
                    "threadId": thread_id,
                    "limit": TURN_LIMIT,
                    "sortDirection": "desc",
                    "itemsView": "full",
                }),
            )
            .await
        {
            Ok(page) => page,
            Err(error) if missing(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        signal.check()?;
        let mut turns = array(page.get("data")).to_vec();
        turns.reverse();
        let working = turns
            .last()
            .is_some_and(|turn| str_of(turn.get("status")) == "inProgress");
        Ok(Some(CodexRead {
            transcript: Transcript {
                turns: codex_turns(&turns, place),
                unknown_thread: 0,
            },
            more: page
                .get("nextCursor")
                .is_some_and(|cursor| !cursor.is_null()),
            working,
        }))
    }
    .await;
    closer.abort();
    rpc.close().await;
    result
}

#[cfg(test)]
mod tests;
