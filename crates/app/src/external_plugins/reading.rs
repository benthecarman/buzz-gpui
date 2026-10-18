//! What is unread, and marking it read (buzz-app's `UnreadCapability`):
//! the selected community's unread counts, followed live, each channel's
//! unread threads, the inbox, and read marks as the user.

use super::capabilities::{Failure, Watch, channel, community, on_tokio};
use super::protocol::INVALID_PARAMS;
use buzz_relay_client::unread::ThreadActivitySnapshot;
use buzz_relay_client::{EventId, UnreadCounts};
use gpui_kit::*;
use serde_json::{Value, json};

fn invalid(message: impl Into<String>) -> Failure {
    (INVALID_PARAMS, message.into())
}

/// Unread counts as plugins see them.
fn counts(unread: &UnreadCounts) -> Value {
    let channels: serde_json::Map<String, Value> = unread
        .channels
        .iter()
        .map(|(id, channel)| {
            (
                id.clone(),
                json!({
                    "unread": channel.unread,
                    "count": channel.count,
                    "attention": channel.attention,
                    "mentions": channel.mentions,
                    "threads": channel.threads,
                    "manual": channel.manual != Default::default(),
                    "stale": channel.stale,
                }),
            )
        })
        .collect();
    json!({
        "channels": channels,
        "mentions": unread.mentions,
        "direct": unread.direct,
        "observed": unread.observed,
    })
}

/// `unread/get`: what is unread in the selected community. Channels with
/// nothing unread are left out.
pub fn get(cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    Ok(counts(&community.read(cx).snapshot.unread))
}

/// `unread/watch`: sends `unread/changed` with the counts now and whenever
/// they change.
pub fn watch(
    _params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let community = community(cx)?;
    let first = counts(&community.read(cx).snapshot.unread);
    // What was last sent: the counts carry more than plugins see, so
    // compare what they would get.
    let last = std::cell::RefCell::new(first.clone());
    let notify = std::rc::Rc::new(notify);
    let send = notify.clone();
    cx.defer(move |cx| send(first, cx));
    let subscription = cx.observe(&community, move |community, cx| {
        let unread = counts(&community.read(cx).snapshot.unread);
        if unread == *last.borrow() {
            return;
        }
        *last.borrow_mut() = unread.clone();
        notify(unread, cx);
    });
    Ok((Watch::new(subscription, Task::ready(())), json!({})))
}

fn activity(snapshot: &ThreadActivitySnapshot) -> Value {
    json!({
        "channel": snapshot.channel_id,
        "items": snapshot.items.as_ref().map(|items| {
            items
                .iter()
                .map(|item| json!({
                    "root": item.root_id,
                    "latestMessage": item.latest_message_id,
                    "author": item.author_id,
                    "createdAt": item.created_at,
                    "preview": item.preview,
                    "unreadCount": item.unread_count,
                }))
                .collect::<Vec<_>>()
        }),
        "error": snapshot.error,
    })
}

/// `threads/activity`: a channel's threads with unread replies for the
/// user, newest first.
pub fn threads(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        match session.thread_activity(&channel).await {
            Some(snapshot) => Ok(activity(&snapshot)),
            None => Ok(json!({ "channel": channel, "items": null, "error": null })),
        }
    }))
}

/// `unread/sync`: how read state is syncing to the relay, and
/// `unread/retrySync` to try again after a failure.
pub fn sync(retry: bool, cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let community = community.read(cx);
    if retry {
        community.session.retry_read_sync();
        return Ok(Value::Null);
    }
    Ok(match &community.snapshot.unread.sync {
        None => Value::Null,
        Some(sync) => json!({
            "capability": format!("{:?}", sync.capability).to_ascii_lowercase(),
            "status": format!("{:?}", sync.status).to_ascii_lowercase(),
            "complete": format!("{:?}", sync.completeness).to_ascii_lowercase(),
            "error": sync.error,
        }),
    })
}

/// `unread/message`: whether a loaded message shows as unread now.
pub fn message(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let id = params
        .get("message")
        .and_then(Value::as_str)
        .and_then(|hex| EventId::from_hex(hex).ok())
        .ok_or_else(|| invalid("message is not an event id"))?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        Ok(json!(session.message_unread(&channel, id).await))
    }))
}

/// `unread/attention`: a loaded message's attention as the badges see
/// it (`unread.attention`), or `null` before read state loads.
pub fn attention(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    use buzz_relay_client::unread::{AttentionStatus, Category};
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let id = params
        .get("message")
        .and_then(Value::as_str)
        .and_then(|hex| EventId::from_hex(hex).ok())
        .ok_or_else(|| invalid("message is not an event id"))?;
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let Some(attention) = session.message_attention(&channel, id).await else {
            return Ok(Value::Null);
        };
        Ok(json!({
            "status": match attention.status {
                AttentionStatus::Unknown => "unknown",
                AttentionStatus::Ineligible => "ineligible",
                AttentionStatus::Eligible => "eligible",
            },
            "category": attention.category.map(|category| match category {
                Category::Mention => "mention",
                Category::Direct => "direct",
                Category::Thread => "thread",
            }),
            "mentioned": attention.mentioned,
            "root": attention.root_id,
            "pending": attention.pending,
            "unread": attention.unread,
            "forced": attention.forced,
            "viewing": attention.viewing,
        }))
    }))
}

/// `read/markLocal` and `read/clearLocal`: marks a channel, a thread
/// (`thread`, its root) or a message unread on this device only, or
/// drops that mark (`markUnreadLocal`, `clearUnreadLocal`).
pub fn local(
    clear: bool,
    params: &Value,
    cx: &App,
) -> Result<Task<Result<Value, Failure>>, Failure> {
    use buzz_relay_client::read_state::ReadTarget;
    let community = community(cx)?;
    let channel_id = channel(params, community.read(cx))?;
    let id = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(|hex| {
                EventId::from_hex(hex)
                    .map(|id| id.to_hex())
                    .map_err(|_| invalid(format!("{key} is not an event id")))
            })
            .transpose()
    };
    let target = match (id("thread")?, id("message")?) {
        (Some(root_id), _) => ReadTarget::Thread {
            channel_id,
            root_id,
        },
        (None, Some(message_id)) => ReadTarget::Message {
            channel_id,
            message_id,
        },
        (None, None) => ReadTarget::Channel { channel_id },
    };
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .mark_local(target, clear)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `inbox/get`: the inbox's conversations, as the Inbox page lists them.
pub fn inbox(cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let state = community.read(cx).session.inbox().borrow().clone();
    Ok(inbox_json(&state))
}

fn inbox_json(state: &buzz_relay_client::session::InboxState) -> Value {
    json!({
        "items": state.inbox.items.iter().map(|item| json!({
            "channel": item.channel_id,
            "message": item.message_id,
            "latestMessage": item.latest_message_id,
            "root": item.root_id,
            "author": item.author_id,
            "preview": item.preview,
            "createdAt": item.created_at,
            "mentioned": item.mentioned,
            "thread": item.thread,
            "unreadCount": item.unread_count,
            // Marked unread on this device (`read/markLocal`).
            "manual": item.manual,
        })).collect::<Vec<_>>(),
        "error": state.inbox.error,
    })
}

/// `inbox/watch`: sends `inbox/changed` with what `inbox/get` gives,
/// now and whenever the Inbox changes.
pub fn watch_inbox(
    _params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let mut receiver = community(cx)?.read(cx).session.inbox();
    let (sender, mut changes) = futures::channel::mpsc::unbounded::<Value>();
    crate::runtime::Runtime::handle(cx).spawn(async move {
        let mut last = Value::Null;
        loop {
            let now = inbox_json(&receiver.borrow_and_update());
            if now != last {
                if sender.unbounded_send(now.clone()).is_err() {
                    return;
                }
                last = now;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    });
    let forward = cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some(inbox) = changes.next().await {
            cx.update(|cx| notify(inbox, cx));
        }
    });
    Ok((Watch::new((), forward), json!({})))
}

/// `read/mark`: marks everything (`all`), a channel, a message, or a
/// channel or thread up to a message (`through`) read, or a channel or
/// message unread (`read: false`).
pub fn mark(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let session = community.read(cx).session.clone();
    let id = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .map(|hex| {
                EventId::from_hex(hex).map_err(|_| invalid(format!("{key} is not an event id")))
            })
            .transpose()
    };
    let read = params.get("read").and_then(Value::as_bool).unwrap_or(true);
    if params.get("all").and_then(Value::as_bool) == Some(true) {
        if !read {
            return Err(invalid("all marks read only"));
        }
        return Ok(on_tokio(cx, async move {
            session
                .mark_all_read()
                .await
                .map(|()| Value::Null)
                .map_err(|error| (3, error))
        }));
    }
    let channel = channel(params, community.read(cx))?;
    let message = id("message")?;
    let root = id("root")?;
    let through = params
        .get("through")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(on_tokio(cx, async move {
        let done = match (message, read, through) {
            (None, true, _) => session.mark_channel_read(&channel).await,
            (None, false, _) => session.mark_channel_unread(&channel).await,
            (Some(message), true, true) => session.mark_through(&channel, root, message).await,
            (Some(message), true, false) => session.mark_message_read(&channel, message).await,
            (Some(message), false, _) => session.mark_message_unread(&channel, message).await,
        };
        done.map(|()| Value::Null).map_err(|error| (3, error))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn counts_list_unread_channels_and_totals() {
        let mut unread = UnreadCounts {
            mentions: 2,
            observed: true,
            ..UnreadCounts::default()
        };
        unread.channels.insert(
            "c1".into(),
            buzz_relay_client::ChannelUnread {
                unread: true,
                count: 3,
                mentions: 2,
                ..Default::default()
            },
        );
        let value = counts(&unread);
        assert_eq!(value["mentions"], 2);
        assert_eq!(value["channels"]["c1"]["count"], 3);
        assert_eq!(value["channels"]["c1"]["manual"], false);
    }
}
