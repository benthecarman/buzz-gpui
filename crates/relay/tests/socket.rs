use buzz_identity::Identity;
use buzz_relay_client::*;
use futures::{SinkExt, StreamExt};
use nostr::prelude::{Event, EventBuilder, FinalizeEvent, Keys, Kind};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// What the fake relay saw from the client.
#[derive(Debug)]
enum Seen {
    Connected,
    Auth(Event),
    Req(String, Vec<Value>),
    Close(String),
    Event(Event),
}

struct FakeRelay {
    address: String,
    seen: mpsc::UnboundedReceiver<Seen>,
    /// Messages to push to the connected client.
    push: mpsc::UnboundedSender<Value>,
    /// Drops the current connection when sent.
    kick: mpsc::UnboundedSender<()>,
}

impl FakeRelay {
    /// `require_auth`: send a challenge on connect and refuse REQs until AUTH.
    async fn start(require_auth: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("ws://{}", listener.local_addr().unwrap());
        let (seen_tx, seen) = mpsc::unbounded_channel();
        let (push, mut push_rx) = mpsc::unbounded_channel::<Value>();
        let (kick, mut kick_rx) = mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                seen_tx.send(Seen::Connected).unwrap();
                let mut authed = !require_auth;
                if require_auth {
                    ws.send(Message::Text(
                        json!(["AUTH", "challenge-1"]).to_string().into(),
                    ))
                    .await
                    .unwrap();
                }
                loop {
                    tokio::select! {
                        Some(()) = kick_rx.recv() => break,
                        Some(value) = push_rx.recv() => {
                            ws.send(Message::Text(value.to_string().into())).await.unwrap();
                        }
                        message = ws.next() => {
                            let Some(Ok(Message::Text(text))) = message else { break };
                            let value: Vec<Value> = serde_json::from_str(text.as_str()).unwrap();
                            match value[0].as_str().unwrap() {
                                "AUTH" => {
                                    let event: Event = serde_json::from_value(value[1].clone()).unwrap();
                                    authed = event.verify().is_ok();
                                    let ok = json!(["OK", event.id.to_hex(), authed, ""]);
                                    seen_tx.send(Seen::Auth(event)).unwrap();
                                    ws.send(Message::Text(ok.to_string().into())).await.unwrap();
                                }
                                "REQ" => {
                                    let id = value[1].as_str().unwrap().to_string();
                                    if !authed {
                                        let closed = json!(["CLOSED", id, "auth-required: sign in"]);
                                        ws.send(Message::Text(closed.to_string().into())).await.unwrap();
                                        continue;
                                    }
                                    seen_tx.send(Seen::Req(id.clone(), value[2..].to_vec())).unwrap();
                                    ws.send(Message::Text(json!(["EOSE", id]).to_string().into()))
                                        .await
                                        .unwrap();
                                }
                                "CLOSE" => {
                                    seen_tx.send(Seen::Close(value[1].as_str().unwrap().into())).unwrap();
                                }
                                "EVENT" => {
                                    let event: Event = serde_json::from_value(value[1].clone()).unwrap();
                                    let rejected = event.content == "reject me";
                                    let ok = json!(["OK", event.id.to_hex(), !rejected, if rejected { "blocked: no" } else { "" }]);
                                    seen_tx.send(Seen::Event(event)).unwrap();
                                    ws.send(Message::Text(ok.to_string().into())).await.unwrap();
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        });
        Self {
            address,
            seen,
            push,
            kick,
        }
    }

    async fn next(&mut self) -> Seen {
        tokio::time::timeout(Duration::from_secs(5), self.seen.recv())
            .await
            .expect("relay saw nothing")
            .unwrap()
    }
}

fn options() -> SocketOptions {
    SocketOptions {
        auth_wait: Duration::from_millis(200),
        min_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(50),
        publish_timeout: Duration::from_secs(2),
        ..Default::default()
    }
}

fn socket(relay: &FakeRelay, identity: &Arc<Identity>) -> RelaySocket {
    RelaySocket::connect(
        &tokio::runtime::Handle::current(),
        RelayOrigin::parse(&relay.address).unwrap(),
        identity.clone(),
        Arc::new(Verifier::default()),
        options(),
    )
}

async fn next_event(subscription: &mut Subscription) -> SubscriptionEvent {
    tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("no subscription event")
        .expect("subscription ended")
}

fn filter(value: Value) -> Filter {
    value.as_object().unwrap().clone()
}

fn note(keys: &Keys, content: &str) -> Event {
    EventBuilder::new(Kind::TextNote, content)
        .finalize(keys)
        .unwrap()
}

#[tokio::test]
async fn authenticates_before_subscribing() {
    let mut relay = FakeRelay::start(true).await;
    let identity = Arc::new(Identity::ephemeral());
    let socket = socket(&relay, &identity);
    let mut subscription = socket.subscribe(vec![filter(json!({"kinds": [9], "#h": ["general"]}))]);

    assert!(matches!(relay.next().await, Seen::Connected));
    let Seen::Auth(auth) = relay.next().await else {
        panic!("expected auth first");
    };
    assert_eq!(auth.kind, Kind::Authentication);
    assert_eq!(auth.pubkey, identity.public_key());
    let tags: Vec<Vec<String>> = auth.tags.iter().map(|t| t.as_slice().to_vec()).collect();
    assert!(tags.contains(&vec!["challenge".into(), "challenge-1".into()]));
    assert!(tags.contains(&vec!["relay".into(), relay.address.clone()]));

    let Seen::Req(_, filters) = relay.next().await else {
        panic!("expected the held REQ after auth");
    };
    assert_eq!(filters, vec![json!({"kinds": [9], "#h": ["general"]})]);
    assert!(matches!(
        next_event(&mut subscription).await,
        SubscriptionEvent::Eose
    ));
    let mut state = socket.state();
    state.wait_for(|s| *s == SocketState::Ready).await.unwrap();
}

#[tokio::test]
async fn delivers_verified_events_only() {
    let mut relay = FakeRelay::start(false).await;
    let identity = Arc::new(Identity::ephemeral());
    let socket = socket(&relay, &identity);
    let mut subscription = socket.subscribe(vec![filter(json!({"kinds": [1]}))]);
    assert!(matches!(relay.next().await, Seen::Connected));
    let Seen::Req(id, _) = relay.next().await else {
        panic!("expected REQ");
    };
    assert!(matches!(
        next_event(&mut subscription).await,
        SubscriptionEvent::Eose
    ));

    let author = Keys::generate();
    let good = note(&author, "hello");
    let mut forged = serde_json::to_value(note(&author, "original")).unwrap();
    forged["content"] = json!("tampered");
    relay.push.send(json!(["EVENT", id, forged])).unwrap();
    relay.push.send(json!(["EVENT", id, good])).unwrap();

    let SubscriptionEvent::Event(event) = next_event(&mut subscription).await else {
        panic!("expected an event");
    };
    assert_eq!(event.content, "hello", "the forged event must be dropped");

    drop(subscription);
    assert!(matches!(relay.next().await, Seen::Close(closed) if closed == id));
}

#[tokio::test]
async fn publish_waits_for_ok_and_reports_rejections() {
    let mut relay = FakeRelay::start(false).await;
    let identity = Arc::new(Identity::ephemeral());
    let keys = Keys::generate();
    let socket = socket(&relay, &identity);
    let mut state = socket.state();
    state.wait_for(|s| *s == SocketState::Ready).await.unwrap();

    socket.publish(note(&keys, "hi")).await.unwrap();
    assert!(matches!(relay.next().await, Seen::Connected));
    assert!(matches!(relay.next().await, Seen::Event(e) if e.content == "hi"));
    assert_eq!(
        socket.publish(note(&keys, "reject me")).await,
        Err(PublishError::Rejected("blocked: no".into()))
    );
}

#[tokio::test]
async fn resubscribes_after_reconnect_and_fails_publishes_while_down() {
    let mut relay = FakeRelay::start(true).await;
    let identity = Arc::new(Identity::ephemeral());
    let socket = socket(&relay, &identity);
    let mut subscription = socket.subscribe(vec![filter(json!({"kinds": [9]}))]);
    assert!(matches!(relay.next().await, Seen::Connected));
    assert!(matches!(relay.next().await, Seen::Auth(_)));
    let Seen::Req(first_id, _) = relay.next().await else {
        panic!("expected REQ");
    };
    assert!(matches!(
        next_event(&mut subscription).await,
        SubscriptionEvent::Eose
    ));

    relay.kick.send(()).unwrap();
    let mut state = socket.state();
    state.wait_for(|s| *s != SocketState::Ready).await.unwrap();

    assert!(matches!(relay.next().await, Seen::Connected));
    assert!(
        matches!(relay.next().await, Seen::Auth(_)),
        "re-authenticates"
    );
    let Seen::Req(second_id, filters) = relay.next().await else {
        panic!("expected the REQ again");
    };
    assert_eq!(first_id, second_id);
    assert_eq!(filters, vec![json!({"kinds": [9]})]);
    assert!(matches!(
        next_event(&mut subscription).await,
        SubscriptionEvent::Resubscribed
    ));
    assert!(matches!(
        next_event(&mut subscription).await,
        SubscriptionEvent::Eose
    ));
}

#[tokio::test]
async fn publish_without_connection_fails_fast() {
    let identity = Arc::new(Identity::ephemeral());
    let keys = Keys::generate();
    let socket = RelaySocket::connect(
        &tokio::runtime::Handle::current(),
        RelayOrigin::parse("ws://127.0.0.1:1").unwrap(),
        identity.clone(),
        Arc::new(Verifier::default()),
        options(),
    );
    let result = tokio::time::timeout(Duration::from_secs(2), socket.publish(note(&keys, "x")))
        .await
        .expect("publish must not wait for a connection");
    assert_eq!(result, Err(PublishError::NotConnected));
}

#[tokio::test]
async fn without_resubscribe_subscriptions_are_reported_lost() {
    let mut relay = FakeRelay::start(false).await;
    let identity = Arc::new(Identity::ephemeral());
    let socket = RelaySocket::connect(
        &tokio::runtime::Handle::current(),
        RelayOrigin::parse(&relay.address).unwrap(),
        identity,
        Arc::new(Verifier::default()),
        SocketOptions {
            resubscribe: false,
            ..options()
        },
    );
    let mut subscription = socket.subscribe(vec![filter(json!({"kinds": [9]}))]);
    assert!(matches!(relay.next().await, Seen::Connected));
    assert!(matches!(relay.next().await, Seen::Req(..)));
    assert!(matches!(
        next_event(&mut subscription).await,
        SubscriptionEvent::Eose
    ));

    relay.kick.send(()).unwrap();
    assert!(matches!(
        next_event(&mut subscription).await,
        SubscriptionEvent::Lost
    ));
    assert!(matches!(relay.next().await, Seen::Connected));
    // Nothing is re-sent on the new connection.
    assert!(
        tokio::time::timeout(Duration::from_millis(500), relay.seen.recv())
            .await
            .is_err()
    );
}
