//! Agent types compiled into the app, buzz-app's `src/agent-plugins`.

pub mod claude_code;
pub mod codex;
pub mod owner_relay;
pub mod process;
mod tabs;

use crate::agents2::attention::EventData;
use crate::buzz_mcp::BuzzEvent;
use futures::future::BoxFuture;
use std::sync::Arc;

/// Keeps a delivery counted as working until the future settles: its
/// turn outlives `run`, which returns once the turn is handed over.
pub type HoldUntil = Arc<dyn Fn(BoxFuture<'static, ()>) + Send + Sync>;

/// A delivery's [`HoldUntil`], from its working hold.
fn hold_until(working: crate::agents2::runtime::Working) -> HoldUntil {
    Arc::new(move |done| working.until(done))
}

/// The event an agent was woken for, as the Buzz tools take it.
fn event_of(event: &EventData) -> BuzzEvent {
    BuzzEvent {
        id: event.id.clone(),
        pubkey: event.pubkey.clone(),
        created_at: event.created_at,
        kind: u32::from(event.kind),
        tags: event.tags.clone(),
        content: event.content.clone(),
        sig: String::new(),
    }
}

/// The tags of a reply to `event` in `channel`: in its thread, flat, at
/// the thread's root, and to `to`, the agent's owner. Tagging whoever
/// asked could be a sibling agent, which wakes it, and two agents failing
/// alike would wake each other without end.
fn reply_tags(channel: &str, event: &BuzzEvent, to: &str) -> Vec<Vec<String>> {
    let root = crate::buzz_mcp::tools::root_of(event);
    vec![
        vec!["h".to_string(), channel.to_string()],
        vec!["e".into(), root, String::new(), "reply".into()],
        vec!["p".into(), to.to_owned()],
    ]
}

/// Whether an agent answers `event`: its owner's, or one by another agent
/// the owner attested (NIP-OA). Both compiled-in types answer nobody else.
pub(crate) fn from_owner(event: &BuzzEvent, owner: &str) -> bool {
    event.pubkey == owner
        || u16::try_from(event.kind).ok().and_then(|kind| {
            buzz_relay_client::profiles::attested_owner_of(
                &event.pubkey,
                kind,
                event.created_at,
                &event.tags,
            )
        }) == Some(owner.to_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nostr::prelude::{Keys, ToBech32};

    /// An owner, an agent it attested, and that attestation's `auth` tag.
    pub(crate) fn attested() -> (String, String, Vec<String>) {
        let (owner, agent) = (Keys::generate(), Keys::generate());
        let nsec = owner.secret_key().to_bech32().unwrap();
        let identity = buzz_identity::Identity::from_nsec(&nsec).unwrap();
        let auth = identity.attest_agent(&agent.public_key().to_hex()).unwrap();
        (
            owner.public_key().to_hex(),
            agent.public_key().to_hex(),
            serde_json::from_str(&auth).unwrap(),
        )
    }

    #[test]
    fn answers_the_owner_and_agents_the_owner_attested_nobody_else() {
        let (owner, agent, auth) = attested();
        let from = |pubkey: &str, tags: Vec<Vec<String>>| BuzzEvent {
            pubkey: pubkey.into(),
            kind: 9,
            created_at: 100,
            tags,
            ..Default::default()
        };
        assert!(from_owner(&from(&owner, vec![]), &owner));
        assert!(from_owner(&from(&agent, vec![auth.clone()]), &owner));
        assert!(!from_owner(&from(&agent, vec![]), &owner));
        // Another key cannot borrow the agent's attestation, nor claim an
        // owner.
        assert!(!from_owner(
            &from(&"e".repeat(64), vec![auth.clone()]),
            &owner
        ));
        let forged = vec!["auth".into(), owner.clone(), String::new(), "0".repeat(128)];
        assert!(!from_owner(&from(&agent, vec![forged]), &owner));
    }

    #[test]
    fn a_reply_tags_the_owner_not_whoever_asked() {
        let event = BuzzEvent {
            id: "1".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 1,
            kind: 9,
            tags: vec![vec!["h".into(), "c".into()]],
            content: "hi".into(),
            sig: String::new(),
        };
        let tags = reply_tags("c", &event, &"f".repeat(64));
        assert_eq!(tags.last(), Some(&vec!["p".to_string(), "f".repeat(64)]));
        assert!(!tags.iter().any(|tag| tag.get(1) == Some(&event.pubkey)));
    }
}
