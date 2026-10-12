//! Agent types compiled into the app, buzz-app's `src/agent-plugins`.

pub mod claude_code;
pub mod codex;
pub mod owner_relay;
pub mod process;

use crate::agents2::attention::EventData;
use crate::buzz_mcp::BuzzEvent;

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
/// when that is another event, the event itself, and its author.
fn reply_tags(channel: &str, event: &BuzzEvent) -> Vec<Vec<String>> {
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
    tags.push(vec!["p".into(), event.pubkey.clone()]);
    tags
}
