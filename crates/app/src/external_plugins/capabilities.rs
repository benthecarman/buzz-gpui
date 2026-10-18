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
/// How long a read waits for the relay to confirm rows shown from disk.
const FRESH_TIMEOUT: Duration = Duration::from_secs(5);
/// Most messages one answer carries.
pub const MAX_MESSAGES: usize = 200;
const DEFAULT_MESSAGES: usize = 50;
const MAX_PROFILES: usize = 500;

pub type Failure = (i64, String);

fn invalid(message: impl Into<String>) -> Failure {
    (INVALID_PARAMS, message.into())
}

thread_local! {
    /// The community the request being answered names, if any.
    static SCOPE: std::cell::RefCell<Option<Entity<Community>>> = const { std::cell::RefCell::new(None) };
}

/// Runs `answer` with requests reaching the joined community at
/// `params.community` (its `wss://` relay) rather than the selected one,
/// as buzz-app's `agents.relay` reaches a running agent's community.
pub fn in_community<T>(
    params: &Value,
    cx: &mut App,
    answer: impl FnOnce(&mut App) -> T,
) -> Result<T, Failure> {
    let scope = match params.get("community").and_then(Value::as_str) {
        None => None,
        Some(relay) => Some(
            cx.global::<AppState>()
                .communities
                .iter()
                .find(|community| {
                    let origin = &community.read(cx).origin;
                    origin.websocket() == relay || origin.to_string() == relay
                })
                .cloned()
                .ok_or_else(|| invalid(format!("You have not joined {relay}")))?,
        ),
    };
    let previous = SCOPE.with_borrow_mut(|current| std::mem::replace(current, scope));
    let answered = answer(cx);
    SCOPE.with_borrow_mut(|current| *current = previous);
    Ok(answered)
}

/// The community a request reaches: the one it names, or the selected
/// one; an error a plugin can show without either.
pub fn community(cx: &App) -> Result<Entity<Community>, Failure> {
    if let Some(scoped) = SCOPE.with_borrow(Clone::clone) {
        return Ok(scoped);
    }
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

/// `channel` for a read: one listed here, or any well-formed id, such as
/// an open channel you have not joined, which the relay shows as a
/// preview or refuses (`channels.window`).
pub fn readable(params: &Value, community: &Community) -> Result<String, Failure> {
    channel(params, community).or_else(|failure| {
        text(params, "channel")
            .filter(|id| {
                id.len() <= 128
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
            .map(str::to_owned)
            .ok_or(failure)
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
        // A relay-signed join, leave or removal row (`ChannelMessage.membership`).
        "membership": row.membership.map(|change| json!({
            "type": match change.kind {
                buzz_relay_client::fold::MembershipKind::Joined => "member_joined",
                buzz_relay_client::fold::MembershipKind::Left => "member_left",
                buzz_relay_client::fold::MembershipKind::Removed => "member_removed",
            },
            "actor": change.actor.to_hex(),
            "target": change.target.to_hex(),
        })),
        "reactions": row.reactions.iter().map(|group| json!({
            "content": group.content,
            "emojiUrl": group.emoji.as_ref().map(|emoji| emoji.url.clone()),
            "count": group.events.len(),
            "authors": group.events.iter().map(|reaction| reaction.author.to_hex()).collect::<Vec<_>>(),
            // Each reaction event, so its author can delete it.
            "events": group.events.iter().map(|reaction| json!({
                "id": reaction.id.to_hex(),
                "author": reaction.author.to_hex(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "attachments": row.attachments.iter().map(|attachment| json!({
            "url": attachment.url,
            "kind": match attachment.kind {
                buzz_relay_client::fold::AttachmentKind::Image => "image",
                buzz_relay_client::fold::AttachmentKind::Video => "video",
                buzz_relay_client::fold::AttachmentKind::Audio => "audio",
                buzz_relay_client::fold::AttachmentKind::File => "file",
            },
            "mime": attachment.mime,
            "size": attachment.size,
            "name": attachment.name,
            "duration": attachment.duration,
            "waveform": attachment.waveform,
            "voiceNote": attachment.voice_note,
            "dimensions": attachment.dimensions.map(|size| json!({
                "width": size.width,
                "height": size.height,
            })),
            "blurhash": attachment.blurhash,
            "previewUrl": attachment.preview_url,
        })).collect::<Vec<_>>(),
        "mentionReferences": row
            .mention_references
            .iter()
            .map(|key| key.to_hex())
            .collect::<Vec<_>>(),
        "sourceContent": row.source_content,
        "sentFromThread": row.sent_from_thread.as_ref().map(|sent| json!({
            "root": sent.root_id.to_hex(),
            "excerpt": sent.excerpt,
        })),
        "diff": row.diff.as_ref().map(|diff| json!({
            "filePath": diff.file_path,
            "repoUrl": diff.repo_url,
            "commitSha": diff.commit_sha,
            "description": diff.description,
            "truncated": diff.truncated,
        })),
        "deliveryError": row.delivery.as_ref().and_then(|delivery| delivery.error.clone()),
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
pub fn timeline(
    snapshot: &TimelineSnapshot,
    limit: usize,
    membership: bool,
    profiles: &Profiles,
) -> Value {
    let rows: Vec<&Arc<MessageRow>> = snapshot
        .rows
        .iter()
        .filter(|row| membership || row.membership.is_none())
        .collect();
    let start = rows.len().saturating_sub(limit);
    json!({
        "channel": snapshot.channel,
        "messages": rows[start..].iter().map(|row| message(row, profiles)).collect::<Vec<_>>(),
        "hasMore": snapshot.has_more || start > 0,
        // The window's state (`ChannelWindow`).
        "status": match &snapshot.status {
            TimelineStatus::Loading => "loading",
            TimelineStatus::Ready => "ready",
            TimelineStatus::Error(_) => "error",
        },
        "loadingOlder": snapshot.loading_older,
        "historyLimited": snapshot.history_limited,
        "restored": snapshot.restored,
        "stale": snapshot.stale,
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
            // An agent's attested owner (NIP-OA), if its profile names one.
            "owner": profile.owner_pubkey,
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
    let channel = readable(params, community.read(cx))?;
    let limit = limit(params);
    let membership = params.get("membership").and_then(Value::as_bool) == Some(true);
    let session = community.read(cx).session.clone();
    let wait = on_runtime(cx, async move {
        let view = session.open_channel(&channel);
        let mut snapshot = view.snapshot.clone();
        // A head restored from disk is ready before the relay confirms
        // it; wait a while for the relay's read, then answer with what
        // there is.
        let fresh = snapshot.wait_for(|timeline| {
            timeline.status != TimelineStatus::Loading
                && (!timeline.stale || timeline.error.is_some())
        });
        let _ = tokio::time::timeout(FRESH_TIMEOUT, fresh).await;
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
            timeline(&snapshot, limit, membership, &profiles)
        }))
    }))
}

/// `thread/messages`: a thread's root and replies.
pub fn thread_messages(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = readable(params, community.read(cx))?;
    let (root, exact) = thread_target(params)?;
    let session = community.read(cx).session.clone();
    let thread_channel = channel.clone();
    let wait = on_runtime(cx, async move {
        let view = session.open_thread(&channel, root, exact);
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
            thread_json(&thread_channel, &thread, &profiles)
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
    // A custom emoji outside the community's catalog, such as one on
    // another reaction: `emojiUrl` with `emoji` as `:shortcode:`.
    let custom = match text(params, "emojiUrl") {
        None => None,
        Some(url) if url.starts_with("https://") => {
            let shortcode = emoji
                .strip_prefix(':')
                .and_then(|rest| rest.strip_suffix(':'))
                .filter(|code| {
                    !code.is_empty()
                        && code
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                })
                .ok_or_else(|| invalid("a custom emoji is written :shortcode:"))?;
            Some(buzz_relay_client::messages::CustomEmoji {
                shortcode: shortcode.to_owned(),
                url: url.to_owned(),
            })
        }
        Some(_) => return Err(invalid("emojiUrl must be https")),
    };
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        match custom {
            Some(custom) => {
                session
                    .react_with(&channel, message, &emoji, Some(custom))
                    .await
            }
            None => session.react(&channel, message, &emoji).await,
        }
        .map(|()| Value::Null)
        .map_err(|error| (3, error))
    }))
}

/// `messages/delete`: deletes one of the user's messages or reactions, or
/// a message of an agent the user owns.
pub fn delete(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    // Several at once (`messages.remove` takes up to 100).
    if let Some(ids) = params.get("messages") {
        let community = community(cx)?;
        let channel = channel(params, community.read(cx))?;
        let ids: Vec<EventId> = ids
            .as_array()
            .filter(|ids| !ids.is_empty() && ids.len() <= 100)
            .ok_or_else(|| invalid("messages must be a list of 1 to 100 ids"))?
            .iter()
            .map(|id| {
                id.as_str()
                    .and_then(|hex| EventId::from_hex(hex).ok())
                    .ok_or_else(|| invalid("messages must be event ids"))
            })
            .collect::<Result<_, _>>()?;
        let session = community.read(cx).session.clone();
        return Ok(on_tokio(cx, async move {
            let mut failed = serde_json::Map::new();
            for id in &ids {
                if let Err(error) = session.remove(&channel, *id).await {
                    failed.insert(id.to_hex(), json!(error));
                }
            }
            Ok(json!({ "deleted": ids.len() - failed.len(), "failed": failed }))
        }));
    }
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
pub fn upload(
    plugin: &str,
    params: &Value,
    cx: &App,
) -> Result<Task<Result<Value, Failure>>, Failure> {
    use base64::Engine;
    use buzz_relay_client::upload::{Source, Stage, Upload};
    let community = community(cx)?;
    // A file on disk, up to the relay's limit for its type, or a small
    // file inline.
    let (source, size, name) = match (text(params, "path"), text(params, "data")) {
        (Some(path), _) => {
            let path = std::path::PathBuf::from(path);
            let size = std::fs::metadata(&path)
                .ok()
                .filter(|meta| meta.is_file())
                .map(|meta| meta.len())
                .ok_or_else(|| invalid("path is not a readable file"))?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_owned());
            (Source::Path(path), size, name)
        }
        (None, Some(data)) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| invalid("data is not base64"))?;
            if bytes.len() > MAX_UPLOAD {
                return Err(invalid(
                    "Inline data is at most 3 MiB; pass a path instead.",
                ));
            }
            let size = bytes.len() as u64;
            (Source::Bytes(bytes.into()), size, "file".to_owned())
        }
        (None, None) => return Err(invalid("path or data (base64) is required")),
    };
    if size == 0 {
        return Err(invalid("The file is empty."));
    }
    let name = text(params, "name")
        .map(|name| name.chars().take(200).collect::<String>())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(name);
    let mime = match text(params, "mime") {
        Some(mime) => mime.to_owned(),
        None => buzz_relay_client::upload::mime_type(&name, &head(&source)),
    };
    if size > buzz_relay_client::messages::media_byte_limit(&mime) {
        return Err(invalid(
            "The file is larger than the community takes for its type.",
        ));
    }
    // `progress` names the upload in `files/progress` notifications.
    let progress = text(params, "progress").map(str::to_owned);
    let (plugin, sender) = (plugin.to_owned(), progress_sender(cx));
    let media = community.read(cx).session.media().clone();
    Ok(on_tokio(cx, async move {
        let upload = Upload::new(name, mime, source);
        let report = move |stage: Stage| {
            let Some(token) = &progress else {
                return;
            };
            let value = match stage {
                Stage::Preparing => json!({ "progress": token, "stage": "preparing" }),
                Stage::Uploading => json!({ "progress": token, "stage": "uploading" }),
                Stage::Sent { sent, total } => {
                    json!({ "progress": token, "stage": "sending", "sent": sent, "total": total })
                }
            };
            let _ = sender.unbounded_send((plugin.clone(), value));
        };
        let attachment = buzz_relay_client::upload::send(&media, upload, report)
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

/// The first bytes of `source`, to tell its type.
fn head(source: &buzz_relay_client::upload::Source) -> Vec<u8> {
    use buzz_relay_client::upload::Source;
    match source {
        Source::Bytes(bytes) => bytes[..bytes.len().min(512)].to_vec(),
        Source::Path(path) => {
            use std::io::Read;
            let mut head = vec![0; 512];
            let read = std::fs::File::open(path)
                .and_then(|mut file| file.read(&mut head))
                .unwrap_or(0);
            head.truncate(read);
            head
        }
    }
}

type Progress = futures::channel::mpsc::UnboundedSender<(String, Value)>;

thread_local! {
    static PROGRESS: std::cell::RefCell<Option<Progress>> = const { std::cell::RefCell::new(None) };
}

/// Where upload progress goes to reach the plugin as `files/progress`.
fn progress_sender(cx: &App) -> Progress {
    if let Some(sender) = PROGRESS.with_borrow(Clone::clone) {
        return sender;
    }
    let (sender, mut reports) = futures::channel::mpsc::unbounded::<(String, Value)>();
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some((plugin, value)) = reports.next().await {
            cx.update(|cx| {
                super::ExternalPlugins::notify_running(&plugin, "files/progress", value, cx)
            });
        }
    })
    .detach();
    PROGRESS.with_borrow_mut(|progress| *progress = Some(sender.clone()));
    sender
}

/// `media/url`: a loopback `http://127.0.0.1` URL the plugin can fetch or
/// stream community media from, of any size (`session.media`), or `null`
/// when Buzz cannot serve it so.
pub fn media_url(params: &Value, cx: &App) -> Result<Value, Failure> {
    let url = text(params, "url").ok_or_else(|| invalid("url is required"))?;
    let media = community(cx)?.read(cx).session.media().clone();
    Ok(json!(media.stream_url(url)))
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
    let references: Vec<String> = params
        .get("references")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    // `{pubkey: {"model", "effort"}}` for mentioned agents.
    let picks = params
        .get("picks")
        .and_then(Value::as_object)
        .map(|picks| {
            picks
                .iter()
                .map(|(key, pick)| {
                    let field =
                        |name: &str| pick.get(name).and_then(Value::as_str).map(str::to_owned);
                    (
                        key.to_ascii_lowercase(),
                        buzz_relay_client::messages::RunPick {
                            model: field("model"),
                            effort: field("effort"),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mentions = Mentions {
        recipients,
        references,
        picks,
    };
    let attachments = attachments(params)?;
    let session: Arc<Session> = community.read(cx).session.clone();
    let sent = session.send(&channel, &content, mentions, attachments, thread);
    Ok(on_tokio(cx, async move {
        sent.await.map_err(|error| (3, error))?;
        // The outbox holds it, delivered or not, for `outbox/get`.
        let id = session.queued_message(&channel, &content).await;
        Ok(json!({ "id": id.map(|id| id.to_hex()) }))
    }))
}

/// The place `ui/navigate`'s params name, other than Back and Forward.
fn place_of(params: &Value, cx: &App) -> Result<Place, Failure> {
    use crate::ui::pages::Page;
    use crate::ui::settings::Section;
    Ok(match text(params, "to") {
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
                    let version = params.get("version").and_then(Value::as_u64);
                    if version.is_some_and(|version| version != u64::from(routes.version)) {
                        return Err(invalid(format!(
                            "The page {key} takes routes of version {}",
                            routes.version
                        )));
                    }
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
        Some("conversation") => {
            // Any well-formed id: the visit looks up a channel not listed
            // here, such as an open one you have not joined, and fails as
            // `not-found` when there is none (`open()`).
            let channel = text(params, "channel")
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= 128
                        && id
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                })
                .ok_or_else(|| invalid("channel is required"))?
                .to_owned();
            let message = event_id(params, "message")?;
            let thread = event_id(params, "thread")?;
            Place::Conversation {
                channel,
                message: message.or(thread),
                thread,
                members: params.get("members").and_then(Value::as_bool) == Some(true),
            }
        }
        _ => {
            return Err(invalid(
                "to must be home, me, agents, settings, page, conversation, back or forward",
            ));
        }
    })
}

/// A place as `ui/navigate` takes it, so a plugin can return to it.
fn place_json(target: &Target, cx: &App) -> Value {
    use crate::ui::pages::Page;
    use crate::ui::settings::Section;
    let mut place = match &target.place {
        Place::Page(page) if *page == Page::MESSAGES => json!({ "to": "home" }),
        Place::Page(page) if *page == Page::ME => json!({ "to": "me" }),
        Place::Page(page) if *page == Page::AGENTS => json!({ "to": "agents" }),
        Place::Page(page) if *page == Page::SETTINGS => json!({ "to": "settings" }),
        Place::Page(page) => json!({ "to": "page", "page": page.key() }),
        Place::Settings(section) => json!({
            "to": "settings",
            "section": match section {
                Section::Profile => "profile",
                Section::CustomEmoji => "custom-emoji",
                Section::Communities => "communities",
                Section::Appearance => "appearance",
                Section::Notifications => "notifications",
                Section::Shortcuts => "shortcuts",
                Section::Agents => "agents",
                Section::Plugins => "plugins",
                Section::Updates => "updates",
                Section::About => "about",
                Section::Card(key) => key,
            },
        }),
        Place::Conversation {
            channel,
            message,
            thread,
            members,
        } => json!({
            "to": "conversation",
            "channel": channel,
            "message": message.map(|id| id.to_hex()),
            "thread": thread.map(|id| id.to_hex()),
            "members": members,
        }),
        Place::Channels(_) => json!({ "to": "home" }),
        Place::Route { page, params } => json!({
            "to": "page",
            "page": page.key(),
            "params": params,
            "version": crate::ui::pages::routes(*page, cx).map(|routes| routes.version),
        }),
    };
    place["community"] = json!(target.community);
    place
}

/// `navigation/get`: where the window is, as `ui/navigate` takes it, and
/// whether Back and Forward go anywhere.
/// buzz-app's `OpenFailure` for `reason`.
fn reason_name(reason: crate::ui::channel_states::Reason) -> &'static str {
    use crate::ui::channel_states::Reason;
    match reason {
        Reason::Denied => "denied",
        Reason::Invalid => "invalid-target",
        Reason::Page => "unavailable",
        Reason::NotFound => "not-found",
        Reason::Timeout => "timeout",
        Reason::HostError => "host-error",
    }
}

pub fn navigation_state(cx: &App) -> Value {
    let Some(workspace) = navigation::workspace(cx) else {
        return Value::Null;
    };
    let workspace = workspace.read(cx);
    let (here, back, forward) = workspace.location();
    let failed = workspace.failed().map(|(failed, retryable)| {
        json!({
            "place": failed.target.as_ref().map(|target| place_json(target, cx)),
            "reason": reason_name(failed.reason),
            "retryable": retryable,
            "ingress": failed.ingress,
        })
    });
    let (attempt, opening) = workspace.visit();
    json!({
        "status": match (&failed, opening) {
            (Some(_), _) => "failed",
            (None, true) => "opening",
            (None, false) => "opened",
        },
        "entry": attempt,
        "place": here.map(|here| place_json(&here, cx)),
        "failed": failed,
        "canGoBack": back,
        "canGoForward": forward,
    })
}

/// `navigation/retry`: opens the visit that failed again.
pub fn retry_navigation(cx: &mut App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let before = navigation::workspace(cx)
        .map(|workspace| workspace.read(cx).visit().0)
        .ok_or((1, "The window is not open.".to_owned()))?;
    navigation::with_workspace(cx, |workspace, window, cx| {
        workspace.retry_failed(window, cx)
    });
    Ok(settle(before, cx))
}

/// `navigation/watch`: sends `navigation/changed` with
/// `navigation/get`'s answer now and whenever it changes.
pub fn watch_navigation(
    _params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let workspace = navigation::workspace(cx).ok_or((1, "The window is not open.".to_owned()))?;
    let first = navigation_state(cx);
    let last = std::cell::RefCell::new(first.clone());
    let notify = std::rc::Rc::new(notify);
    let send = notify.clone();
    cx.defer(move |cx| send(first, cx));
    let subscription = cx.observe(&workspace, move |_, cx| {
        let now = navigation_state(cx);
        if now != *last.borrow() {
            *last.borrow_mut() = now.clone();
            notify(now, cx);
        }
    });
    Ok((Watch::new(subscription, Task::ready(())), json!({})))
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
    let Some(community) = AppState::current(cx) else {
        return json!([]);
    };
    Value::Array(
        community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .map(channel_json)
            .collect(),
    )
}

/// One channel as plugins see it (`ChannelSummary`).
pub fn channel_json(channel: &buzz_relay_client::ChannelSummary) -> Value {
    use buzz_relay_client::ChannelType;
    json!({
        "id": channel.id,
        "name": channel.name,
        "description": channel.description,
        "private": channel.private,
        // `public`, `private`, or `null` when the relay has not said.
        "visibility": channel.visibility.map(|visibility| match visibility {
            buzz_relay_client::Visibility::Public => "public",
            buzz_relay_client::Visibility::Private => "private",
        }),
        "archived": channel.archived,
        // A channel without metadata yet is shown as a stream.
        "kind": match channel.channel_type {
            Some(ChannelType::Dm) => "dm",
            Some(ChannelType::Forum) => "forum",
            Some(ChannelType::Session) => "session",
            Some(ChannelType::Stream) | None => "stream",
        },
        "typeKnown": channel.channel_type.is_some(),
        "members": channel.members,
        "participants": channel.participants,
        "labels": channel.labels,
        "lastActivityAt": channel.last_activity_at,
        "updatedAt": channel.updated_at,
        "preview": channel.preview,
        "readOnly": channel.read_only,
        "cached": channel.cached,
        "hidden": channel.hidden,
        "creator": channel.creator,
        "createdAt": channel.created_at,
        "parent": channel.parent_channel_id,
    })
}

/// `channels/list`: the channel list with how far it got
/// (`channels.list` with its `status` and `error`), and the DMs you hid.
pub fn channel_list(cx: &App) -> Result<Value, Failure> {
    use buzz_relay_client::session::{DmVisibilityStatus, RosterStatus};
    let community = community(cx)?;
    let snapshot = &community.read(cx).snapshot;
    let (status, error) = match &snapshot.roster {
        RosterStatus::Loading => ("loading", None),
        RosterStatus::Ready => ("ready", None),
        RosterStatus::Error(error) => ("error", Some(error.clone())),
    };
    let (hidden_status, hidden_error) = match &snapshot.dm_visibility {
        DmVisibilityStatus::Idle => ("idle", None),
        DmVisibilityStatus::Loading => ("loading", None),
        DmVisibilityStatus::Ready => ("ready", None),
        DmVisibilityStatus::Error(error) => ("error", Some(error.clone())),
    };
    Ok(json!({
        "status": status,
        "error": error,
        "cached": snapshot.cached,
        "channels": snapshot.channels.iter().map(channel_json).collect::<Vec<_>>(),
        "hiddenDms": snapshot.hidden_dms.iter().collect::<Vec<_>>(),
        "hiddenStatus": hidden_status,
        "hiddenError": hidden_error,
    }))
}

/// `channels/searchPublic`: open channels you have not joined whose name
/// has `query` (or is it, with `exact`), as previews.
pub fn search_public(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let query = text(params, "query").unwrap_or_default().to_owned();
    let exact = params.get("exact").and_then(Value::as_bool) == Some(true);
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let page = session
            .search_public_channel_page(&query, exact)
            .await
            .map_err(|error| (3, error))?;
        Ok(json!({
            "channels": page.channels.iter().map(channel_json).collect::<Vec<_>>(),
            "partial": page.partial,
        }))
    }))
}

/// `channels/findLabeled`: the channels `creator` made with `label`,
/// oldest first (NIP-CL `findLabeled`).
pub fn find_labeled(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let creator = text(params, "creator")
        .ok_or_else(|| invalid("creator is required"))?
        .to_ascii_lowercase();
    let label = text(params, "label")
        .ok_or_else(|| invalid("label is required"))?
        .to_owned();
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let found = session
            .find_labeled(&creator, &label)
            .await
            .map_err(|error| (3, error))?;
        Ok(Value::Array(found.iter().map(channel_json).collect()))
    }))
}

/// `channels/resolve`: channels by id, whether or not you joined them,
/// as the relay lets you read them (`channels.get`, `resolve`,
/// `describe`); ones you cannot read are left out.
pub fn resolve_channels(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let ids: Vec<String> = params
        .get("ids")
        .and_then(Value::as_array)
        .filter(|ids| !ids.is_empty() && ids.len() <= 50)
        .ok_or_else(|| invalid("ids must be a list of 1 to 50 channel ids"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let found = session
            .resolve_channels(ids)
            .await
            .map_err(|error| (3, error))?;
        Ok(Value::Array(found.iter().map(channel_json).collect()))
    }))
}

/// What `session/watch` follows, as its JSON now.
fn session_value(what: &str, params: &Value, cx: &App) -> Result<Value, Failure> {
    let channel = text(params, "channel");
    let community = community(cx)?;
    let snapshot = &community.read(cx).snapshot;
    Ok(match what {
        "channels" => channels(cx),
        "channelList" => channel_list(cx)?,
        "sidebar" => super::channels::sidebar(cx)?,
        "sync" => super::reading::sync(false, cx)?,
        "creation" => super::channels::creation(cx)?,
        "profiles" => {
            let keys = pubkeys(params, MAX_PROFILES)?;
            Value::Array(
                keys.iter()
                    .map(|key| profile(key, snapshot.profiles.get(key)))
                    .collect(),
            )
        }
        "typing" => {
            let channel = channel.ok_or_else(|| invalid("typing takes a channel"))?;
            match text(params, "root") {
                // Who is typing in one thread (`typing` with a root).
                Some(root) => json!({
                    "thread": snapshot.thread_typing.get(root).cloned().unwrap_or_default(),
                }),
                None => json!({
                    "channel": snapshot.typing.get(channel).cloned().unwrap_or_default(),
                    "anywhere": snapshot.typing_anywhere.get(channel).cloned().unwrap_or_default(),
                }),
            }
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
            // As `threads/activity` answers.
            match snapshot.unread.activity.get(channel) {
                Some(activity) => super::reading::activity(activity),
                None => json!({ "channel": channel, "items": [], "error": null }),
            }
        }
        _ => {
            return Err(invalid(
                "what must be channels, channelList, sidebar, sync, creation, profiles, typing, \
                 presence, emoji or activity",
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
    let params = params.clone();
    if what == "profiles" {
        // Read the ones not loaded yet, as `profiles.ensure` does.
        let keys = pubkeys(&params, MAX_PROFILES)?;
        community.read(cx).session.want_profiles(keys);
    }
    let first = session_value(&what, &params, cx)?;
    let last = std::rc::Rc::new(std::cell::RefCell::new(first.clone()));
    let notify = std::rc::Rc::new(notify);
    let (send, kind) = (notify.clone(), what.clone());
    cx.defer({
        let kind = kind.clone();
        move |cx| send(json!({ "what": kind, "value": first }), cx)
    });
    let changed = std::rc::Rc::new(move |cx: &mut App| {
        let Ok(now) = session_value(&what, &params, cx) else {
            return;
        };
        if now != *last.borrow() {
            *last.borrow_mut() = now.clone();
            notify(json!({ "what": what, "value": now }), cx);
        }
    });
    let subscription = cx.observe(&community, {
        let changed = changed.clone();
        move |_, cx| changed(cx)
    });
    // Channel creation is not part of the community's snapshot: follow
    // its own state.
    let forward = if kind == "creation" {
        let mut receiver = community.read(cx).session.creation();
        let (sender, mut pokes) = futures::channel::mpsc::unbounded::<()>();
        Runtime::handle(cx).spawn(async move {
            while receiver.changed().await.is_ok() {
                if sender.unbounded_send(()).is_err() {
                    return;
                }
            }
        });
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while pokes.next().await.is_some() {
                cx.update(|cx| changed(cx));
            }
        })
    } else {
        Task::ready(())
    };
    Ok((Watch::new(subscription, forward), json!({})))
}

/// `names/get`: names as Buzz shows them, through the naming policy,
/// with the suffix that tells namesakes apart. `candidates` scopes the
/// namesakes to the keys in view, such as a channel's members.
pub fn names(params: &Value, cx: &App) -> Result<Value, Failure> {
    let keys = pubkeys(params, MAX_PROFILES)?;
    let candidates = candidates(params)?;
    let facts = display_facts(params)?;
    let community = community(cx)?;
    Ok(names_of(
        &community,
        &keys,
        candidates.as_deref(),
        &facts,
        cx,
    ))
}

/// The `displayFacts` param: names this view shows for some keys, such
/// as a draft's recipients, that the naming policy weighs in their place.
fn display_facts(params: &Value) -> Result<Vec<crate::ui::identity::NamingIdentity>, Failure> {
    let Some(facts) = params.get("displayFacts").filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let facts = facts
        .as_array()
        .filter(|facts| facts.len() <= MAX_PROFILES)
        .ok_or_else(|| invalid("displayFacts must be a list of at most 500"))?;
    facts
        .iter()
        .map(|fact| {
            let pubkey = text(fact, "pubkey")
                .filter(|key| key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| invalid("each display fact needs a pubkey"))?;
            Ok(crate::ui::identity::NamingIdentity {
                pubkey: pubkey.to_ascii_lowercase(),
                name: text(fact, "name").unwrap_or_default().to_owned(),
                is_agent: fact
                    .get("isAgent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                owner: text(fact, "owner").map(str::to_ascii_lowercase),
            })
        })
        .collect()
}

/// The `candidates` param: the keys in view, or everyone when absent.
fn candidates(params: &Value) -> Result<Option<Vec<String>>, Failure> {
    match params.get("candidates").filter(|value| !value.is_null()) {
        None => Ok(None),
        Some(_) => {
            let mut keys = pubkeys(&json!({ "pubkeys": params["candidates"] }), 5_000)
                .map_err(|_| invalid("candidates must be a list of pubkeys"))?;
            keys.sort();
            keys.dedup();
            Ok(Some(keys))
        }
    }
}

fn names_of(
    community: &Entity<Community>,
    keys: &[String],
    candidates: Option<&[String]>,
    facts: &[crate::ui::identity::NamingIdentity],
    cx: &App,
) -> Value {
    use crate::ui::identity;
    let community = community.read(cx);
    let library = identity::AgentLibrary::from_hosts(cx);
    let viewer = community.session.viewer().to_string();
    // The requested key always joins the scope.
    let scope = candidates.map(|candidates| {
        let mut scope = candidates.to_vec();
        scope.extend(keys.iter().cloned());
        scope
    });
    let names = identity::names_with(
        &community.snapshot.profiles,
        &library,
        &viewer,
        Some(&community.origin),
        scope.as_deref(),
        facts,
    );
    Value::Object(
        keys.iter()
            .map(|key| {
                let value = json!({
                    "name": names.display(key),
                    "qualifier": names.qualifier(key),
                    "source": names.source(key),
                });
                (key.clone(), value)
            })
            .collect(),
    )
}

/// `names/watch`: sends `names/changed` with `names`, as `names/get`
/// answers, whenever one changes: a profile loads, an agent is renamed,
/// or the naming policy answers or changes.
pub fn watch_names(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let keys = pubkeys(params, MAX_PROFILES)?;
    let candidates = candidates(params)?;
    let facts = display_facts(params)?;
    let community = community(cx)?;
    let first = names_of(&community, &keys, candidates.as_deref(), &facts, cx);
    let mut last = first.clone();
    let forward = cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            cx.update(|cx| {
                let now = names_of(&community, &keys, candidates.as_deref(), &facts, cx);
                if now != last {
                    last = now.clone();
                    notify(json!({ "names": now }), cx);
                }
            });
        }
    });
    Ok((Watch::new((), forward), json!({ "names": first })))
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

/// `presence/watch`: follows up to 64 people's presence, whether or not
/// Buzz shows them, and sends `presence/changed` with their states now
/// and whenever they change.
pub fn watch_presence(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    /// Stops following when the watch ends.
    struct Held(futures::channel::mpsc::UnboundedSender<()>);
    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.0.unbounded_send(());
        }
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let keys = pubkeys(params, 64)?;
    let community = community(cx)?;
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    community.update(cx, |community, _| {
        community.set_presence_plugin(id, keys.clone())
    });
    let states = move |community: &Community| -> Value {
        let presence = &community.snapshot.presence;
        Value::Object(
            keys.iter()
                .map(|key| {
                    (
                        key.clone(),
                        json!(presence.get(key).map(|state| state.as_str())),
                    )
                })
                .collect(),
        )
    };
    let first = states(community.read(cx));
    let last = std::cell::RefCell::new(first.clone());
    let notify = std::rc::Rc::new(notify);
    let send = notify.clone();
    cx.defer(move |cx| send(json!({ "presence": first }), cx));
    let subscription = cx.observe(&community, move |community, cx| {
        let now = states(community.read(cx));
        if now != *last.borrow() {
            *last.borrow_mut() = now.clone();
            notify(json!({ "presence": now }), cx);
        }
    });
    let (sender, mut released) = futures::channel::mpsc::unbounded::<()>();
    let weak = community.downgrade();
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        released.next().await;
        let _ = weak.update(cx, |community, _| {
            community.set_presence_plugin(id, Vec::new())
        });
    })
    .detach();
    Ok((
        Watch::new((subscription, Held(sender)), Task::ready(())),
        json!({}),
    ))
}

/// The signed-in identity.
fn identity(cx: &App) -> Result<Arc<buzz_identity::Identity>, Failure> {
    match &cx.global::<AppState>().identity {
        crate::community::IdentityState::Ready(identity) => Ok(identity.clone()),
        _ => Err((1, "You are not signed in.".to_owned())),
    }
}

/// `projects/gitAuthorization`: a NIP-98 token, after `Authorization:
/// Nostr `, for one of the community's repositories, good on its Git
/// routes for 60 seconds (`authorizeGit`).
pub fn git_authorization(params: &Value, cx: &App) -> Result<Value, Failure> {
    let repository = text(params, "repository").ok_or_else(|| invalid("repository is required"))?;
    let token = community(cx)?
        .read(cx)
        .session
        .projects()
        .git_authorization(repository)
        .map_err(invalid)?;
    Ok(json!({ "repository": repository, "token": token }))
}

/// `agents/attest`: your NIP-OA proof that you own `pubkey`, the tag
/// `["auth", owner, "", signature]` an agent attaches to what it signs
/// (`prepareRemoteAgentAuthorization`).
pub fn attest(params: &Value, cx: &App) -> Result<Value, Failure> {
    let pubkey = text(params, "pubkey")
        .ok_or_else(|| invalid("pubkey is required"))?
        .to_ascii_lowercase();
    let auth = identity(cx)?
        .attest_agent(&pubkey)
        .map_err(|_| invalid("pubkey is not an agent key you can attest"))?;
    let auth: Value = serde_json::from_str(&auth).map_err(|error| (3, error.to_string()))?;
    Ok(json!({ "auth": auth }))
}

/// `format/time`: a time as Buzz words it (`conversation.format`):
/// `byline` ("Yesterday at 2:34 PM"), `day` (a day divider, "Monday"),
/// `full` (a byline's hover text) or `relative` ("5 minutes ago").
pub fn format_time(params: &Value) -> Result<Value, Failure> {
    use crate::ui::time;
    let at = params
        .get("at")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("at (unix seconds) is required"))?;
    // `nowSeconds`: what "today" and "ago" are counted from.
    let now = params
        .get("now")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs())
        });
    Ok(json!(match text(params, "style") {
        None | Some("byline") => time::byline_at(at, time::at(now)),
        Some("item") => time::item_at(at, time::at(now)),
        Some("day") => time::day_at(at, time::at(now)),
        Some("full") => time::full(at),
        Some("relative") => crate::ui::link_preview::relative(at, now),
        Some(_) => return Err(invalid("style must be byline, item, day, full or relative")),
    }))
}

/// `agents/presence`: publishes owner presence (`online` or `offline`)
/// for up to 32 of your agents, such as hosted ones Buzz does not run
/// (`publishAgentPresence`). Answers whether it went out.
pub fn agent_presence(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let status = match text(params, "status") {
        Some(status @ ("online" | "offline")) => status.to_owned(),
        _ => return Err(invalid("status must be online or offline")),
    };
    let agents = pubkeys(params, 32)?;
    let owned = crate::agents2::Agents2::global(cx).is_some_and(|service| {
        let service = service.read(cx);
        agents
            .iter()
            .all(|key| service.agents().iter().any(|agent| agent.pubkey == *key))
    });
    if !owned {
        return Err(invalid("pubkeys must all be your agents"));
    }
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .publish_agent_presence(&status, agents)
            .await
            .map(|sent| json!(sent))
            .map_err(|error| (3, error))
    }))
}

/// `agents/logProof`: the owner's signature for one read of an agent's
/// harness log at the community's relay (`authorizeAgentLog`).
pub fn log_proof(params: &Value, cx: &App) -> Result<Value, Failure> {
    let field = |key: &str| text(params, key).ok_or_else(|| invalid(format!("{key} is required")));
    let community = community(cx)?;
    let origin = community.read(cx).origin.http();
    let signature = identity(cx)?
        .harness_log_proof(
            &origin,
            field("id")?,
            field("pubkey")?,
            field("relayUrl")?,
            field("nonce")?,
        )
        .map_err(|_| (3, "Log authorization unavailable".to_owned()))?;
    Ok(json!({ "signature": signature }))
}

/// The most `media/get` hands over, before base64.
const MAX_MEDIA: usize = 8 * 1024 * 1024;

/// `media/get`: a file in the community's media, which only the user may
/// read, as base64 (`session.media`).
pub fn media(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    use base64::Engine;
    let url = text(params, "url")
        .ok_or_else(|| invalid("url is required"))?
        .to_owned();
    // A draft not sent yet, held in memory under the key
    // `attachment/render` gave.
    if let Some(bytes) = crate::ui::media::local_bytes(&url, cx) {
        return Ok(Task::ready(local_media(&url, bytes, params)));
    }
    let media = community(cx)?.read(cx).session.media().clone();
    if !media.is_relay_media(&url) {
        return Err(invalid(
            "url is not the community's media; fetch it yourself",
        ));
    }
    // Into a folder, any size, under the sender's file name.
    if let Some(directory) = text(params, "directory") {
        let directory = std::path::PathBuf::from(directory);
        if !directory.is_dir() {
            return Err(invalid("directory is not a folder"));
        }
        let name = text(params, "name").map(str::to_owned);
        return Ok(on_tokio(cx, async move {
            media
                .download(&url, name.as_deref(), directory)
                .await
                .map(|path| json!({ "path": path }))
                .map_err(|error| (3, error))
        }));
    }
    Ok(on_tokio(cx, async move {
        let bytes = media
            .fetch(&url)
            .await
            .map_err(|error| (3, error.to_string()))?;
        if bytes.len() > MAX_MEDIA {
            return Err((3, "The file is larger than 8 MiB.".to_owned()));
        }
        Ok(json!({
            "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
            "size": bytes.len(),
            "mime": buzz_relay_client::upload::mime_type(&url, &bytes),
        }))
    }))
}

/// `media/get` for a draft's bytes in memory.
fn local_media(url: &str, bytes: Arc<[u8]>, params: &Value) -> Result<Value, Failure> {
    use base64::Engine;
    if let Some(directory) = text(params, "directory") {
        let directory = std::path::PathBuf::from(directory);
        if !directory.is_dir() {
            return Err(invalid("directory is not a folder"));
        }
        let name = text(params, "name").unwrap_or("draft");
        let name = std::path::Path::new(name)
            .file_name()
            .ok_or_else(|| invalid("name is not a file name"))?;
        let path = directory.join(name);
        std::fs::write(&path, &bytes).map_err(|error| (3, error.to_string()))?;
        return Ok(json!({ "path": path }));
    }
    if bytes.len() > MAX_MEDIA {
        return Err((3, "The file is larger than 8 MiB.".to_owned()));
    }
    Ok(json!({
        "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
        "size": bytes.len(),
        "mime": buzz_relay_client::upload::mime_type(url, &bytes),
    }))
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

/// `emoji/catalog`: the community's custom emoji with how far the read
/// got, and your own set (`emoji.status`, `entries`, `mine`).
pub fn emoji_catalog(cx: &App) -> Result<Value, Failure> {
    use buzz_relay_client::emoji::CatalogStatus;
    let community = community(cx)?;
    let emoji = community.read(cx).snapshot.emoji.clone();
    let (status, error) = match &emoji.status {
        CatalogStatus::Idle => ("idle", None),
        CatalogStatus::Loading => ("loading", None),
        CatalogStatus::Ready => ("ready", None),
        CatalogStatus::Error(error) => ("error", Some(error.clone())),
    };
    let entry = |emoji: &buzz_relay_client::CustomEmoji| json!({ "shortcode": emoji.shortcode, "url": emoji.url });
    Ok(json!({
        "status": status,
        "error": error,
        "entries": emoji.entries.iter().map(entry).collect::<Vec<_>>(),
        "mine": emoji.mine.iter().map(entry).collect::<Vec<_>>(),
    }))
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
        .unwrap_or_default()
        .to_owned();
    let page = params
        .get("page")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        // No query browses people, as the New message picker does
        // (`directMessages.people`).
        let found = if query.is_empty() {
            session.people(&query, page).await
        } else {
            session.search_members(&query, page).await
        }
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

/// `ui/navigate`: shows a page, Settings or a conversation, as a new
/// visit or, with `replace`, in place of the current one.
/// `ui/navigate`: answers once the visit settles, as buzz-app's
/// `open()` resolves with an `OpenResult`: `opened`, `failed` with its
/// `reason`, or `superseded` by another visit. Back and Forward answer
/// at once.
pub fn navigate(params: &Value, cx: &mut App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let before = navigation::workspace(cx).map(|workspace| workspace.read(cx).visit().0);
    let tracked = match start_navigation(params, cx) {
        Ok(tracked) => tracked,
        // A target that cannot be opened fails the open, as buzz-app's
        // `open()` resolves `invalid-target`, rather than the request.
        Err((INVALID_PARAMS, error)) => {
            return Ok(Task::ready(Ok(json!({
                "status": "failed",
                "reason": "invalid-target",
                "error": error,
            }))));
        }
        Err(failure) => return Err(failure),
    };
    let Some(before) = before.filter(|_| tracked) else {
        return Ok(Task::ready(Ok(Value::Null)));
    };
    Ok(settle(before, cx))
}

/// How the visit after attempt `before` turns out (`OpenResult`).
fn settle(before: u64, cx: &mut App) -> Task<Result<Value, Failure>> {
    cx.spawn(async move |cx| {
        let mut ours = None;
        // As long as a visit may take to open (`OPEN_TIMEOUT`), and a bit.
        for step in 0..170 {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let state = cx.update(|cx| {
                navigation::workspace(cx).map(|workspace| {
                    let workspace = workspace.read(cx);
                    (
                        workspace.visit(),
                        workspace.failed().map(|(failed, _)| failed.reason),
                    )
                })
            });
            let Some(((attempt, opening), failure)) = state else {
                break;
            };
            if attempt == before {
                // Nothing new opened: the window already shows it.
                if step >= 10 {
                    return Ok(json!({ "status": "opened" }));
                }
                continue;
            }
            if *ours.get_or_insert(attempt) != attempt {
                return Ok(json!({ "status": "superseded" }));
            }
            if let Some(reason) = failure {
                return Ok(json!({ "status": "failed", "reason": reason_name(reason) }));
            }
            if !opening {
                return Ok(json!({ "status": "opened" }));
            }
        }
        Ok(json!({ "status": "failed", "reason": "timeout" }))
    })
}

/// Starts a navigation; `false` for Back and Forward, which are not
/// followed.
fn start_navigation(params: &Value, cx: &mut App) -> Result<bool, Failure> {
    match text(params, "to") {
        Some("back") => {
            cx.dispatch_action(&crate::ui::navigation::GoBack);
            return Ok(false);
        }
        Some("forward") => {
            cx.dispatch_action(&crate::ui::navigation::GoForward);
            return Ok(false);
        }
        _ => {}
    }
    // `community: null` leaves for Personal space first (`scope: null`).
    if params.get("community") == Some(&Value::Null) {
        let place = place_of(params, cx)?;
        if matches!(place, Place::Conversation { .. }) {
            return Err(invalid("a conversation needs its community"));
        }
        navigation::with_workspace(cx, move |workspace, window, cx| {
            workspace.select_personal(window, cx);
            workspace.navigate(
                Target {
                    community: None,
                    place,
                },
                window,
                cx,
            );
        });
        return Ok(true);
    }
    let target = Target {
        community: scope(params, cx)?,
        place: place_of(params, cx)?,
    };
    let replace = params.get("replace").and_then(Value::as_bool) == Some(true);
    if text(params, "focus") == Some("preserve") {
        navigation::with_workspace(cx, move |workspace, window, cx| {
            workspace.navigate_keeping_focus(target, replace, window, cx)
        });
        return Ok(true);
    }
    if replace {
        navigation::with_workspace(cx, move |workspace, window, cx| {
            workspace.navigate_replacing(target, window, cx)
        });
        return Ok(true);
    }
    let sender = navigation::sender(cx).ok_or((1, "The window is not open.".to_owned()))?;
    let _ = sender.unbounded_send(Request::Open(target).into());
    Ok(true)
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

/// `contributions/list`: what Buzz and its plugins added that a plugin
/// can open or name: pages, panels, Settings sections and account
/// actions, each by its `plugin/id` key.
pub fn contributions(cx: &App) -> Value {
    use crate::ui::pages::Placement;
    use crate::ui::settings::cards::CardVisibility;
    let mut lists = json!({
        "inlineRenderers": [],
        "messageRenderers": [],
        "linkRenderers": [],
        "attachmentRenderers": [],
        "messageActions": [],
        "pages": crate::ui::pages::all(cx).iter().map(|spec| json!({
            "key": spec.page.key(),
            "title": spec.label,
            "placement": match spec.placement {
                Placement::Topbar => "topbar",
                Placement::Sidebar => "sidebar",
                Placement::Hidden => "hidden",
            },
            "routes": crate::ui::pages::routes(spec.page, cx).map(|routes| routes.version),
        })).collect::<Vec<_>>(),
        "panels": crate::ui::channel_panels::all(cx).iter().map(|panel| json!({
            "key": panel.key,
            "title": panel.title,
            "order": panel.order,
            "launcher": panel.launcher.is_some(),
        })).collect::<Vec<_>>(),
        "settings": crate::ui::settings::cards::all(cx).iter().map(|card| json!({
            "key": card.key,
            "title": card.title,
            "visibility": match card.visibility(cx) {
                CardVisibility::Visible => "visible",
                CardVisibility::Hidden => "hidden",
                CardVisibility::Pending => "pending",
                CardVisibility::Error => "error",
            },
        })).collect::<Vec<_>>(),
        "accountActions": crate::ui::account_actions::all(cx).iter().map(|action| json!({
            "key": action.key,
            "title": action.title.to_string(),
        })).collect::<Vec<_>>(),
        "shortcuts": crate::shortcuts::external_list(cx).iter().map(|shortcut| json!({
            "key": shortcut.key,
            "plugin": shortcut.plugin,
            "title": shortcut.title,
            "chords": shortcut.chords,
            "enabled": shortcut.enabled,
        })).collect::<Vec<_>>(),
        "composerTools": crate::ui::composer_tools::listed(cx).iter()
            .filter(|(_, _, accessory)| !accessory)
            .map(|(key, order, _)| json!({
                "key": key,
                "order": order,
                "title": title_of(key),
            }))
            .collect::<Vec<_>>(),
        "completions": crate::ui::composer_completions::listed().iter()
            .map(|(key, trigger, order)| json!({
                "key": key,
                "trigger": trigger.to_string(),
                "order": order,
                "title": title_of(key),
            }))
            .collect::<Vec<_>>(),
    });
    let mut accessories: Vec<Value> = crate::ui::composer_tools::listed(cx)
        .iter()
        .filter(|(_, _, accessory)| *accessory)
        .map(|(key, order, _)| {
            json!({ "key": key, "order": order, "placement": "composer", "title": title_of(key) })
        })
        .collect();
    for (kind, key, title) in crate::ui::renderers::listed() {
        let (named, order) = super::contribute::titled(&key).unzip();
        if kind == "messageAccessories" {
            accessories.push(json!({
                "key": key,
                "placement": "message",
                "title": named,
                "order": order.flatten(),
            }));
            continue;
        }
        let entry = json!({
            "key": key,
            "title": title.or(named).unwrap_or_else(|| key.clone()),
            "order": order.flatten(),
        });
        if let Some(list) = lists[kind].as_array_mut() {
            list.push(entry);
        } else {
            lists[kind] = json!([entry]);
        }
    }
    lists["accessories"] = Value::Array(accessories);
    lists
}

/// A contribution's title for `contributions/list`: the plugin's, or
/// the key for Buzz's own.
fn title_of(key: &str) -> String {
    super::contribute::titled(key).map_or_else(|| key.to_owned(), |(title, _)| title)
}

/// `community/info`: the community relay's NIP-11 document, whose `self`
/// key signs what the relay publishes, such as archives and rosters.
pub fn info(cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let identity = match &cx.global::<AppState>().identity {
        crate::community::IdentityState::Ready(identity) => identity.clone(),
        _ => return Err((1, "You are not signed in.".to_owned())),
    };
    let http = buzz_relay_client::RelayHttp::new(community.read(cx).origin.clone(), identity);
    Ok(on_tokio(cx, async move {
        http.information()
            .await
            .map_err(|error| (3, error.to_string()))
    }))
}

/// `ui/openPanel`: opens one of the plugin's panels (by `panel` id) as a
/// tab of the open conversation, showing `target`.
pub fn open_panel(plugin: &str, params: &Value, cx: &mut App) -> Result<Value, Failure> {
    let target = text(params, "target")
        .filter(|target| target.len() <= 2048)
        .ok_or_else(|| invalid("target is required"))?
        .to_owned();
    // One of the plugin's panels, or without `panel` whichever panel shows
    // `target`, any plugin's (`ComposerAccessoryProps.open`).
    let key = match text(params, "panel") {
        Some(panel) => {
            let key = format!("{plugin}/{panel}");
            if crate::ui::channel_panels::get(&key, cx).is_none() {
                return Err(invalid(format!("The plugin has no panel {panel}")));
            }
            Some(key)
        }
        None => match crate::ui::channel_panels::resolve(&target, cx) {
            Some(_) => None,
            None => return Ok(json!(false)),
        },
    };
    // The conversation and thread it opens for, such as an accessory's.
    let channel = text(params, "channel").map(str::to_owned);
    let root = text(params, "thread").map(str::to_owned);
    navigation::with_workspace(cx, move |workspace, window, cx| {
        workspace.open_panel_at(key.as_deref(), &target, channel, root, window, cx);
    });
    Ok(json!(true))
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
    // Any place `ui/navigate` takes, or the short forms below.
    let place = match (text(params, "to"), text(params, "channel"), &community) {
        (Some(_), _, _) => place_of(params, cx)?,
        (None, Some(channel), Some(community)) => {
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
    /// Loads more of what it follows (`watch/more`), if it can.
    more: Option<Box<dyn Fn()>>,
    /// Shows another message of the thread (`watch/retarget`).
    retarget: Option<Box<dyn Fn(EventId)>>,
}

impl Watch {
    pub fn new(keep: impl std::any::Any, forward: Task<()>) -> Self {
        Self {
            _keep: Box::new(keep),
            _forward: forward,
            more: None,
            retarget: None,
        }
    }

    fn with_retarget(mut self, retarget: impl Fn(EventId) + 'static) -> Self {
        self.retarget = Some(Box::new(retarget));
        self
    }

    /// Moves a thread watch to another message (`ThreadView.retarget`);
    /// `false` for a watch that is not a thread's.
    pub fn retarget(&self, message: EventId) -> bool {
        self.retarget
            .as_ref()
            .map(|retarget| retarget(message))
            .is_some()
    }

    fn with_more(mut self, more: impl Fn() + 'static) -> Self {
        self.more = Some(Box::new(more));
        self
    }

    /// Asks for more history; `false` when this watch has none to load.
    pub fn more(&self) -> bool {
        self.more.as_ref().map(|more| more()).is_some()
    }
}

/// The most messages a channel watch grows to with `watch/more`.
const MAX_WATCHED: usize = 1_000;

/// `messages/sendToChannel`: copies one of your thread replies into the
/// channel as a new message that names the thread ("Also send to
/// channel", `messages.sendToChannel`).
pub fn send_to_channel(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let message = event_id(params, "message")?.ok_or_else(|| invalid("message is required"))?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let view = session.open_thread(&channel, message, true);
        let mut snapshot = view.snapshot.clone();
        let thread = tokio::time::timeout(
            READ_TIMEOUT,
            snapshot.wait_for(|thread| {
                thread.target.is_some()
                    || matches!(
                        thread.target_status,
                        Some(
                            buzz_relay_client::threads::TargetStatus::Unavailable
                                | buzz_relay_client::threads::TargetStatus::Error
                        )
                    )
            }),
        )
        .await
        .map_err(|_| (2, "The relay did not answer in time.".to_owned()))?
        .map_err(|_| (2, "The thread closed.".to_owned()))?
        .clone();
        drop(view);
        let row = thread
            .target
            .clone()
            .ok_or_else(|| (3, "That reply is not available.".to_owned()))?;
        session
            .send_to_channel(row, thread.root.clone())
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// The message a thread read opens at (`root`, or any `message` in the
/// thread), and whether it is `exact`: folded around that message on
/// its own evidence (`thread({exact})`).
fn thread_target(params: &Value) -> Result<(EventId, bool), Failure> {
    let target = event_id(params, "message")?
        .or(event_id(params, "root")?)
        .ok_or_else(|| invalid("root is required"))?;
    Ok((
        target,
        params.get("exact").and_then(Value::as_bool) == Some(true),
    ))
}

/// A thread as `thread/messages` and `thread/changed` give it.
fn thread_json(
    channel: &str,
    thread: &buzz_relay_client::threads::ThreadSnapshot,
    profiles: &buzz_relay_client::Profiles,
) -> Value {
    use buzz_relay_client::threads::TargetStatus;
    json!({
        "channel": channel,
        "root": thread.root.as_ref().map(|row| message(row, profiles)),
        "replies": thread
            .replies
            .iter()
            .map(|row| message(row, profiles))
            .collect::<Vec<_>>(),
        "hasMore": thread.can_load_more,
        "limited": thread.limited,
        "status": format!("{:?}", thread.status).to_ascii_lowercase(),
        "direction": thread.direction.map(|direction| format!("{direction:?}").to_ascii_lowercase()),
        "readKind": thread.read_kind.map(|kind| format!("{kind:?}").to_ascii_lowercase()),
        "target": thread.target.as_ref().map(|row| message(row, profiles)),
        "targetStatus": thread.target_status.map(|status| match status {
            TargetStatus::Loading => "loading",
            TargetStatus::Ready => "ready",
            TargetStatus::Unavailable => "unavailable",
            TargetStatus::Error => "error",
        }),
        "error": thread.error,
    })
}

/// `thread/watch`: sends `thread/changed` with the root and replies now
/// and whenever the thread changes.
pub fn watch_thread(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let community = community(cx)?;
    let channel = readable(params, community.read(cx))?;
    let (root, exact) = thread_target(params)?;
    let view = std::rc::Rc::new(
        community
            .read(cx)
            .session
            .open_thread(&channel, root, exact),
    );
    let more = {
        let view = view.clone();
        move || view.load_more()
    };
    let retarget = {
        let view = view.clone();
        // The outcome shows in the next `thread/changed` (`targetStatus`).
        move |message| drop(view.retarget(message))
    };
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
                notify(thread_json(&thread_channel, &thread, &profiles), cx);
            });
        }
    });
    Ok((
        Watch::new(view, forward)
            .with_more(more)
            .with_retarget(retarget),
        // As asked: `message` is any message of the thread, not its root.
        json!({
            "channel": channel,
            "root": params.get("root").filter(|_| params.get("message").is_none()),
            "message": params.get("message"),
        }),
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
    let channel = readable(params, community.read(cx))?;
    let membership = params.get("membership").and_then(Value::as_bool) == Some(true);
    let limit = std::rc::Rc::new(std::cell::Cell::new(limit(params)));
    let view = std::rc::Rc::new(community.read(cx).session.open_channel(&channel));
    let mut receiver = view.snapshot.clone();
    let (sender, mut values) = futures::channel::mpsc::unbounded::<Arc<TimelineSnapshot>>();
    let _ = sender.unbounded_send(receiver.borrow_and_update().clone());
    // `watch/more`: older history, and room in the answer to show it.
    let more = {
        let (view, limit, sender) = (view.clone(), limit.clone(), sender.clone());
        move || {
            limit.set((limit.get() + DEFAULT_MESSAGES).min(MAX_WATCHED));
            view.load_older();
            let _ = sender.unbounded_send(view.snapshot.borrow().clone());
        }
    };
    let shown = limit.clone();
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
                notify(timeline(&snapshot, shown.get(), membership, &profiles), cx);
            });
        }
    });
    Ok((
        Watch::new(view, forward).with_more(more),
        json!({ "channel": channel }),
    ))
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
