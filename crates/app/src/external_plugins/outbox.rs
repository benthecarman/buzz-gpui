//! What the user's events are doing on the way to the relay (buzz-app's
//! `outbox`): every event queued for sending, such as messages, until the
//! relay shows it back. An event missing from the queue was delivered or
//! dismissed.

use super::capabilities::{Failure, Watch, community, on_tokio};
use super::protocol::INVALID_PARAMS;
use buzz_relay_client::EventId;
use buzz_relay_client::outbox::{Delivery, Operation, Outbox};
use gpui_kit::*;
use serde_json::{Value, json};

fn delivery(delivery: Delivery) -> &'static str {
    match delivery {
        Delivery::Sending => "sending",
        Delivery::Accepted => "accepted",
        Delivery::Unknown => "unknown",
        Delivery::Failed => "failed",
        Delivery::Seen => "seen",
    }
}

fn operation(operation: &Operation) -> Value {
    let channel = operation
        .event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .find(|tag| tag.first().is_some_and(|name| name == "h"))
        .and_then(|tag| tag.get(1).cloned());
    json!({
        "id": operation.id().to_hex(),
        "kind": operation.event.kind.as_u16(),
        "channel": channel,
        "delivery": delivery(operation.delivery),
        "error": operation.error,
        "acknowledged": operation.acknowledged,
    })
}

fn pending(outbox: &Outbox) -> Value {
    json!(
        outbox
            .pending()
            .iter()
            .map(|op| operation(op))
            .collect::<Vec<_>>()
    )
}

async fn outbox_of(session: std::sync::Arc<buzz_relay_client::Session>) -> Result<Outbox, Failure> {
    session
        .outbox()
        .await
        .ok_or_else(|| (3, "This community takes no writes.".to_owned()))
}

/// `outbox/get`: one event's delivery while the outbox still knows it,
/// or `null`; without `id`, every queued event.
pub fn get(params: &Value, cx: &App) -> Result<Task<Result<Value, Failure>>, Failure> {
    let id = match params.get("id").and_then(Value::as_str) {
        None => None,
        Some(hex) => Some(
            EventId::from_hex(hex)
                .map_err(|_| (INVALID_PARAMS, "id is not an event id".to_owned()))?,
        ),
    };
    let session = community(cx)?.read(cx).session.clone();
    Ok(on_tokio(cx, async move {
        let outbox = outbox_of(session).await?;
        Ok(match id {
            None => pending(&outbox),
            Some(id) => outbox.get(&id).map_or(Value::Null, |op| operation(&op)),
        })
    }))
}

/// `outbox/watch`: sends `outbox/changed` with the queued events now and
/// whenever the queue changes.
pub fn watch(
    _params: &Value,
    notify: impl Fn(Value, &mut App) + 'static,
    cx: &mut App,
) -> Result<(Watch, Value), Failure> {
    let session = community(cx)?.read(cx).session.clone();
    let (sender, mut changes) = futures::channel::mpsc::unbounded::<Value>();
    crate::runtime::Runtime::handle(cx).spawn(async move {
        let Some(outbox) = session.outbox().await else {
            return;
        };
        let mut receiver = outbox.changes();
        let mut last = Value::Null;
        loop {
            let now = pending(&outbox);
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
        while let Some(queued) = changes.next().await {
            cx.update(|cx| notify(json!({ "events": queued }), cx));
        }
    });
    Ok((Watch::new((), forward), json!({})))
}
