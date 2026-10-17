//! Raw Nostr events for plugins (buzz-app's `session.read`, `observe` and
//! `outbox.send`): reading the selected community as the user, following
//! live events that match filters, and publishing events of the kinds the
//! app itself writes. Publishing needs the `publish-events` permission.

use super::capabilities::{Failure, Watch, community, on_tokio};
use super::protocol::INVALID_PARAMS;
use crate::community::{AppState, IdentityState};
use crate::runtime::Runtime;
use buzz_identity::{Identity, Purpose};
use buzz_relay_client::RelayHttp;
use gpui_kit::*;
use nostr::prelude::Event;
use serde_json::{Map, Value, json};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

const MAX_FILTERS: usize = 10;
/// Most events one `events/query` answer carries.
const MAX_EVENTS: usize = 500;
const MAX_TAGS: usize = 2_000;
/// Event ids a watch remembers, so overlapping routes send each once.
const SEEN: usize = 4_096;

fn invalid(message: impl Into<String>) -> Failure {
    (INVALID_PARAMS, message.into())
}

fn identity(cx: &App) -> Result<Arc<Identity>, Failure> {
    match &cx.global::<AppState>().identity {
        IdentityState::Ready(identity) => Ok(identity.clone()),
        _ => Err((1, "You are not signed in.".to_owned())),
    }
}

/// The `filters` param: NIP-01 filters of `ids`, `authors`, `kinds`,
/// `#<letter>` tags, `since`, `until` and `limit`.
fn filters(params: &Value) -> Result<Vec<Map<String, Value>>, Failure> {
    let list = params
        .get("filters")
        .and_then(Value::as_array)
        .filter(|list| (1..=MAX_FILTERS).contains(&list.len()))
        .ok_or_else(|| invalid(format!("filters must list 1 to {MAX_FILTERS} filters")))?;
    list.iter()
        .map(|filter| {
            let filter = filter
                .as_object()
                .ok_or_else(|| invalid("a filter must be an object"))?;
            for (key, value) in filter {
                let valid = match key.as_str() {
                    "ids" | "authors" => strings(value).is_some(),
                    "kinds" => value
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().all(|kind| kind.as_u64().is_some())),
                    "since" | "until" | "limit" => value.as_u64().is_some(),
                    tag => {
                        tag.len() == 2
                            && tag.starts_with('#')
                            && tag.as_bytes()[1].is_ascii_alphabetic()
                            && strings(value).is_some()
                    }
                };
                if !valid {
                    return Err(invalid(format!("a filter's {key} is not valid")));
                }
            }
            Ok(filter.clone())
        })
        .collect()
}

fn strings(value: &Value) -> Option<Vec<&str>> {
    value.as_array()?.iter().map(Value::as_str).collect()
}

/// Whether `event` matches `filter`, as a relay decides (NIP-01).
pub fn matches(filter: &Map<String, Value>, event: &Event) -> bool {
    let has =
        |value: &Value, wanted: &str| strings(value).is_some_and(|list| list.contains(&wanted));
    filter.iter().all(|(key, value)| match key.as_str() {
        "ids" => has(value, &event.id.to_hex()),
        "authors" => has(value, &event.pubkey.to_hex()),
        "kinds" => value.as_array().is_some_and(|kinds| {
            kinds
                .iter()
                .any(|kind| kind.as_u64() == Some(u64::from(event.kind.as_u16())))
        }),
        "since" => value
            .as_u64()
            .is_none_or(|since| event.created_at.as_secs() >= since),
        "until" => value
            .as_u64()
            .is_none_or(|until| event.created_at.as_secs() <= until),
        "limit" => true,
        tag => {
            let name = &tag[1..];
            event.tags.iter().any(|t| {
                let t = t.as_slice();
                t.first().is_some_and(|first| first == name)
                    && t.get(1).is_some_and(|tag_value| has(value, tag_value))
            })
        }
    })
}

/// `events/query`: events matching `filters`, read from the community as
/// the user.
pub fn query(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let filters = filters(params)?;
    let http = RelayHttp::new(community.read(cx).origin.clone(), identity(cx)?);
    Ok(on_tokio(cx, async move {
        let mut events = http
            .query_raw(&filters)
            .await
            .map_err(|error| (2, error.to_string()))?;
        events.truncate(MAX_EVENTS);
        Ok(Value::Array(events))
    }))
}

/// `events/publish`: signs `kind`, `content` and `tags` as the user and
/// posts the event to the community. Kinds the app does not write itself
/// are refused.
pub fn publish(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let community = community(cx)?;
    let identity = identity(cx)?;
    let kind = params
        .get("kind")
        .and_then(Value::as_u64)
        .and_then(|kind| u16::try_from(kind).ok())
        .ok_or_else(|| invalid("kind is required"))?;
    let content = params
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let tags: Vec<Vec<String>> = match params.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(tags) => tags
            .as_array()
            .filter(|tags| tags.len() <= MAX_TAGS)
            .and_then(|tags| {
                tags.iter()
                    .map(|tag| {
                        strings(tag)
                            .filter(|tag| !tag.is_empty())
                            .map(|tag| tag.into_iter().map(str::to_owned).collect())
                    })
                    .collect()
            })
            .ok_or_else(|| invalid("tags must be a list of lists of strings"))?,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let event = identity
        .template(Purpose::Content, kind, now, tags, content)
        .and_then(|unsigned| identity.sign(Purpose::Content, unsigned))
        .map_err(|error| (3, format!("Buzz does not publish this event: {error}")))?;
    let http = RelayHttp::new(community.read(cx).origin.clone(), identity);
    Ok(on_tokio(cx, async move {
        let body = serde_json::to_string(&event).map_err(|error| (3, error.to_string()))?;
        buzz_relay_client::outbox::accept_publish(http.post("/events", body).await, &event.id)
            .map_err(|error| {
                (
                    3,
                    match error {
                        buzz_relay_client::outbox::PublishError::Rejected(message)
                        | buzz_relay_client::outbox::PublishError::Uncertain(message) => message,
                    },
                )
            })?;
        Ok(json!({ "id": event.id.to_hex(), "event": event }))
    }))
}

/// `events/watch`: sends `events/live` with each live event that matches
/// `filters`, from now on.
pub fn watch(
    params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let community = community(cx)?;
    let filters = filters(params)?;
    let mut live = community.read(cx).session.live_events();
    let (sender, mut values) = futures::channel::mpsc::unbounded::<Vec<Value>>();
    Runtime::handle(cx).spawn(async move {
        let mut seen = (HashSet::new(), VecDeque::new());
        while let Some(batch) = live.recv().await {
            if sender.is_closed() {
                return;
            }
            let matched: Vec<Value> = batch
                .events
                .iter()
                .filter(|event| filters.iter().any(|filter| matches(filter, event)))
                .filter(|event| {
                    let fresh = seen.0.insert(event.id);
                    if fresh {
                        seen.1.push_back(event.id);
                        if seen.1.len() > SEEN
                            && let Some(old) = seen.1.pop_front()
                        {
                            seen.0.remove(&old);
                        }
                    }
                    fresh
                })
                .filter_map(|event| serde_json::to_value(&**event).ok())
                .collect();
            if !matched.is_empty() && sender.unbounded_send(matched).is_err() {
                return;
            }
        }
    });
    let forward = cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some(events) = values.next().await {
            cx.update(|cx| notify(json!({ "events": events }), cx));
        }
    });
    Ok((Watch::new((), forward), json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn event(kind: u16, tags: Vec<Vec<String>>) -> Event {
        let identity = Identity::ephemeral();
        let unsigned = identity
            .template(Purpose::Content, kind, 1_000, tags, "hi".into())
            .unwrap();
        identity.sign(Purpose::Content, unsigned).unwrap()
    }

    #[test]
    fn filters_match_like_a_relay() {
        let event = event(9, vec![vec!["h".into(), "room".into()]]);
        let filter = |value: Value| filters(&json!({ "filters": [value] })).unwrap().remove(0);
        assert!(matches(
            &filter(json!({ "kinds": [9], "#h": ["room"] })),
            &event
        ));
        assert!(!matches(&filter(json!({ "kinds": [7] })), &event));
        assert!(!matches(&filter(json!({ "#h": ["other"] })), &event));
        assert!(matches(
            &filter(json!({ "authors": [event.pubkey.to_hex()], "since": 1_000 })),
            &event
        ));
        assert!(!matches(&filter(json!({ "until": 999 })), &event));
        assert!(matches(
            &filter(json!({ "ids": [event.id.to_hex()] })),
            &event
        ));
    }

    #[test]
    fn filters_are_checked() {
        for bad in [
            json!({ "filters": [] }),
            json!({ "filters": [{ "search": "x" }] }),
            json!({ "filters": [{ "#hh": ["x"] }] }),
            json!({ "filters": [{ "kinds": ["9"] }] }),
            json!({ "filters": [7] }),
        ] {
            assert!(filters(&bad).is_err(), "{bad}");
        }
    }
}
