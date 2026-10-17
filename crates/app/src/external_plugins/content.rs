//! What the selected community keeps beside messages, as the user sees
//! it: an agent's memories (buzz-app's `agentMemories`), a channel's
//! Canvas (`canvas`), its details (`channelDetails`), and the user's
//! kit of teams, templates and sidebar groups (`channelKit`). Saving a
//! Canvas needs `send-messages`; saving details or the kit,
//! `manage-channels`.

use super::capabilities::{Failure, channel, community, on_tokio};
use super::protocol::INVALID_PARAMS;
use buzz_relay_client::EventId;
use buzz_relay_client::canvas::{HistoryCursor, Revision};
use buzz_relay_client::kit::KitValue;
use buzz_relay_client::lifecycle::{AttemptStatus, DetailsDraft};
use gpui_kit::*;
use serde_json::{Value, json};

fn invalid(message: impl Into<String>) -> Failure {
    (INVALID_PARAMS, message.into())
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

/// An optional event id at `key`.
fn event_id(params: &Value, key: &str) -> Result<Option<EventId>, Failure> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .and_then(|hex| EventId::from_hex(hex).ok())
            .map(Some)
            .ok_or_else(|| invalid(format!("{key} is not an event id"))),
    }
}

/// `agents/memories`: what one of the user's agents remembers, newest
/// per slug. Only an agent's owner reads them.
pub fn memories(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let agent = text(params, "agent")
        .ok_or_else(|| invalid("agent is required"))?
        .to_ascii_lowercase();
    let owned = crate::agents2::Agents2::global(cx).is_some_and(|agents| {
        agents
            .read(cx)
            .agents()
            .iter()
            .any(|known| known.pubkey == agent)
    });
    if !owned {
        return Err(invalid("agent is not one of your agents"));
    }
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let listing = session
            .agent_memories(&agent)
            .await
            .map_err(|error| (3, error.to_string()))?;
        Ok(json!({
            "entries": listing.entries.iter().map(|entry| json!({
                "slug": entry.slug,
                "body": entry.body,
                "eventId": entry.event_id,
                "createdAt": entry.created_at,
            })).collect::<Vec<_>>(),
            "partial": listing.partial,
        }))
    }))
}

fn revision(revision: &Revision) -> Value {
    json!({
        "id": revision.id.to_hex(),
        "author": revision.author,
        "createdAt": revision.created_at,
        "content": revision.content,
    })
}

/// `canvas/read`: a channel's Canvas now, or `null` without one;
/// `strong` reads past replicas, as before an edit.
pub fn canvas_read(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let strong = params
        .get("strong")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let head = session
            .canvas_read(&channel, strong)
            .await
            .map_err(|error| (3, error.to_string()))?;
        Ok(head.as_ref().map_or(Value::Null, revision))
    }))
}

/// `canvas/history`: a channel's Canvas revisions, newest first, 25 at a
/// time; `cursor` is the `next` of the page before.
pub fn canvas_history(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let cursor = match params.get("cursor").filter(|cursor| !cursor.is_null()) {
        None => None,
        Some(cursor) => Some(HistoryCursor {
            until: cursor
                .get("until")
                .and_then(Value::as_u64)
                .ok_or_else(|| invalid("cursor.until is required"))?,
            before_id: event_id(cursor, "beforeId")?
                .ok_or_else(|| invalid("cursor.beforeId is required"))?,
        }),
    };
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let page = session
            .canvas_history(&channel, cursor)
            .await
            .map_err(|error| (3, error.to_string()))?;
        Ok(json!({
            "revisions": page.revisions.iter().map(revision).collect::<Vec<_>>(),
            "next": page.next.map(|next| json!({
                "until": next.until,
                "beforeId": next.before_id.to_hex(),
            })),
        }))
    }))
}

/// `canvas/save`: replaces a channel's Canvas, if it is still `expected`
/// (the id read last, or `null` for none). A conflict fails with code 4.
pub fn canvas_save(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let content = text(params, "content")
        .ok_or_else(|| invalid("content is required"))?
        .to_owned();
    if buzz_relay_client::canvas::too_large(&content) {
        return Err(invalid("content is too large"));
    }
    let expected = event_id(params, "expected")?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        match session.canvas_save(&channel, &content, expected).await {
            Ok(saved) => Ok(revision(&saved)),
            Err(error) if error.is_conflict() => Err((4, error.to_string())),
            Err(error) => Err((3, error.to_string())),
        }
    }))
}

fn draft(draft: &DetailsDraft) -> Value {
    json!({
        "name": draft.name,
        "description": draft.description,
        "private": draft.private,
        "ttlSeconds": draft.ttl,
        "labels": draft.labels,
    })
}

/// `channels/details`: a channel's name, description, visibility,
/// lifetime and labels, the `version` a save starts from, whether the
/// user may edit them, and a save still in flight or unconfirmed.
pub fn details(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let details = session
            .channel_details(&channel)
            .await
            .map_err(|error| (3, error))?;
        let attempt = session.details_attempt(&channel);
        Ok(json!({
            "channel": details.channel,
            "version": details.version.to_hex(),
            "details": draft(&details.draft),
            "canEdit": details.can_edit,
            "attempt": attempt.map(|attempt| json!({
                "details": draft(&attempt.draft),
                "status": match attempt.status {
                    AttemptStatus::Saving => "saving",
                    AttemptStatus::Unconfirmed => "unconfirmed",
                },
            })),
        }))
    }))
}

/// `channels/saveDetails`: saves `details` over `version`; with
/// `check: true`, checks an unconfirmed save instead.
pub fn save_details(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let session = community.read(cx).session.clone();
    if params.get("check").and_then(Value::as_bool) == Some(true) {
        return Ok(on_tokio(cx, async move {
            session
                .check_channel_details(&channel)
                .await
                .map(|()| Value::Null)
                .map_err(|error| (3, error))
        }));
    }
    let version = event_id(params, "version")?.ok_or_else(|| invalid("version is required"))?;
    let given = params
        .get("details")
        .ok_or_else(|| invalid("details is required"))?;
    let name = text(given, "name")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("details.name is required"))?;
    let labels = match given.get("labels") {
        None | Some(Value::Null) => None,
        Some(labels) => Some(
            labels
                .as_array()
                .ok_or_else(|| invalid("details.labels must be a list"))?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
    };
    let draft = DetailsDraft {
        name: name.to_owned(),
        description: text(given, "description").unwrap_or_default().to_owned(),
        private: given
            .get("private")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ttl: given.get("ttlSeconds").and_then(Value::as_u64),
        labels,
    };
    Ok(on_tokio(cx, async move {
        session
            .save_channel_details(&channel, version, draft)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `kit/get`: the user's kit in the selected community (teams, channel
/// templates and sidebar groups), read once if it never was.
pub fn kit(cx: &App) -> Result<Value, Failure> {
    let session = community(cx)?.read(cx).session.clone();
    session.kit_ensure();
    let snapshot = session.kit().borrow().clone();
    Ok(json!({
        "status": format!("{:?}", snapshot.status).to_ascii_lowercase(),
        "entries": snapshot.entries.iter().map(|entry| json!({
            "eventId": entry.event_id.to_hex(),
            "createdAt": entry.created_at,
            "version": entry.record.version,
            "deleted": entry.record.deleted,
            "value": serde_json::to_value(&entry.record.value).unwrap_or(Value::Null),
        })).collect::<Vec<_>>(),
        "error": snapshot.error,
    }))
}

/// `kit/save`: saves one kit `value` (a `team`, `template` or `groups`,
/// tagged by `type`) over `expected`, the event id it was read as, or
/// deletes it with `deleted: true`. Answers with the new event id.
pub fn save_kit(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let value: KitValue = params
        .get("value")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or_else(|| invalid("value is not a kit value"))?;
    let expected = event_id(params, "expected")?;
    let deleted = params
        .get("deleted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .kit_save(value, expected, deleted)
            .await
            .map(|id| json!({ "eventId": id.to_hex() }))
            .map_err(|error| (3, error))
    }))
}
