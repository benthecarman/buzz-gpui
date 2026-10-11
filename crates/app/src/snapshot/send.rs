//! Delivering a snapshot file in a direct message through the session's
//! durable outbox (`snapshot-send.ts`). The message journals its intent
//! (see [`super::share::recovery_value`]) atomically with itself, so a send
//! interrupted by a restart resumes the exact same message, never a second
//! encode, upload or message.

use super::share::{EncodedSnapshot, validate_file};
use buzz_relay_client::messages::{self, Compose, EmojiCatalog, Mentions};
use buzz_relay_client::upload::{self, UploadFile};
use buzz_relay_client::{Attachment, Delivery, EventId, Media, Outbox, Recovery, SendOptions};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Idle,
    Preparing,
    Uploading,
    Sending,
    Done,
    Error,
}

impl Phase {
    pub fn sending(self) -> bool {
        matches!(self, Self::Preparing | Self::Uploading | Self::Sending)
    }
}

/// The message a send produced. Kept before delivery is confirmed, so an
/// uncertain result retries this exact message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub channel: String,
    pub event_id: EventId,
}

/// Uploads a snapshot file to the community's media as it is: the PNG
/// keeps its manifest chunk.
pub async fn upload(media: &Media, file: &EncodedSnapshot) -> Result<Attachment, String> {
    validate_file(file)?;
    upload::upload(
        media,
        UploadFile {
            name: file.file_name.clone(),
            mime: file.mime().into(),
            bytes: file.bytes.clone(),
        },
    )
    .await
    .map_err(|error| error.to_string())
}

/// Queues the message carrying `attachment` in `channel`, journaled with
/// `recovery`. Returns its event ID.
pub fn queue(
    outbox: &Outbox,
    origin: &buzz_relay_client::RelayOrigin,
    channel: &str,
    attachment: Attachment,
    recovery: Recovery,
) -> Result<EventId, String> {
    let draft = messages::message(&Compose {
        channel_id: channel,
        content: "",
        mentions: &Mentions::default(),
        attachments: std::slice::from_ref(&attachment),
        emoji: EmojiCatalog::Ready(&[]),
        origin,
    })
    .map_err(|error| error.to_string())?;
    outbox
        .send(
            draft,
            SendOptions {
                supersedes: None,
                recovery: Some(recovery),
                gate: None,
            },
        )
        .map_err(|error| error.to_string())
}

/// Follows the outbox until `receipt` is delivered, first retrying it when
/// it failed or its outcome is unknown (`directMessages.delivered`). The
/// outbox owns delivery deadlines; this only follows its state.
pub async fn delivered(outbox: &Outbox, receipt: &Receipt) -> Result<(), String> {
    let Some(op) = outbox.get(&receipt.event_id) else {
        return Err("The earlier send could not be confirmed. Reconnect and retry.".into());
    };
    if op.kind() != 9 || op.channel() != Some(receipt.channel.as_str()) {
        return Err("The saved message does not belong to this conversation.".into());
    }
    if matches!(op.delivery, Delivery::Failed | Delivery::Unknown) {
        outbox
            .retry(&receipt.event_id, None)
            .map_err(|error| error.to_string())?;
    }
    let mut changes = outbox.changes();
    loop {
        match outbox.get(&receipt.event_id) {
            Some(op) => match op.delivery {
                Delivery::Accepted | Delivery::Seen => return Ok(()),
                Delivery::Failed | Delivery::Unknown => {
                    return Err(op.error.clone().unwrap_or_else(|| {
                        "The message could not be confirmed. Retry to continue.".into()
                    }));
                }
                Delivery::Sending => {}
            },
            None => return Err("The earlier send was dismissed.".into()),
        }
        changes
            .changed()
            .await
            .map_err(|_| "The session has closed".to_string())?;
    }
}

/// Where a send stands, for the outbox's own view of a receipt.
pub fn delivery(outbox: &Outbox, receipt: &Receipt) -> Option<Delivery> {
    outbox.get(&receipt.event_id).map(|op| op.delivery)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Kind;
    use crate::snapshot::manifest::{MemoryLevel, parse_agent_snapshot};
    use crate::snapshot::png::{Format, encode};
    use crate::snapshot::share::{self, Recipient};
    use buzz_identity::Identity;
    use buzz_relay_client::{RelayOrigin, Session, Store};
    use std::sync::Arc;
    use std::time::Duration;

    fn fixture() -> Option<(RelayOrigin, serde_json::Value)> {
        let dir = std::env::var("BUZZ_TEST_RELAY").ok()?;
        let identities: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{dir}/identities.json")).unwrap(),
        )
        .unwrap();
        let origin = RelayOrigin::parse(identities["relay_ws_url"].as_str().unwrap()).unwrap();
        Some((origin, identities))
    }

    /// Alice sends Bob a snapshot over the plain test relay: the DM opens,
    /// the PNG uploads with its manifest intact, the message is delivered
    /// with its journal entry, and the file reads back byte for byte.
    #[tokio::test(flavor = "multi_thread")]
    async fn sends_a_snapshot_in_a_direct_message() {
        let Some((origin, identities)) = fixture() else {
            eprintln!("BUZZ_TEST_RELAY not set; skipping");
            return;
        };
        let identity = |name: &str| {
            Arc::new(
                Identity::from_nsec(identities["identities"][name]["nsec"].as_str().unwrap())
                    .unwrap(),
            )
        };
        let (alice, bob) = (identity("alice"), identity("bob"));
        let session = Session::open(
            &tokio::runtime::Handle::current(),
            origin.clone(),
            alice.clone(),
            Store::in_memory().unwrap(),
        );
        let mut snapshot = session.snapshot();
        tokio::time::timeout(
            Duration::from_secs(20),
            snapshot.wait_for(|s| s.roster == buzz_relay_client::RosterStatus::Ready),
        )
        .await
        .unwrap()
        .unwrap();
        let channel = session.open_dm(vec![bob.pubkey_hex()]).await.unwrap();

        // A unique name keeps reruns from reusing an earlier upload.
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let manifest = format!(
            r#"{{"format":"buzz-agent-snapshot","version":1,"definition":{{"name":"Scout {nonce}"}},"profile":{{"displayName":"Scout {nonce}"}},"memory":{{"level":"none"}}}}"#
        );
        let parsed = parse_agent_snapshot(manifest.as_bytes(), false).unwrap();
        let file = EncodedSnapshot {
            bytes: encode(&parsed, Format::Png, None).unwrap(),
            file_name: "Scout.agent.png".into(),
        };
        let attachment = upload(session.media(), &file).await.unwrap();
        assert_eq!(attachment.sha256, share::sha256_hex(&file.bytes));

        let outbox = session.outbox().await.unwrap();
        let key = share::recovery_key(Kind::Agent, &nonce).unwrap();
        let people = [Recipient {
            pubkey: bob.pubkey_hex(),
            name: "Bob".into(),
            is_agent: false,
        }];
        let recovery = Recovery {
            key: key.clone(),
            value: share::recovery_value(&people, MemoryLevel::None),
        };
        let event_id = queue(
            &outbox,
            session.origin(),
            &channel,
            attachment.clone(),
            recovery,
        )
        .unwrap();
        let receipt = Receipt {
            channel: channel.clone(),
            event_id,
        };
        // The journal holds the intent until the send is acknowledged.
        let recovered = share::recover(&outbox.pending(), &key).unwrap().unwrap();
        assert_eq!(recovered.people, people);
        assert_eq!(recovered.channel, channel);
        tokio::time::timeout(Duration::from_secs(20), delivered(&outbox, &receipt))
            .await
            .unwrap()
            .unwrap();
        outbox.acknowledge(&event_id).await.unwrap();
        assert_eq!(share::recover(&outbox.pending(), &key).unwrap(), None);

        // Bob reads the file through his own authenticated media.
        let bobs = Media::new(tokio::runtime::Handle::current(), origin, bob);
        let bytes = share::read_attachment(&bobs, &attachment.url, Some(&attachment.name))
            .await
            .unwrap();
        assert_eq!(bytes, file.bytes);
        assert_eq!(parse_agent_snapshot(&bytes, false).unwrap(), parsed);
    }
}
