//! A whole community session against the local test relay. Skipped unless
//! `BUZZ_TEST_RELAY` points at `.test-relay`.

use buzz_identity::Identity;
use buzz_relay_client::*;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

fn fixture() -> Option<(RelayOrigin, Value, Value)> {
    let dir = std::env::var("BUZZ_TEST_RELAY").ok()?;
    let read = |name: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/{name}")).unwrap()).unwrap()
    };
    let identities = read("identities.json");
    let origin = RelayOrigin::parse(identities["relay_ws_url"].as_str().unwrap()).unwrap();
    Some((origin, identities, read("seed.json")))
}

async fn until<T: Clone>(
    receiver: &mut watch::Receiver<T>,
    what: &str,
    predicate: impl Fn(&T) -> bool,
) -> T {
    tokio::time::timeout(
        Duration::from_secs(20),
        receiver.wait_for(|value| predicate(value)),
    )
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    .unwrap()
    .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_channels_and_folds_a_timeline() {
    let Some((origin, identities, seed)) = fixture() else {
        eprintln!("BUZZ_TEST_RELAY not set; skipping");
        return;
    };
    let alice = Arc::new(
        Identity::from_nsec(identities["identities"]["alice"]["nsec"].as_str().unwrap()).unwrap(),
    );
    let session = Session::open(&tokio::runtime::Handle::current(), origin, alice);
    let mut snapshot = session.snapshot();
    let channel = |name: &str| seed["channels"][name].as_str().unwrap().to_string();

    let ready = until(&mut snapshot, "the roster", |s| {
        s.roster == RosterStatus::Ready && !s.channels.is_empty()
    })
    .await;
    let ids: Vec<&str> = ready.channels.iter().map(|c| c.id.as_str()).collect();
    for name in ["general", "random", "core-team", "dm-alice-bob"] {
        assert!(
            ids.contains(&channel(name).as_str()),
            "{name} missing from {ids:?}"
        );
    }
    let dm = ready
        .channels
        .iter()
        .find(|c| c.id == channel("dm-alice-bob"))
        .unwrap();
    assert_eq!(dm.channel_type, Some(ChannelType::Dm));
    assert_eq!(ready.relay.as_ref().unwrap().authority.len(), 64);

    let view = session.open_channel(&channel("general"));
    let mut timeline = view.snapshot.clone();
    let welcome = seed["events"]["welcome"].as_str().unwrap();
    // Other tests post to #general too, so page back to the seeded
    // welcome rather than expecting it on the first page.
    let mut shown = 0;
    let loaded = loop {
        let page = until(&mut timeline, "a #general page", |t| {
            t.status == TimelineStatus::Ready
                && !t.loading_older
                && (t.rows.len() > shown || !t.has_more)
        })
        .await;
        if page.rows.iter().any(|r| r.id.to_hex() == welcome) {
            break page;
        }
        assert!(page.has_more, "the welcome is not in #general");
        shown = page.rows.len();
        view.load_older();
    };
    let row = loaded
        .rows
        .iter()
        .find(|r| r.id.to_hex() == welcome)
        .unwrap();
    // At least the seeded replies; other runs may have added more.
    assert!(row.reply_count >= 2, "thread summary folded in");
    assert!(!row.reactions.is_empty(), "reactions folded in");
    // Replies stay in the thread, not the channel timeline.
    let reply = seed["events"]["thread_reply_bob"].as_str().unwrap();
    assert!(loaded.rows.iter().all(|r| r.id.to_hex() != reply));

    let bob = identities["identities"]["bob"]["pk_hex"]
        .as_str()
        .unwrap()
        .to_string();
    let profiles = until(&mut snapshot, "bob's profile", |s| {
        s.profiles.contains_key(&bob)
    })
    .await;
    assert!(!profiles.profiles[&bob].name_is_fallback);
}
