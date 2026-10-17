//! Channels, threads and reminders as the user: starting a DM, making,
//! joining and leaving channels (buzz-app's `directMessages`,
//! `channelCreation` and `channelLifecycle`), following threads
//! (`unread.follow`), and reminders (`reminders`). Changing channels needs
//! the `manage-channels` permission.

use super::capabilities::{Failure, Watch, channel, community, on_tokio};
use super::protocol::INVALID_PARAMS;
use buzz_relay_client::lifecycle::{Action, NewChannel};
use buzz_relay_client::{EventId, Reminder, ReminderStatus, ReminderTarget};
use gpui_kit::*;
use serde_json::{Value, json};

fn invalid(message: impl Into<String>) -> Failure {
    (INVALID_PARAMS, message.into())
}

fn text<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

fn event_id(params: &Value, key: &str) -> Result<EventId, Failure> {
    text(params, key)
        .and_then(|hex| EventId::from_hex(hex).ok())
        .ok_or_else(|| invalid(format!("{key} is not an event id")))
}

/// `dm/open`: the direct message with `pubkeys` (1 to 8 others), made if
/// it is new. Answers with its channel id.
pub fn open_dm(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let keys: Vec<String> = params
        .get("pubkeys")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("pubkeys must be a list"))?
        .iter()
        .filter_map(Value::as_str)
        .filter(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .collect();
    if !(1..=8).contains(&keys.len()) {
        return Err(invalid("pubkeys must name 1 to 8 people"));
    }
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let id = session.open_dm(keys).await.map_err(|error| (3, error))?;
        Ok(json!({ "channel": id }))
    }))
}

/// `channels/create`: a new stream channel. Answers with its id.
pub fn create(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let name = text(params, "name")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("name is required"))?
        .to_owned();
    let channel = NewChannel {
        name,
        description: text(params, "description").unwrap_or_default().to_owned(),
        private: params
            .get("private")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ttl: params.get("ttlSeconds").and_then(Value::as_u64),
        labels: params
            .get("labels")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .take(16)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    };
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let id = session
            .create_channel(channel)
            .await
            .map_err(|error| (3, error))?;
        Ok(json!({ "channel": id }))
    }))
}

/// `channels/action`: joins, leaves, archives, unarchives or deletes a
/// channel, or hides a DM, and answers once the relay shows it.
pub fn action(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let action = match text(params, "action") {
        Some("join") => Action::Join,
        Some("leave") => Action::Leave,
        Some("archive") => Action::Archive,
        Some("unarchive") => Action::Unarchive,
        Some("delete") => Action::Delete,
        Some("hide") => Action::Hide,
        _ => {
            return Err(invalid(
                "action must be join, leave, archive, unarchive, delete or hide",
            ));
        }
    };
    // A channel to join need not be one of yours yet.
    let id = text(params, "channel")
        .ok_or_else(|| invalid("channel is required"))?
        .to_owned();
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .channel_action(action, &id)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error.to_string()))
    }))
}

/// `channels/addMember`: adds someone, such as one of your agents, to a
/// channel, and answers once the roster lists them.
pub fn add_member(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let pubkey = text(params, "pubkey")
        .filter(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("pubkey is required"))?
        .to_ascii_lowercase();
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .add_member(&channel, &pubkey)
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `channels/changeMember`: makes a member an admin, member or guest, or
/// removes them; `expected` is the role they have now.
pub fn change_member(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    use buzz_relay_client::lifecycle::MemberRole;
    use buzz_relay_client::members::{Change, MemberChange};
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let pubkey = text(params, "pubkey")
        .ok_or_else(|| invalid("pubkey is required"))?
        .to_ascii_lowercase();
    let change = match text(params, "change") {
        Some("admin") => Change::Admin,
        Some("member") => Change::Member,
        Some("guest") => Change::Guest,
        Some("remove") => Change::Remove,
        _ => return Err(invalid("change must be admin, member, guest or remove")),
    };
    let expected = match text(params, "expected") {
        Some("owner") => MemberRole::Owner,
        Some("admin") => MemberRole::Admin,
        Some("member") => MemberRole::Member,
        Some("guest") => MemberRole::Guest,
        Some("bot") => MemberRole::Bot,
        _ => return Err(invalid("expected must be the member's role now")),
    };
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .change_member(
                &channel,
                MemberChange {
                    pubkey,
                    expected,
                    change,
                },
            )
            .await
            .map(|()| Value::Null)
            .map_err(|error| (3, error))
    }))
}

/// `threads/follow`: follows or unfollows a thread on this device, or
/// with no `following`, says whether you do.
pub fn follow(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let channel = channel(params, community.read(cx))?;
    let root = event_id(params, "root")?;
    let following = params.get("following").and_then(Value::as_bool);
    let session = community.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        match following {
            None => Ok(json!(session.following(&channel, root).await)),
            Some(following) => session
                .follow(&channel, root, following)
                .await
                .map(|()| json!(following))
                .map_err(|error| (3, error)),
        }
    }))
}

fn reminder(reminder: &Reminder) -> Value {
    json!({
        "id": reminder.id,
        "createdAt": reminder.created_at,
        "notBefore": reminder.not_before,
        "status": match reminder.status {
            ReminderStatus::Pending => "pending",
            ReminderStatus::Done => "done",
            ReminderStatus::Cancelled => "cancelled",
        },
        "note": reminder.note,
        "message": reminder.target.as_ref().map(|target| json!({
            "id": target.event_id,
            "channel": target.channel_id,
            "preview": target.preview,
            "author": target.author_pubkey,
        })),
    })
}

fn reminders_now(cx: &App) -> Result<Value, Failure> {
    let state = community(cx)?.read(cx).session.reminders().borrow().clone();
    Ok(json!({
        "reminders": state.reminders.iter().map(reminder).collect::<Vec<_>>(),
        "error": state.error,
    }))
}

/// `reminders/list`: your reminders.
pub fn list(cx: &App) -> Result<Value, Failure> {
    reminders_now(cx)
}

/// `reminders/watch`: sends `reminders/changed` with your reminders now
/// and whenever they change.
pub fn watch(
    _params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let community = community(cx)?;
    let mut receiver = community.read(cx).session.reminders();
    let (sender, mut changes) = futures::channel::mpsc::unbounded::<()>();
    crate::runtime::Runtime::handle(cx).spawn(async move {
        while receiver.changed().await.is_ok() {
            if sender.unbounded_send(()).is_err() {
                return;
            }
        }
    });
    let first = reminders_now(cx)?;
    let forward = cx.spawn(async move |cx| {
        use futures::StreamExt;
        cx.update(|cx| notify(first, cx));
        while changes.next().await.is_some() {
            cx.update(|cx| {
                if let Ok(now) = reminders_now(cx) {
                    notify(now, cx);
                }
            });
        }
    });
    Ok((Watch::new((), forward), json!({})))
}

/// `reminders/create`, `reminders/snooze`, `reminders/complete` and
/// `reminders/cancel`.
pub fn change(
    method: &str,
    params: &Value,
    cx: &App,
) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let session = community.read(cx).session.clone();
    let at = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid(format!("{key} (unix seconds) is required")))
    };
    let id = || {
        text(params, "id")
            .map(str::to_owned)
            .ok_or_else(|| invalid("id is required"))
    };
    let task = match method {
        "reminders/create" => {
            let channel = channel(params, community.read(cx))?;
            let message_id = event_id(params, "message")?;
            let not_before = at("notBefore")?;
            let note = text(params, "note").map(str::to_owned);
            let preview = text(params, "preview").unwrap_or_default().to_owned();
            let author = text(params, "author").unwrap_or_default().to_owned();
            let target = ReminderTarget {
                event_id: message_id.to_hex(),
                channel_id: channel,
                preview,
                author_pubkey: author,
            };
            on_tokio(cx, async move {
                session
                    .create_reminder(target, not_before, note)
                    .await
                    .map_err(|error| (3, error))
            })
        }
        "reminders/snooze" => {
            let (id, until) = (id()?, at("notBefore")?);
            on_tokio(cx, async move {
                session
                    .snooze_reminder(&id, until)
                    .await
                    .map_err(|error| (3, error))
            })
        }
        "reminders/complete" => {
            let id = id()?;
            on_tokio(cx, async move {
                session
                    .complete_reminder(&id)
                    .await
                    .map_err(|error| (3, error))
            })
        }
        _ => {
            let id = id()?;
            on_tokio(cx, async move {
                session
                    .cancel_reminder(&id)
                    .await
                    .map_err(|error| (3, error))
            })
        }
    };
    Ok(cx.background_spawn(async move { task.await.map(|()| Value::Null) }))
}

/// `archives/get`: the identities the community archived, which Buzz
/// leaves out of mentions, member choices and search, once read;
/// `refresh: true` reads them again.
pub fn archives(params: &Value, cx: &mut App) -> Result<Task<Result<Value, Failure>>, Failure> {
    use crate::ui::archives::{self, Status};
    let community = community(cx)?;
    let again = params.get("refresh").and_then(Value::as_bool) == Some(true);
    archives::ensure(&community, again, cx);
    Ok(cx.spawn(async move |cx| {
        // The read takes a request or two; wait up to 15 seconds.
        for _ in 0..150 {
            let status = cx.update(|cx| archives::status(&community, cx));
            if status != Some(Status::Loading) {
                break;
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
        }
        cx.update(|cx| match archives::status(&community, cx) {
            Some(Status::Ready) => Ok(json!({ "archived": archives::archived(&community, cx) })),
            Some(Status::Error) => Err((3, "The archive could not be read.".to_owned())),
            _ => Err((3, "The archive is still loading.".to_owned())),
        })
    }))
}

/// `sessions/create`: a new private work session titled `title`, under
/// `parent` if given, with `agents` (your agents in this community)
/// added. Answers with its channel id once the relay lists it.
pub fn create_session(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    use buzz_relay_client::session::Expected;
    let title = text(params, "title")
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or_else(|| invalid("title is required"))?;
    let title = buzz_relay_client::session::session_title(title);
    let parent = text(params, "parent").map(str::to_owned);
    let agents: Vec<String> = params
        .get("agents")
        .and_then(Value::as_array)
        .map(|agents| {
            agents
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let community = community(cx)?;
    let rules = crate::ui::me::recipients::rules(&community, cx);
    if let Some(stranger) = agents.iter().find(|key| !rules.selectable.contains(key)) {
        return Err(invalid(format!(
            "{stranger} is not one of your agents here"
        )));
    }
    let session = community.read(cx).session.clone();
    let id = uuid::Uuid::new_v4().to_string();
    Ok(on_tokio(cx, async move {
        let fail = |error: String| (3, error);
        let created = session
            .work_session_create(&id, &title, parent.as_deref())
            .await
            .map_err(fail)?;
        session
            .work_session_delivered(created)
            .await
            .map_err(fail)?;
        session
            .work_session_refresh(&id, Expected::default(), true)
            .await
            .map_err(fail)?;
        if !agents.is_empty() {
            session
                .add_agents(&id, &agents, &rules)
                .await
                .map_err(fail)?;
        }
        Ok(json!({ "channel": id }))
    }))
}

/// `sessions/memberOf`: the channels that list `pubkey` as a member.
pub fn member_of(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let pubkey = text(params, "pubkey")
        .filter(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("pubkey is required"))?
        .to_ascii_lowercase();
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        session
            .member_channels(&pubkey)
            .await
            .map(|channels| json!(channels))
            .map_err(|error| (3, error))
    }))
}

/// `sidebar/get`: how the user arranged the sidebar: starred and muted
/// channels, saved groups and where channels sit in them, and each
/// group's sort (`sidebarPreferences`).
pub fn sidebar(cx: &App) -> Result<Value, Failure> {
    let community = community(cx)?;
    let state = community.read(cx).snapshot.sidebar.clone();
    let Some(data) = &state.data else {
        return Ok(json!({ "loaded": false, "writable": false }));
    };
    Ok(json!({
        "loaded": true,
        "writable": state.writable,
        "starred": data.starred,
        "muted": data.muted,
        "sort": data
            .sort
            .iter()
            .map(|(group, mode)| (group.clone(), json!(mode.as_str())))
            .collect::<serde_json::Map<_, _>>(),
        "groups": data.sections.iter().map(|section| json!({
            "id": section.id,
            "name": section.name,
            "icon": section.icon,
        })).collect::<Vec<_>>(),
        "assignments": data.assignments,
    }))
}

/// `sidebar/star`, `sidebar/mute` and `sidebar/sort`: stars or unstars
/// a channel, mutes or unmutes it, or sorts a group (`starred`,
/// `channels`, `forums`, `dms` or `section:<id>`) by `alpha` or `recent`.
pub fn change_sidebar(
    method: &str,
    params: &Value,
    cx: &App,
) -> Result<Task<Result<Value, Failure>>, Failure> {
    use buzz_relay_client::sidebar::SortMode;
    use buzz_relay_client::sidebar_store::Destination;
    let session = community(cx)?.read(cx).session.clone();
    let on = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(|| invalid(format!("{key} must be true or false")))
    };
    let task = match method {
        "sidebar/sort" => {
            let group = text(params, "group")
                .ok_or_else(|| invalid("group is required"))?
                .to_owned();
            let mode = match text(params, "mode") {
                Some("alpha") => SortMode::Alpha,
                Some("recent") => SortMode::Recent,
                _ => return Err(invalid("mode must be alpha or recent")),
            };
            on_tokio(cx, async move {
                session
                    .sidebar_sort(&group, mode)
                    .await
                    .map_err(|error| (3, error))
            })
        }
        "sidebar/mute" => {
            let channel = channel(params, community(cx)?.read(cx))?;
            let muted = on("muted")?;
            on_tokio(cx, async move {
                session
                    .sidebar_mute(&channel, muted)
                    .await
                    .map_err(|error| (3, error))
            })
        }
        _ => {
            let channel = channel(params, community(cx)?.read(cx))?;
            let destination = if on("starred")? {
                Destination::Starred
            } else {
                Destination::Section(None)
            };
            on_tokio(cx, async move {
                session
                    .sidebar_move(&channel, destination)
                    .await
                    .map_err(|error| (3, error))
            })
        }
    };
    Ok(cx.background_spawn(async move { task.await.map(|()| Value::Null) }))
}
