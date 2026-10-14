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

/// The tags of a reply to `event` in `channel`, in its thread: its root
/// when that is another event, the event itself, and `to`, the agent's
/// owner. Tagging whoever asked could be a sibling agent, which wakes it,
/// and two agents failing alike would wake each other without end.
fn reply_tags(channel: &str, event: &BuzzEvent, to: &str) -> Vec<Vec<String>> {
    let root = crate::buzz_mcp::tools::root_of(event);
    let mut tags = vec![vec!["h".to_string(), channel.to_string()]];
    if root != event.id {
        tags.push(vec!["e".into(), root, String::new(), "root".into()]);
    }
    tags.push(vec![
        "e".into(),
        event.id.clone(),
        String::new(),
        "reply".into(),
    ]);
    tags.push(vec!["p".into(), to.to_owned()]);
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

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
