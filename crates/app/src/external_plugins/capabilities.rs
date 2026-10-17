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
    EventId, Mentions, MessageRow, Profile, Profiles, Session, ThreadStatus, TimelineSnapshot,
    TimelineStatus,
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
        "kind": row.kind,
        "audience": row.audience.map(|_| "agents"),
        "signer": row.signer.map(|key| key.to_hex()),
        "workflowOwner": row.workflow_owner.map(|key| key.to_hex()),
        "participants": row.participants.iter().map(|key| key.to_hex()).collect::<Vec<_>>(),
        "reactions": row.reactions.iter().map(|group| json!({
            "content": group.content,
            "emojiUrl": group.emoji.as_ref().map(|emoji| emoji.url.clone()),
            "count": group.events.len(),
            "authors": group.events.iter().map(|reaction| reaction.author.to_hex()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "attachments": row.attachments.iter().map(|attachment| json!({
            "url": attachment.url,
            "mime": attachment.mime,
            "size": attachment.size,
            "name": attachment.name,
        })).collect::<Vec<_>>(),
        "emoji": row.emoji.iter().map(|emoji| json!({
            "shortcode": emoji.shortcode,
            "url": emoji.url,
        })).collect::<Vec<_>>(),
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

/// Runs `work` on the tokio runtime, where the session's network calls
/// need to be, and answers with what it returns.
pub(super) fn on_tokio<T: Send + 'static>(
    cx: &App,
    work: impl std::future::Future<Output = Result<T, Failure>> + Send + 'static,
) -> Task<Result<T, Failure>> {
    let handle = Runtime::handle(cx).spawn(work);
    cx.background_spawn(async move {
        handle
            .await
            .unwrap_or_else(|_| Err((2, "The request stopped.".to_owned())))
    })
}

/// The `channel` and `message` params of a write, checked.
fn message_target(
    params: &Value,
    cx: &App,
) -> Result<(Entity<Community>, String, EventId), Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let message = event_id(params, "message")?.ok_or_else(|| invalid("message is required"))?;
    Ok((community, channel, message))
}

/// `messages/edit`: replaces the text of one of the user's messages.
pub fn edit(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let (community, channel, message) = message_target(params, cx)?;
    let content = text(params, "content")
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| invalid("content is required"))?
        .to_owned();
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .edit(&channel, message, &content)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `messages/react`: reacts to a message with an emoji.
pub fn react(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let (community, channel, message) = message_target(params, cx)?;
    let emoji = text(params, "emoji")
        .map(str::trim)
        .filter(|emoji| !emoji.is_empty() && emoji.chars().count() <= 64)
        .ok_or_else(|| invalid("emoji is required"))?
        .to_owned();
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .react(&channel, message, &emoji)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `messages/delete`: deletes one of the user's messages or reactions, or
/// a message of an agent the user owns.
pub fn delete(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let (community, channel, message) = message_target(params, cx)?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .remove(&channel, message)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `messages/report`: reports a message to the community's moderators
/// (NIP-56), with a `reason` and a `note`.
pub fn report(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    use buzz_relay_client::messages::ReportType;
    let (community, channel, message) = message_target(params, cx)?;
    let kind = match text(params, "reason") {
        Some("spam") => ReportType::Spam,
        Some("profanity") => ReportType::Profanity,
        Some("nudity") => ReportType::Nudity,
        Some("impersonation") => ReportType::Impersonation,
        Some("malware") => ReportType::Malware,
        Some("illegal") => ReportType::Illegal,
        Some("other") | None => ReportType::Other,
        Some(_) => return Err(invalid("reason is not a report reason")),
    };
    let note = text(params, "note")
        .unwrap_or_default()
        .chars()
        .take(2000)
        .collect::<String>();
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .report(&channel, message, kind, &note)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `messages/retry` and `messages/dismiss`: sends one of your failed
/// messages or changes again, or drops it.
pub fn retry(
    dismiss: bool,
    params: &Value,
    cx: &App,
) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let message = event_id(params, "message")?.ok_or_else(|| invalid("message is required"))?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let done = if dismiss {
            session.dismiss(message).await
        } else {
            session.retry_send(message).await
        };
        done.map(|()| Value::Null).map_err(|error| (3, error))
    }))
}

/// The largest file `files/upload` takes, before base64.
const MAX_UPLOAD: usize = 3 * 1024 * 1024;

/// `files/upload`: uploads a file to the community, for `messages/send`'s
/// `attachments`.
pub fn upload(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    use base64::Engine;
    let community = community(cx)?;
    let data = text(params, "data").ok_or_else(|| invalid("data (base64) is required"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| invalid("data is not base64"))?;
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD {
        return Err(invalid("A file must be 1 byte to 3 MiB."));
    }
    let name = text(params, "name")
        .map(|name| name.chars().take(200).collect::<String>())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "file".to_owned());
    let mime = text(params, "mime")
        .map(str::to_owned)
        .unwrap_or_else(|| buzz_relay_client::upload::mime_type(&name, &bytes));
    let media = community.read(cx).session.media().clone();
    Ok(on_tokio(cx, async move {
        let file = buzz_relay_client::upload::UploadFile { name, mime, bytes };
        let attachment = buzz_relay_client::upload::upload(&media, file)
            .await
            .map_err(|error| (3, error.to_string()))?;
        Ok(json!({
            "name": attachment.name,
            "url": attachment.url,
            "mime": attachment.mime,
            "size": attachment.size,
            "sha256": attachment.sha256,
        }))
    }))
}

/// `messages/send`'s `attachments`, as `files/upload` answered them.
fn attachments(params: &Value) -> Result<Vec<buzz_relay_client::Attachment>, Failure> {
    let Some(list) = params.get("attachments").filter(|list| !list.is_null()) else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .filter(|list| list.len() <= 16)
        .ok_or_else(|| invalid("attachments must be a list of at most 16"))?;
    list.iter()
        .map(|item| {
            let field = |key: &str| {
                item.get(key)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| invalid(format!("an attachment needs {key}")))
            };
            let url = field("url")?;
            if !url.starts_with("https://") && !url.starts_with("http://") {
                return Err(invalid("an attachment's url must be http(s)"));
            }
            Ok(buzz_relay_client::Attachment {
                name: field("name")?,
                url,
                mime: field("mime")?,
                size: item.get("size").and_then(Value::as_u64).unwrap_or(0),
                sha256: field("sha256")?,
                voice_duration: None,
            })
        })
        .collect()
}

/// `messages/send`. It answers once the message is safely queued; the
/// relay may still refuse it, which `channel/watch` shows as a delivery
/// of `failed`.
pub fn send(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let content = text(params, "content")
        .filter(|content| !content.trim().is_empty() || params.get("attachments").is_some())
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
    let attachments = attachments(params)?;
    let session: Arc<Session> = community.read(cx).session.clone();
    let sent = session.send(&channel, &content, mentions, attachments, thread);
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
            // `panel: "members"`: the members dialog over it.
            members: params.get("members").and_then(Value::as_bool) == Some(true),
        },
    };
    let sender = navigation::sender(cx).ok_or((1, "The window is not open.".to_owned()))?;
    let _ = sender.unbounded_send(Request::Open(target).into());
    Ok(Value::Null)
}

/// Up to `max` lowercase hex keys from the `pubkeys` param.
fn pubkeys(params: &Value, max: usize) -> Result<Vec<String>, Failure> {
    Ok(params
        .get("pubkeys")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("pubkeys must be a list"))?
        .iter()
        .filter_map(Value::as_str)
        .filter(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .take(max)
        .collect())
}

/// `community/channels`: the selected community's channels.
pub fn channels(cx: &App) -> Value {
    use buzz_relay_client::ChannelType;
    let Some(community) = AppState::current(cx) else {
        return json!([]);
    };
    Value::Array(
        community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .map(|channel| {
                json!({
                    "id": channel.id,
                    "name": channel.name,
                    "description": channel.description,
                    "private": channel.private,
                    "archived": channel.archived,
                    "kind": match channel.channel_type {
                        Some(ChannelType::Dm) => "dm",
                        Some(ChannelType::Forum) => "forum",
                        Some(ChannelType::Session) => "session",
                        Some(ChannelType::Stream) | None => "stream",
                    },
                    "members": channel.members,
                    "labels": channel.labels,
                    "lastActivityAt": channel.last_activity_at,
                    "readOnly": channel.read_only,
                    "creator": channel.creator,
                    "createdAt": channel.created_at,
                    "parent": channel.parent_channel_id,
                })
            })
            .collect(),
    )
}

/// What `session/watch` follows, as its JSON now.
fn session_value(what: &str, channel: Option<&str>, cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let snapshot = &community.read(cx).snapshot;
    Ok(match what {
        "channels" => channels(cx),
        "typing" => {
            let channel = channel.ok_or_else(|| invalid("typing takes a channel"))?;
            json!({
                "channel": snapshot.typing.get(channel).cloned().unwrap_or_default(),
                "anywhere": snapshot.typing_anywhere.get(channel).cloned().unwrap_or_default(),
            })
        }
        "presence" => Value::Object(
            snapshot
                .presence
                .iter()
                .map(|(key, state)| (key.clone(), json!(state.as_str())))
                .collect(),
        ),
        "emoji" => Value::Array(
            snapshot
                .emoji
                .entries
                .iter()
                .map(|emoji| json!({ "shortcode": emoji.shortcode, "url": emoji.url }))
                .collect(),
        ),
        "activity" => {
            let channel = channel.ok_or_else(|| invalid("activity takes a channel"))?;
            match snapshot.unread.activity.get(channel) {
                Some(activity) => json!(activity.items.as_ref().map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            json!({
                                "root": item.root_id,
                                "latestMessage": item.latest_message_id,
                                "author": item.author_id,
                                "createdAt": item.created_at,
                                "preview": item.preview,
                                "unreadCount": item.unread_count,
                            })
                        })
                        .collect::<Vec<_>>()
                })),
                None => json!([]),
            }
        }
        _ => {
            return Err(invalid(
                "what must be channels, typing, presence, emoji or activity",
            ));
        }
    })
}

/// `session/watch`: sends `session/changed` with `what` (`channels`,
/// `typing` or `activity` in a `channel`, `presence`, `emoji`) now and
/// whenever it changes.
pub fn watch_session(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let community = community(cx)?;
    let what = text(params, "what").unwrap_or_default().to_owned();
    let channel = text(params, "channel").map(str::to_owned);
    let first = session_value(&what, channel.as_deref(), cx)?;
    let last = std::cell::RefCell::new(first.clone());
    let notify = std::rc::Rc::new(notify);
    let (send, kind) = (notify.clone(), what.clone());
    cx.defer(move |cx| send(json!({ "what": kind, "value": first }), cx));
    let subscription = cx.observe(&community, move |_, cx| {
        let Ok(now) = session_value(&what, channel.as_deref(), cx) else {
            return;
        };
        if now != *last.borrow() {
            *last.borrow_mut() = now.clone();
            notify(json!({ "what": what, "value": now }), cx);
        }
    });
    Ok((Watch::new(subscription, Task::ready(())), json!({})))
}

/// `names/get`: display names as Buzz shows them, through the naming
/// policy on, with the npub suffix that told namesakes apart, if any.
pub fn names(params: &Value, cx: &App) -> Result<Value, Failure> {
    use crate::ui::identity;
    let keys = pubkeys(params, MAX_PROFILES)?;
    let community = community(cx)?;
    let community = community.read(cx);
    let library = identity::AgentLibrary::from_hosts(cx);
    let viewer = community.session.viewer().to_string();
    let names = identity::names(
        &community.snapshot.profiles,
        &library,
        &viewer,
        Some(&community.origin),
        None,
    );
    Ok(Value::Object(
        keys.into_iter()
            .map(|key| {
                let value = json!({
                    "name": names.display(&key),
                    "qualifier": names.qualifier(&key),
                });
                (key, value)
            })
            .collect(),
    ))
}

/// `presence/get`: who is online, away or offline, for the people Buzz
/// already follows; `null` for anyone it does not.
pub fn presence(params: &Value, cx: &App) -> Result<Value, Failure> {
    let keys = pubkeys(params, MAX_PROFILES)?;
    let community = community(cx)?;
    let presence = community.read(cx).snapshot.presence.clone();
    Ok(Value::Object(
        keys.into_iter()
            .map(|key| {
                let state = presence.get(&key).map(|state| state.as_str());
                (key, json!(state))
            })
            .collect(),
    ))
}

/// `statuses/get`: people's current statuses, read fresh; `null` for
/// none.
pub fn statuses(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let keys = pubkeys(params, 20)?;
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let mut statuses = serde_json::Map::new();
        for key in keys {
            let status = session
                .user_status(&key)
                .await
                .map_err(|error| (2, error))?
                .map(|status| {
                    json!({
                        "text": status.text,
                        "emoji": status.emoji,
                        "updatedAt": status.updated_at,
                        "expiresAt": status.expires_at,
                    })
                });
            statuses.insert(key, json!(status));
        }
        Ok(Value::Object(statuses))
    }))
}

/// `emoji/list`: the community's custom emoji.
pub fn emoji(cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let emoji = community.read(cx).snapshot.emoji.clone();
    Ok(Value::Array(
        emoji
            .entries
            .iter()
            .map(|emoji| json!({ "shortcode": emoji.shortcode, "url": emoji.url }))
            .collect(),
    ))
}

/// `typing/get`: who is typing in a channel, or in one of its threads.
pub fn typing(params: &Value, cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let community = community.read(cx);
    let channel = channel(params, community)?;
    let people = match text(params, "root") {
        Some(root) => community.snapshot.thread_typing.get(root).cloned(),
        None => community.snapshot.typing.get(&channel).cloned(),
    };
    Ok(json!(people.unwrap_or_default()))
}

/// `people/search`: people in the community whose profile matches
/// `query`, a page at a time.
pub fn search_people(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let query = text(params, "query")
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .ok_or_else(|| invalid("query is required"))?
        .to_owned();
    let page = params
        .get("page")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let found = session
            .search_members(&query, page)
            .await
            .map_err(|error| (2, error))?;
        Ok(json!({
            "people": found
                .people
                .iter()
                .map(|person| profile(&person.pubkey, Some(&Arc::new(person.profile.clone()))))
                .collect::<Vec<_>>(),
            "more": found.more,
        }))
    }))
}

/// `ui/navigate`: shows a page or Settings.
pub fn navigate(params: &Value, cx: &mut App) -> Result<Value, Failure> {
    use crate::ui::pages::Page;
    use crate::ui::settings::Section;
    match text(params, "to") {
        Some("back") => {
            cx.dispatch_action(&crate::ui::navigation::GoBack);
            return Ok(Value::Null);
        }
        Some("forward") => {
            cx.dispatch_action(&crate::ui::navigation::GoForward);
            return Ok(Value::Null);
        }
        _ => {}
    }
    let place = match text(params, "to") {
        Some("home") | Some("messages") => Place::Page(Page::MESSAGES),
        Some("me") => Place::Page(Page::ME),
        Some("agents") => Place::Page(Page::AGENTS),
        Some("settings") => Place::Settings(match text(params, "section") {
            None => Section::default_for(AppState::current(cx).is_some()),
            Some("profile") => Section::Profile,
            Some("custom-emoji") => Section::CustomEmoji,
            Some("communities") => Section::Communities,
            Some("appearance") => Section::Appearance,
            Some("notifications") => Section::Notifications,
            Some("shortcuts") => Section::Shortcuts,
            Some("agents") => Section::Agents,
            Some("plugins") => Section::Plugins,
            Some("updates") => Section::Updates,
            Some("about") => Section::About,
            Some(card) => {
                let card = crate::ui::settings::cards::get(card, cx)
                    .ok_or_else(|| invalid(format!("Settings has no section {card}")))?;
                Section::Card(card.key)
            }
        }),
        Some("page") => {
            let key = text(params, "page").ok_or_else(|| invalid("page is required"))?;
            let spec = crate::ui::pages::all(cx)
                .into_iter()
                .find(|spec| spec.page.key() == key)
                .ok_or_else(|| invalid(format!("There is no page {key}")))?;
            match params.get("params").filter(|params| !params.is_null()) {
                None => Place::Page(spec.page),
                Some(route) => {
                    let routes = crate::ui::pages::routes(spec.page, cx)
                        .ok_or_else(|| invalid(format!("The page {key} takes no routes")))?;
                    if !(routes.validate)(route) {
                        return Err(invalid(format!("The page {key} cannot open these params")));
                    }
                    Place::Route {
                        page: spec.page,
                        params: route.clone(),
                    }
                }
            }
        }
        _ => {
            return Err(invalid(
                "to must be home, me, agents, settings, page, back or forward",
            ));
        }
    };
    let target = Target {
        community: scope(params, cx)?,
        place,
    };
    let sender = navigation::sender(cx).ok_or((1, "The window is not open.".to_owned()))?;
    let _ = sender.unbounded_send(Request::Open(target).into());
    Ok(Value::Null)
}

/// The community a navigation targets: `community` (a joined community's
/// relay, `wss://…`), else the selected one.
fn scope(params: &Value, cx: &App) -> Result<Option<String>, Failure> {
    match text(params, "community") {
        None => Ok(AppState::current(cx).map(|community| community.read(cx).origin.to_string())),
        Some(relay) => cx
            .global::<AppState>()
            .communities
            .iter()
            .map(|community| community.read(cx))
            .find(|community| community.origin.websocket() == relay)
            .map(|community| Some(community.origin.to_string()))
            .ok_or_else(|| invalid(format!("You have not joined {relay}"))),
    }
}

/// `ui/openLink`: opens a link as one clicked in a message does: a
/// `buzz://` link in Buzz, a web link in the panel that shows it or the
/// browser.
pub fn open_link(params: &Value, cx: &mut App) -> Result<Value, Failure> {
    let url = text(params, "url")
        .filter(|url| {
            url.starts_with("buzz://") || url.starts_with("https://") || url.starts_with("http://")
        })
        .ok_or_else(|| invalid("url must be a buzz:// or web link"))?
        .to_owned();
    let window =
        crate::ui::navigation::main_window(cx).ok_or((1, "The window is not open.".to_owned()))?;
    window
        .update(cx, |_, window, cx| {
            if url.starts_with("buzz://") {
                navigation::open_url(&url, window, cx);
            } else {
                navigation::open_web_link(&url, Modifiers::default(), window, cx);
            }
        })
        .map_err(|_| (1, "The window is not open.".to_owned()))?;
    Ok(Value::Null)
}

/// `ui/openPanel`: opens one of the plugin's panels (by `panel` id) as a
/// tab of the open conversation, showing `target`.
pub fn open_panel(plugin: &str, params: &Value, cx: &mut App) -> Result<Value, Failure> {
    let panel = text(params, "panel").ok_or_else(|| invalid("panel is required"))?;
    let key = format!("{plugin}/{panel}");
    if crate::ui::channel_panels::get(&key, cx).is_none() {
        return Err(invalid(format!("The plugin has no panel {panel}")));
    }
    let target = text(params, "target")
        .filter(|target| target.len() <= 2048)
        .ok_or_else(|| invalid("target is required"))?
        .to_owned();
    navigation::with_workspace(cx, move |workspace, window, cx| {
        workspace.open_panel_at(Some(&key), &target, None, None, window, cx);
    });
    Ok(Value::Null)
}

/// `notifications/submit`: a notification in one of the plugin's
/// categories, through the user's preferences. `true` if it showed.
pub fn notify(plugin: &str, params: &Value, cx: &mut App) -> Result<Value, Failure> {
    use crate::notifications::categories::{self, Submission};
    let category = text(params, "category").ok_or_else(|| invalid("category is required"))?;
    let source = text(params, "sourceKey")
        .filter(|key| !key.is_empty() && key.len() <= 256)
        .ok_or_else(|| invalid("sourceKey is required"))?;
    let community = AppState::current(cx);
    let place = match (text(params, "channel"), &community) {
        (Some(channel), Some(community)) => {
            let channel = self::channel(&json!({ "channel": channel }), community.read(cx))?;
            let message = event_id(params, "message")?;
            let thread = event_id(params, "thread")?;
            Place::Conversation {
                channel,
                message: message.or(thread),
                thread,
                members: false,
            }
        }
        _ => match text(params, "page").and_then(|key| {
            crate::ui::pages::all(cx)
                .into_iter()
                .find(|spec| spec.page.key() == key)
        }) {
            // A plugin page a click opens.
            Some(spec) => Place::Page(spec.page),
            None => Place::Page(crate::ui::pages::Page::MESSAGES),
        },
    };
    let target = Target {
        community: community.map(|community| community.read(cx).origin.to_string()),
        place,
    };
    let shown = categories::submit(
        &format!("{plugin}/{category}"),
        Submission {
            source_key: source.to_owned(),
            title: text(params, "title").unwrap_or_default().to_owned(),
            body: text(params, "body").unwrap_or_default().to_owned(),
            target,
        },
        cx,
    );
    Ok(Value::Bool(shown))
}

/// Something a plugin follows, live until dropped: a channel, a thread or
/// events.
pub struct Watch {
    _keep: Box<dyn std::any::Any>,
    _forward: Task<()>,
}

impl Watch {
    pub fn new(keep: impl std::any::Any, forward: Task<()>) -> Self {
        Self {
            _keep: Box::new(keep),
            _forward: forward,
        }
    }
}

/// `thread/watch`: sends `thread/changed` with the root and replies now
/// and whenever the thread changes.
pub fn watch_thread(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let root = event_id(params, "root")?.ok_or_else(|| invalid("root is required"))?;
    let view = community
        .read(cx)
        .session
        .open_thread(&channel, root, false);
    let mut receiver = view.snapshot.clone();
    let (sender, mut values) = futures::channel::mpsc::unbounded();
    let _ = sender.unbounded_send(receiver.borrow_and_update().clone());
    Runtime::handle(cx).spawn(async move {
        while receiver.changed().await.is_ok() {
            let value = receiver.borrow_and_update().clone();
            if sender.unbounded_send(value).is_err() {
                return;
            }
        }
    });
    let thread_channel = channel.clone();
    let forward = cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some(mut thread) = values.next().await {
            while let Ok(newer) = values.try_recv() {
                thread = newer;
            }
            if !matches!(thread.status, ThreadStatus::Ready | ThreadStatus::Error) {
                continue;
            }
            cx.update(|cx| {
                let profiles = community.read(cx).snapshot.profiles.clone();
                notify(
                    json!({
                        "channel": thread_channel,
                        "root": thread.root.as_ref().map(|row| message(row, &profiles)),
                        "replies": thread
                            .replies
                            .iter()
                            .map(|row| message(row, &profiles))
                            .collect::<Vec<_>>(),
                        "hasMore": thread.can_load_more,
                        "error": thread.error,
                    }),
                    cx,
                );
            });
        }
    });
    Ok((
        Watch::new(view, forward),
        json!({ "channel": channel, "root": root.to_hex() }),
    ))
}

/// `channel/watch`: sends `channel/changed` with the newest messages now
/// and whenever the channel changes, until `channel/unwatch` or the
/// plugin stops.
pub fn watch(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
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
    Ok((Watch::new(view, forward), json!({ "channel": channel })))
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
