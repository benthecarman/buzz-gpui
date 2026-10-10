//! What the composer publishes, against the local test relay: mentions of
//! people outside the channel and custom emoji. Skipped unless
//! `BUZZ_TEST_RELAY` points at `.test-relay`. Messages go to a throwaway
//! channel whose name carries the process id.

use buzz_identity::Identity;
use buzz_relay_client::lifecycle::NewChannel;
use buzz_relay_client::*;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

struct Fixture {
    origin: RelayOrigin,
    identities: Value,
}

fn fixture() -> Option<Fixture> {
    let dir = std::env::var("BUZZ_TEST_RELAY").ok()?;
    let identities: Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/identities.json")).unwrap())
            .unwrap();
    Some(Fixture {
        origin: RelayOrigin::parse(identities["relay_ws_url"].as_str().unwrap()).unwrap(),
        identities,
    })
}

impl Fixture {
    fn pubkey(&self, name: &str) -> String {
        self.identities["identities"][name]["pk_hex"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn session(&self, name: &str) -> Session {
        let nsec = self.identities["identities"][name]["nsec"]
            .as_str()
            .unwrap();
        let session = Session::open(
            &tokio::runtime::Handle::current(),
            self.origin.clone(),
            Arc::new(Identity::from_nsec(nsec).unwrap()),
            Store::in_memory().unwrap(),
        );
        let mut snapshot = session.snapshot();
        until(&mut snapshot, "the roster", |s| {
            s.roster == RosterStatus::Ready && s.live
        })
        .await;
        session
    }
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

/// The accepted echo of `content` in the channel `view` follows.
async fn echo(view: &ChannelView, content: &str) -> Arc<fold::MessageRow> {
    let mut timeline = view.snapshot.clone();
    let seen = until(&mut timeline, content, |t| {
        t.rows.iter().any(|r| {
            r.content == content
                && r.delivery
                    .as_ref()
                    .is_none_or(|d| d.status == fold::Delivery::Seen)
        })
    })
    .await;
    seen.rows
        .iter()
        .find(|r| r.content == content)
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn mentions_outsiders_as_references_or_invites_them() {
    let Some(fixture) = fixture() else {
        eprintln!("BUZZ_TEST_RELAY not set; skipping");
        return;
    };
    let alice = fixture.session("alice").await;
    let carol = fixture.pubkey("carol");
    let id = alice
        .create_channel(NewChannel {
            name: format!("#outsiders-{}", std::process::id()),
            description: "Created by a test".into(),
            private: false,
        })
        .await
        .unwrap();
    let view = alice.open_channel(&id);

    // "Do nothing": the outsider is named but not notified or added.
    let skipped = format!("@Carol not added {}", std::process::id());
    alice
        .send(
            &id,
            &skipped,
            Mentions {
                recipients: Vec::new(),
                references: vec![carol.clone()],
            },
            Vec::new(),
            None,
        )
        .await
        .unwrap();
    let row = echo(&view, &skipped).await;
    assert!(row.mentions.is_empty());
    assert_eq!(
        row.mention_references
            .iter()
            .map(|k| k.to_hex())
            .collect::<Vec<_>>(),
        std::slice::from_ref(&carol)
    );

    // "Invite": add them, then they are a recipient like any member.
    alice.add_member(&id, &carol).await.unwrap();
    let invited = format!("@Carol welcome {}", std::process::id());
    alice
        .send(
            &id,
            &invited,
            Mentions {
                recipients: vec![carol.clone()],
                references: Vec::new(),
            },
            Vec::new(),
            None,
        )
        .await
        .unwrap();
    let row = echo(&view, &invited).await;
    assert_eq!(
        row.mentions.iter().map(|k| k.to_hex()).collect::<Vec<_>>(),
        [carol]
    );
}
