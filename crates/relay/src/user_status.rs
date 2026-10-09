//! User status (kind 30315, NIP-38; `docs/port/relay.md` §5j): parsing and
//! the event a save publishes. I/O lives in the session.

use crate::event::Verified;
use crate::messages::EventTemplate;
use crate::socket::Filter;
use serde_json::{Value, json};

pub const KIND: u16 = crate::event::kinds::USER_STATUS;
/// Text and emoji limit, in UTF-16 units as the reference counts them.
pub const TEXT_LIMIT: usize = 100;
/// How far past now a save may date its event (clock skew guard).
const MAX_SKEW_SECS: u64 = 300;

/// A status as shown. Empty and expired statuses are `None` instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserStatus {
    pub text: String,
    /// A unicode emoji or `:shortcode:`.
    pub emoji: String,
    pub updated_at: u64,
    pub expires_at: Option<u64>,
}

/// What the user asked for. Empty text and emoji clear the status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusInput {
    pub text: String,
    pub emoji: String,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StatusError {
    #[error("Keep your status to one short line (100 characters).")]
    TooLong,
    #[error("Choose an expiration in the future.")]
    Expired,
    #[error(
        "Your current status is dated too far in the future. Check your devices’ clocks and try again later."
    )]
    ClockSkew,
}

fn valid_text(text: &str, emoji: &str) -> bool {
    text.encode_utf16().count() <= TEXT_LIMIT
        && emoji.encode_utf16().count() <= TEXT_LIMIT
        && !text.contains(['\r', '\n'])
        && !emoji.contains(['\r', '\n'])
}

/// The read of one author's current status (`#d` `general`).
pub fn filter(author: &str) -> Filter {
    match json!({"kinds": [KIND], "#d": ["general"], "authors": [author], "limit": 1}) {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

/// The newest valid event's status, or `None` when there is none, it was
/// cleared, or it has expired by `now`.
pub fn current(events: &[Verified], author: &str, now: u64) -> Option<UserStatus> {
    events
        .iter()
        .filter(|event| event.pubkey_hex() == author)
        .filter_map(|event| parse(event).map(|status| (event, status)))
        .max_by(|(a, _), (b, _)| {
            if crate::event::is_newer(a, b) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        })
        .map(|(_, status)| status)
        .filter(|status| !(status.text.is_empty() && status.emoji.is_empty()))
        .filter(|status| status.expires_at.is_none_or(|at| at > now))
}

/// Newest `created_at` among the author's valid status events, cleared or
/// expired ones included: a save must replace it.
pub fn latest_created_at(events: &[Verified], author: &str) -> Option<u64> {
    events
        .iter()
        .filter(|event| event.pubkey_hex() == author && parse(event).is_some())
        .map(Verified::created_at_secs)
        .max()
}

/// Parses a status event (`user-status.ts:17-41`). Events with an `h` tag,
/// or without exactly one `d` tag of `general`, are not statuses.
pub fn parse(event: &Verified) -> Option<UserStatus> {
    let d: Vec<&[String]> = event.tags_named("d").collect();
    if event.kind_u16() != KIND
        || d.len() != 1
        || d[0].get(1).map(String::as_str) != Some("general")
        || event.tags_named("h").next().is_some()
    {
        return None;
    }
    let emoji = event.tag("emoji").unwrap_or_default();
    if !valid_text(&event.content, emoji) {
        return None;
    }
    let expires_at = match event.tag("expiration") {
        Some(value) => Some(value.parse::<u64>().ok()?),
        None => None,
    };
    Some(UserStatus {
        text: event.content.trim().to_string(),
        emoji: emoji.trim().to_string(),
        updated_at: event.created_at_secs(),
        expires_at,
    })
}

/// The event that saves `input` (§5j.1), dated after `previous` (the
/// author's newest status event) so it replaces it.
pub fn template(
    input: &StatusInput,
    previous: Option<u64>,
    now: u64,
) -> Result<EventTemplate, StatusError> {
    let text = input.text.trim();
    let emoji = input.emoji.trim();
    if !valid_text(text, emoji) {
        return Err(StatusError::TooLong);
    }
    let expires_at = if text.is_empty() && emoji.is_empty() {
        None
    } else {
        input.expires_at
    };
    if expires_at.is_some_and(|at| at <= now) {
        return Err(StatusError::Expired);
    }
    let created_at = now.max(previous.map_or(0, |at| at + 1));
    if created_at > now + MAX_SKEW_SECS {
        return Err(StatusError::ClockSkew);
    }
    let mut tags = vec![vec!["d".to_string(), "general".to_string()]];
    if !emoji.is_empty() {
        tags.push(vec!["emoji".into(), emoji.into()]);
    }
    if let Some(at) = expires_at {
        tags.push(vec!["expiration".into(), at.to_string()]);
    }
    Ok(EventTemplate {
        kind: KIND,
        created_at,
        tags,
        content: text.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Verifier;
    use nostr::prelude::{EventBuilder, FinalizeEvent, Keys, Kind, Tag, Timestamp};

    fn event(keys: &Keys, content: &str, at: u64, tags: &[&[&str]]) -> Verified {
        let tags = tags
            .iter()
            .map(|tag| Tag::parse(tag.iter().copied()).unwrap())
            .collect::<Vec<_>>();
        let event = EventBuilder::new(Kind::from_u16(KIND), content)
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(at))
            .finalize(keys)
            .unwrap();
        Verifier::default().verify(event).unwrap()
    }

    #[test]
    fn builds_the_reference_shape() {
        let input = StatusInput {
            text: "  In a meeting ".into(),
            emoji: "📅".into(),
            expires_at: Some(1_760_028_800),
        };
        let template = template(&input, None, 1_760_000_000).unwrap();
        assert_eq!(template.kind, 30315);
        assert_eq!(template.content, "In a meeting");
        assert_eq!(template.created_at, 1_760_000_000);
        assert_eq!(
            template.tags,
            vec![
                vec!["d".to_string(), "general".into()],
                vec!["emoji".into(), "📅".into()],
                vec!["expiration".into(), "1760028800".into()],
            ]
        );
    }

    #[test]
    fn clearing_drops_emoji_and_expiration() {
        let input = StatusInput {
            text: " ".into(),
            emoji: String::new(),
            expires_at: Some(5),
        };
        let template = template(&input, Some(99), 100).unwrap();
        assert_eq!(template.content, "");
        assert_eq!(template.tags, vec![vec!["d".to_string(), "general".into()]]);
    }

    #[test]
    fn dates_after_the_previous_status() {
        let input = StatusInput::default();
        assert_eq!(template(&input, Some(100), 100).unwrap().created_at, 101);
        assert_eq!(template(&input, Some(50), 100).unwrap().created_at, 100);
        assert_eq!(
            template(&input, Some(400), 100),
            Err(StatusError::ClockSkew)
        );
    }

    #[test]
    fn refuses_long_multiline_and_past_statuses() {
        let long = StatusInput {
            text: "x".repeat(101),
            ..Default::default()
        };
        assert_eq!(template(&long, None, 1), Err(StatusError::TooLong));
        let lines = StatusInput {
            text: "a\nb".into(),
            ..Default::default()
        };
        assert_eq!(template(&lines, None, 1), Err(StatusError::TooLong));
        let past = StatusInput {
            text: "hi".into(),
            expires_at: Some(10),
            ..Default::default()
        };
        assert_eq!(template(&past, None, 10), Err(StatusError::Expired));
    }

    #[test]
    fn reads_the_current_status() {
        let keys = Keys::generate();
        let me = keys.public_key().to_hex();
        let old = event(&keys, "old", 10, &[&["d", "general"]]);
        let new = event(
            &keys,
            "Commuting",
            20,
            &[&["d", "general"], &["emoji", "🚌"], &["expiration", "100"]],
        );
        let scoped = event(&keys, "nope", 30, &[&["d", "general"], &["h", "x"]]);
        let events = [old, new, scoped];
        let status = current(&events, &me, 50).unwrap();
        assert_eq!(status.text, "Commuting");
        assert_eq!(status.emoji, "🚌");
        assert_eq!(status.expires_at, Some(100));
        assert_eq!(latest_created_at(&events, &me), Some(20));
        // Expired.
        assert_eq!(current(&events, &me, 100), None);
        // Cleared.
        let cleared = event(&keys, "", 40, &[&["d", "general"]]);
        assert_eq!(current(&[events[1].clone(), cleared], &me, 50), None);
    }
}
