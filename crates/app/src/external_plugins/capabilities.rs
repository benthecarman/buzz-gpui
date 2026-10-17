//! What a plugin can ask the app for: reading the selected community's
//! conversations and people, following a channel, sending as the user and
//! opening a conversation (`docs/plugins.md`).
//!
//! Reads open a channel or thread view on the session, wait for its first
//! answer and close it again; they never select the channel, so nothing is
//! marked read. Sending needs the `send-messages` permission.

use super::protocol::INVALID_PARAMS;
use crate::community::{AppState, Community};
use crate::runtime::Runtime;
use crate::ui::navigation::{self, Place, Request, Target};
use buzz_relay_client::fold::Delivery;
use buzz_relay_client::{
    ChannelView, EventId, Mentions, MessageRow, Profile, Profiles, Session, ThreadStatus,
    TimelineSnapshot, TimelineStatus,
};
use gpui_kit::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// How long a read waits for the relay.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Most messages one answer carries.
pub const MAX_MESSAGES: usize = 200;
const DEFAULT_MESSAGES: usize = 50;
const MAX_PROFILES: usize = 500;

pub type Failure = (i64, String);

fn invalid(message: impl Into<String>) -> Failure {
    (INVALID_PARAMS, message.into())
}

/// The selected community, or an error a plugin can show.
pub fn community(cx: &App) -> Result<Entity<Community>, Failure> {
    AppState::current(cx).ok_or_else(|| (1, "No community is selected.".to_owned()))
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn event_id(params: &Value, key: &str) -> Result<Option<EventId>, Failure> {
    text(params, key)
        .map(|hex| EventId::from_hex(hex).map_err(|_| invalid(format!("{key} is not an event id"))))
        .transpose()
}

/// The `channel` param, checked against the community's channels.
pub fn channel(params: &Value, community: &Community) -> Result<String, Failure> {
    let id = text(params, "channel").ok_or_else(|| invalid("channel is required"))?;
    community
        .snapshot
        .channels
        .iter()
        .any(|channel| channel.id == id)
        .then(|| id.to_owned())
        .ok_or_else(|| {
            (
                1,
                format!("This community has no channel {id} you can read."),
            )
        })
}

pub fn limit(params: &Value) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MESSAGES, |limit| {
            (limit as usize).clamp(1, MAX_MESSAGES)
        })
}

/// A message as plugins see it.
pub fn message(row: &MessageRow, profiles: &Profiles) -> Value {
    let author = row.author.to_hex();
    let name = profiles.get(&author).map(|profile| profile.name.clone());
    json!({
        "id": row.id.to_hex(),
        "channel": &*row.channel,
        "author": author,
        "authorName": name,
        "content": row.content,
        "createdAt": row.created_at,
        "createdAtMs": row.created_at_ms,
        "edited": row.edited,
        "threadRoot": row.thread_root_id.map(|id| id.to_hex()),
        "replyParent": row.reply_parent_id.map(|id| id.to_hex()),
        "replyCount": row.reply_count,
        "mentions": row.mentions.iter().map(|key| key.to_hex()).collect::<Vec<_>>(),
        "delivery": row.delivery.as_ref().map(|delivery| match delivery.status {
            Delivery::Sending => "sending",
            Delivery::Accepted => "accepted",
            Delivery::Unknown => "unknown",
            Delivery::Failed => "failed",
            Delivery::Seen => "seen",
        }),
    })
}

/// The newest `limit` messages of a timeline, oldest first. Relay
/// activity rows, such as joins, are left out.
pub fn timeline(snapshot: &TimelineSnapshot, limit: usize, profiles: &Profiles) -> Value {
    let rows: Vec<&Arc<MessageRow>> = snapshot
        .rows
        .iter()
        .filter(|row| row.membership.is_none())
        .collect();
    let start = rows.len().saturating_sub(limit);
    json!({
        "channel": snapshot.channel,
        "messages": rows[start..].iter().map(|row| message(row, profiles)).collect::<Vec<_>>(),
        "hasMore": snapshot.has_more || start > 0,
        "error": match &snapshot.status {
            TimelineStatus::Error(error) => Some(error.clone()),
            _ => None,
        },
    })
}

fn profile(key: &str, profile: Option<&Arc<Profile>>) -> Value {
    match profile {
        Some(profile) => json!({
            "pubkey": key,
            "name": profile.name,
            "picture": profile.picture,
            "about": profile.about,
            "nip05": profile.nip05,
            "isAgent": profile.is_agent,
            "loaded": !profile.name_is_fallback,
        }),
        None => json!({
            "pubkey": key,
            "name": Profile::fallback(key).name,
            "picture": null,
            "about": null,
            "nip05": null,
            "isAgent": false,
            "loaded": false,
        }),
    }
}

/// `profiles/get`: known profiles now; unknown ones are asked for, and
/// come back with `loaded: true` on a later call.
pub fn profiles(params: &Value, cx: &App) -> Result<Value, Failure> {
    let keys: Vec<String> = params
        .get("pubkeys")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("pubkeys must be a list"))?
        .iter()
        .filter_map(Value::as_str)
        .filter(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .take(MAX_PROFILES)
        .collect();
    let community = community(cx)?;
    let community = community.read(cx);
    let known = &community.snapshot.profiles;
    let missing: Vec<String> = keys
        .iter()
        .filter(|key| !known.contains_key(*key))
        .cloned()
        .collect();
    if !missing.is_empty() {
        community.session.want_profiles(missing);
    }
    Ok(Value::Array(
        keys.iter()
            .map(|key| profile(key, known.get(key)))
            .collect(),
    ))
}

/// Waits on the tokio runtime for `wait`, at most [`READ_TIMEOUT`].
fn on_runtime<T: Send + 'static>(
    cx: &App,
    wait: impl std::future::Future<Output = T> + Send + 'static,
) -> tokio::task::JoinHandle<Option<T>> {
    Runtime::handle(cx).spawn(async move { tokio::time::timeout(READ_TIMEOUT, wait).await.ok() })
}

/// `channel/messages`: the channel's newest messages.
pub fn channel_messages(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let limit = limit(params);
    let session = community.read(cx).session.clone();
    let wait = on_runtime(cx, async move {
        let view = session.open_channel(&channel);
        let mut snapshot = view.snapshot.clone();
        let ready = snapshot
            .wait_for(|timeline| timeline.status != TimelineStatus::Loading)
            .await
            .map(|timeline| timeline.clone());
        drop(view);
        ready.ok()
    });
    Ok(cx.spawn(async move |cx| {
        let snapshot = wait
            .await
            .ok()
            .flatten()
            .flatten()
            .ok_or((2, "The relay did not answer in time.".to_owned()))?;
        Ok(cx.update(|cx| {
            let profiles = community.read(cx).snapshot.profiles.clone();
            timeline(&snapshot, limit, &profiles)
        }))
    }))
}

/// `thread/messages`: a thread's root and replies.
pub fn thread_messages(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let root = event_id(params, "root")?.ok_or_else(|| invalid("root is required"))?;
    let session = community.read(cx).session.clone();
    let wait = on_runtime(cx, async move {
        let view = session.open_thread(&channel, root, false);
        let mut snapshot = view.snapshot.clone();
        let ready = snapshot
            .wait_for(|thread| matches!(thread.status, ThreadStatus::Ready | ThreadStatus::Error))
            .await
            .map(|thread| thread.clone());
        drop(view);
        ready.ok()
    });
    Ok(cx.spawn(async move |cx| {
        let thread = wait
            .await
            .ok()
            .flatten()
            .flatten()
            .ok_or((2, "The relay did not answer in time.".to_owned()))?;
        if thread.status == ThreadStatus::Error {
            return Err((
                2,
                thread
                    .error
                    .clone()
                    .unwrap_or_else(|| "The thread could not be read.".to_owned()),
            ));
        }
        Ok(cx.update(|cx| {
            let profiles = community.read(cx).snapshot.profiles.clone();
            json!({
                "root": thread.root.as_ref().map(|row| message(row, &profiles)),
                "replies": thread
                    .replies
                    .iter()
                    .map(|row| message(row, &profiles))
                    .collect::<Vec<_>>(),
                "hasMore": thread.can_load_more,
            })
        }))
    }))
}

/// `messages/send`. It answers once the message is safely queued; the
/// relay may still refuse it, which `channel/watch` shows as a delivery
/// of `failed`.
pub fn send(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let content = text(params, "content")
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| invalid("content is required"))?
        .to_owned();
    let thread = match params.get("thread") {
        None | Some(Value::Null) => None,
        Some(thread) => {
            let root =
                event_id(thread, "root")?.ok_or_else(|| invalid("thread.root is required"))?;
            Some((root, event_id(thread, "parent")?.unwrap_or(root)))
        }
    };
    let recipients: Vec<String> = params
        .get("mentions")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let mentions = Mentions {
        recipients,
        ..Mentions::default()
    };
    let session: Arc<Session> = community.read(cx).session.clone();
    let sent = session.send(&channel, &content, mentions, Vec::new(), thread);
    Ok(cx.spawn(async move |_| sent.await.map(|()| Value::Null).map_err(|error| (3, error))))
}

/// `ui/openConversation`: shows a channel, a message or a thread.
pub fn open_conversation(params: &Value, cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let community = community.read(cx);
    let channel = channel(params, community)?;
    let message = event_id(params, "message")?;
    let thread = event_id(params, "thread")?;
    let target = Target {
        community: Some(community.origin.to_string()),
        place: Place::Conversation {
            channel,
            // A thread alone opens it.
            message: message.or(thread),
            thread,
            members: false,
        },
    };
    let sender = navigation::sender(cx).ok_or((1, "The window is not open.".to_owned()))?;
    let _ = sender.unbounded_send(Request::Open(target).into());
    Ok(Value::Null)
}

/// A channel a plugin follows: the view keeps the timeline live.
pub struct Watch {
    pub channel: String,
    _view: ChannelView,
    _forward: Task<()>,
}

/// `channel/watch`: sends `channel/changed` with the newest messages now
/// and whenever the channel changes, until `channel/unwatch` or the
/// plugin stops.
pub fn watch(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<Watch, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let limit = limit(params);
    let view = community.read(cx).session.open_channel(&channel);
    let mut receiver = view.snapshot.clone();
    let (sender, mut values) = futures::channel::mpsc::unbounded::<Arc<TimelineSnapshot>>();
    let _ = sender.unbounded_send(receiver.borrow_and_update().clone());
    Runtime::handle(cx).spawn(async move {
        while receiver.changed().await.is_ok() {
            let value = receiver.borrow_and_update().clone();
            if sender.unbounded_send(value).is_err() {
                return;
            }
        }
    });
    let forward = cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some(mut snapshot) = values.next().await {
            // Only the newest of a burst matters.
            while let Ok(newer) = values.try_recv() {
                snapshot = newer;
            }
            if snapshot.status == TimelineStatus::Loading {
                continue;
            }
            cx.update(|cx| {
                let profiles = community.read(cx).snapshot.profiles.clone();
                notify(timeline(&snapshot, limit, &profiles), cx);
            });
        }
    });
    Ok(Watch {
        channel,
        _view: view,
        _forward: forward,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn limits_are_bounded() {
        assert_eq!(limit(&json!({})), DEFAULT_MESSAGES);
        assert_eq!(limit(&json!({ "limit": 0 })), 1);
        assert_eq!(limit(&json!({ "limit": 10_000 })), MAX_MESSAGES);
    }

    #[test]
    fn unknown_profiles_are_fallbacks() {
        let key = "a".repeat(64);
        let value = profile(&key, None);
        assert_eq!(value["loaded"], false);
        assert_eq!(value["pubkey"], key);
        assert!(value["name"].as_str().is_some_and(|name| !name.is_empty()));
    }

    #[test]
    fn event_ids_are_checked() {
        assert!(event_id(&json!({ "root": "nope" }), "root").is_err());
        assert_eq!(event_id(&json!({}), "root"), Ok(None));
        let hex = "b".repeat(64);
        assert_eq!(
            event_id(&json!({ "root": hex }), "root")
                .unwrap()
                .unwrap()
                .to_hex(),
            hex
        );
    }
}
