//! What the relay's activity feed shows of agents anywhere (buzz-app's
//! `agentActivity`): their turns, typing and decrypted transcript records,
//! live and from the archive. Only an agent's owner can read its records.

use super::capabilities::{Failure, Watch, community};
use crate::agent_activity::observer;
use crate::agent_activity::turns::TurnState;
use crate::agent_activity::{FeedStatus, HistoryStatus, Snapshot};
use gpui_kit::*;
use serde_json::{Value, json};

/// The most records one answer carries, newest kept.
const MAX_RECORDS: usize = 200;

fn state(snapshot: &Snapshot, agent: Option<&str>) -> Value {
    let mine = |who: &str| agent.is_none_or(|agent| agent == who);
    let records: Vec<_> = snapshot
        .records
        .iter()
        .filter(|record| mine(&record.agent))
        .collect();
    let start = records.len().saturating_sub(MAX_RECORDS);
    json!({
        "status": snapshot.status.label(),
        "turns": snapshot.turns.iter().filter(|turn| mine(&turn.agent)).map(|turn| json!({
            "agent": turn.agent,
            "turn": turn.turn_id,
            "channel": turn.channel_id,
            "at": turn.timestamp,
            "state": match turn.state {
                TurnState::Working => "working",
                TurnState::Unknown => "unknown",
                TurnState::Ended => "ended",
            },
            "requests": turn.requests.as_ref().map(|requests| requests.iter().map(|request| json!({
                "message": request.message_id,
                "root": request.thread_root_id,
            })).collect::<Vec<_>>()),
        })).collect::<Vec<_>>(),
        "typing": snapshot.typing.iter().filter(|typing| mine(&typing.agent)).map(|typing| json!({
            "agent": typing.agent,
            "channel": typing.channel_id,
            "root": typing.thread_root,
            "since": typing.started_at,
        })).collect::<Vec<_>>(),
        "records": records[start..].iter().map(|record| json!({
            "id": record.id,
            "agent": record.agent,
            "kind": record.kind,
            "createdAt": record.created_at,
            "receivedAt": record.received_at,
            "historical": record.historical,
            "channels": record.channel_ids,
            "frame": serde_json::from_str::<Value>(&record.plaintext)
                .unwrap_or_else(|_| Value::String(record.plaintext.to_string())),
        })).collect::<Vec<_>>(),
        "history": match snapshot.history {
            HistoryStatus::Unavailable => "unavailable",
            HistoryStatus::Loading => "loading",
            HistoryStatus::Ready => "ready",
            HistoryStatus::Error => "error",
        },
        "hasOlder": snapshot.has_older,
        "workingChannels": snapshot.working_channels,
    })
}

fn agent(params: &Value) -> Option<&str> {
    params.get("agent").and_then(Value::as_str)
}

/// `activity/observed`: what the feed shows now, of one `agent` or all.
pub fn get(params: &Value, cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let snapshot = observer::snapshot(&community, cx);
    if snapshot.status == FeedStatus::Unavailable {
        return Err((3, "This device cannot read agent activity.".to_owned()));
    }
    Ok(state(&snapshot, agent(params)))
}

/// `activity/history`: reads an agent's saved records from the archive
/// (`selectHistory`), older ones with `older: true` (`loadOlder`), or the
/// newest of everyone with neither. Answers at once; `activity/watch`
/// shows them as they arrive.
pub fn history(params: &Value, cx: &mut App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let activity = observer::get(&community, cx)
        .ok_or_else(|| (3, "Agent activity is not available.".to_owned()))?;
    let older = params.get("older").and_then(Value::as_bool) == Some(true);
    let agent = agent(params).map(str::to_owned);
    activity.update(cx, |activity, cx| match (older, agent) {
        (true, _) => activity.load_older(cx),
        (false, Some(agent)) => activity.select_history(&agent, cx),
        (false, None) => activity.latest_history(cx),
    });
    Ok(Value::Null)
}

/// `activity/watch`: sends `activity/changed` with what
/// `activity/observed` gives, now and whenever it changes.
pub fn watch(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let community = community(cx)?;
    let activity = observer::get(&community, cx)
        .ok_or_else(|| (3, "Agent activity is not available.".to_owned()))?;
    let agent = agent(params).map(str::to_owned);
    let first = state(&activity.read(cx).snapshot, agent.as_deref());
    let last = std::cell::RefCell::new(first.clone());
    let notify = std::rc::Rc::new(notify);
    let send = notify.clone();
    cx.defer(move |cx| send(first, cx));
    let subscription = cx.observe(&activity, move |activity, cx| {
        let now = state(&activity.read(cx).snapshot, agent.as_deref());
        if now != *last.borrow() {
            *last.borrow_mut() = now.clone();
            notify(now, cx);
        }
    });
    Ok((Watch::new(subscription, Task::ready(())), json!({})))
}
