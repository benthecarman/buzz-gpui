//! Profile and status against the local test relay. Skipped unless
//! `BUZZ_TEST_RELAY` points at `.test-relay`. Leaving is not exercised: it
//! would end a fixture identity's membership.

use buzz_identity::Identity;
use buzz_relay_client::*;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

fn fixture() -> Option<(RelayOrigin, Arc<Identity>)> {
    let dir = std::env::var("BUZZ_TEST_RELAY").ok()?;
    let identities: Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/identities.json")).unwrap())
            .unwrap();
    let origin = RelayOrigin::parse(identities["relay_ws_url"].as_str().unwrap()).unwrap();
    let carol =
        Identity::from_nsec(identities["identities"]["carol"]["nsec"].as_str().unwrap()).unwrap();
    Some((origin, Arc::new(carol)))
}

async fn wait_for(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_status_and_profile() {
    let Some((origin, carol)) = fixture() else {
        eprintln!("BUZZ_TEST_RELAY not set; skipping");
        return;
    };
    let store = Store::in_memory().unwrap();
    let viewer = carol.pubkey_hex();
    let session = Session::open(
        &tokio::runtime::Handle::current(),
        origin.clone(),
        carol.clone(),
        store,
    );

    // Status: set, read back, clear.
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    session
        .set_status(StatusInput {
            text: "Testing settings".into(),
            emoji: "🧪".into(),
            expires_at: Some(expires),
        })
        .await
        .unwrap();
    let status = session.own_status().await.unwrap().unwrap();
    assert_eq!(status.text, "Testing settings");
    assert_eq!(status.emoji, "🧪");
    assert_eq!(status.expires_at, Some(expires));
    session.set_status(StatusInput::default()).await.unwrap();
    assert_eq!(session.own_status().await.unwrap(), None);

    // Profile: republish the current fields, which the session shows.
    let http = RelayHttp::new(origin.clone(), carol.clone());
    let filter = buzz_relay_client::account::own_profile_filter(&viewer);
    let events = http.query(&[filter]).await.unwrap();
    let current = events
        .iter()
        .max_by_key(|event| event.created_at)
        .map(|event| serde_json::from_str::<Value>(&event.content).unwrap())
        .unwrap_or(Value::Null);
    let text = |key: &str| current[key].as_str().unwrap_or_default().to_string();
    let name = Some(text("display_name"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Carol".into());
    session
        .publish_profile(ProfileEdit {
            name: name.clone(),
            picture: text("picture"),
            about: text("about"),
        })
        .await
        .unwrap();
    let snapshot = session.snapshot();
    assert!(
        wait_for(|| snapshot
            .borrow()
            .profiles
            .get(&viewer)
            .is_some_and(|profile| profile.name == name))
        .await
    );

    // The availability choice reaches the viewer's own presence.
    session.watch_presence(vec![viewer.clone()]);
    session.set_presence_mode(PresenceMode::Away);
    assert!(
        wait_for(|| snapshot.borrow().presence.get(&viewer) == Some(&PresenceState::Away)).await
    );

    session.dispose().await;
}
