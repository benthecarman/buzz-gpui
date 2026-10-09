//! `buzz://` links from message bodies, the OS and other instances
//! (`docs/deep-links.md` in buzz-app).
//!
//! Only the link forms are parsed here. A link carries no community, so a
//! conversation binds to the selected one, and a valid address is never
//! authorization: the session still decides what the viewer may read.

use buzz_relay_client::{EventId, RelayOrigin, parse_buzz_link};
use url::Url;

/// What a `buzz://` link asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// `buzz://channel/<id>[/<event>]` or
    /// `buzz://message?channel=<id>&id=<event>[&thread=<root>]`. The thread
    /// is a hint only; the root is re-derived from signed ancestry.
    Conversation {
        channel: String,
        message: Option<EventId>,
        thread: Option<EventId>,
    },
    /// `buzz://join?relay=<url>&code=<code>`.
    Join { relay: RelayOrigin, code: String },
}

/// Parses one of the accepted forms; anything else, unknown hosts included,
/// is `None`.
pub fn parse(href: &str) -> Option<Link> {
    if !href.starts_with("buzz:") {
        return None;
    }
    if let Some(link) = parse_buzz_link(href) {
        return Some(Link::Conversation {
            channel: link.channel,
            message: link.message,
            thread: link.thread,
        });
    }
    join(href)
}

/// `parseInviteLink`: one relay, one code of at most 256 characters, at
/// most one policy receipt; other parameters are ignored.
fn join(href: &str) -> Option<Link> {
    if href.len() > 4096 || !(href.starts_with("buzz://join?") || href.starts_with("buzz://join/?"))
    {
        return None;
    }
    let url = Url::parse(href).ok()?;
    if url.host_str() != Some("join")
        || !matches!(url.path(), "" | "/")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let all = |key: &str| {
        url.query_pairs()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
            .collect::<Vec<_>>()
    };
    let (relay, code, receipts) = (all("relay"), all("code"), all("policy_receipt"));
    if relay.len() != 1
        || code.len() != 1
        || receipts.len() > 1
        || code[0].is_empty()
        || code[0].chars().count() > 256
        || receipts.first().is_some_and(String::is_empty)
    {
        return None;
    }
    let relay = RelayOrigin::parse(&relay[0]).ok()?;
    Some(Link::Join {
        relay,
        code: code[0].clone(),
    })
}

/// The `buzz:` URL on a command line, if any. The newest intent wins, so
/// the last one counts.
pub fn from_args(args: impl IntoIterator<Item = String>) -> Option<String> {
    args.into_iter()
        .filter(|arg| arg.len() <= 32_768 && arg.starts_with("buzz:"))
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> EventId {
        EventId::from_byte_array([byte; 32])
    }

    #[test]
    fn parses_conversation_forms() {
        let message = id(1).to_hex();
        let root = id(2).to_hex();
        assert_eq!(
            parse("buzz://channel/general"),
            Some(Link::Conversation {
                channel: "general".into(),
                message: None,
                thread: None,
            })
        );
        assert_eq!(
            parse(&format!("buzz://channel/general/{message}")),
            Some(Link::Conversation {
                channel: "general".into(),
                message: Some(id(1)),
                thread: None,
            })
        );
        assert_eq!(
            parse(&format!(
                "buzz://message?channel=0e9d-217a&id={message}&thread={root}"
            )),
            Some(Link::Conversation {
                channel: "0e9d-217a".into(),
                message: Some(id(1)),
                thread: Some(id(2)),
            })
        );
        // Uppercase hex is accepted and normalized.
        assert_eq!(
            parse(&format!(
                "buzz://message?channel=c&id={}",
                message.to_uppercase()
            )),
            Some(Link::Conversation {
                channel: "c".into(),
                message: Some(id(1)),
                thread: None,
            })
        );
    }

    #[test]
    fn rejects_malformed_and_unknown_links() {
        let message = id(1).to_hex();
        for bad in [
            "buzz://channel/-bad".to_string(),
            "buzz://channel/a/b/c".to_string(),
            "buzz://message?channel=a".to_string(),
            format!("buzz://message?channel=a&id={message}&id={message}"),
            format!("buzz://message?channel=a&id={message}#x"),
            "buzz://repo?owner=x".to_string(),
            "buzz://settings".to_string(),
            "https://channel/general".to_string(),
            "buzz:".to_string(),
            String::new(),
        ] {
            assert_eq!(parse(&bad), None, "{bad}");
        }
    }

    #[test]
    fn parses_invites() {
        let relay = RelayOrigin::parse("wss://relay.example.com").unwrap();
        assert_eq!(
            parse("buzz://join?relay=wss%3A%2F%2Frelay.example.com&code=abc"),
            Some(Link::Join {
                relay: relay.clone(),
                code: "abc".into(),
            })
        );
        // Windows supplies a root path; unknown parameters are ignored.
        assert_eq!(
            parse("buzz://join/?relay=https://relay.example.com/&code=abc&x=1&x=2"),
            Some(Link::Join {
                relay,
                code: "abc".into(),
            })
        );
        for bad in [
            "buzz://join?relay=example&code=abc",
            "buzz://join?relay=wss://relay.example.com",
            "buzz://join?relay=wss://relay.example.com&code=",
            "buzz://join?relay=wss://a.example&relay=wss://b.example&code=abc",
            "buzz://join?relay=wss://relay.example.com&code=abc&policy_receipt=",
            "buzz://join/extra?relay=wss://relay.example.com&code=abc",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
        let long = format!(
            "buzz://join?relay=wss://relay.example.com&code={}",
            "a".repeat(257)
        );
        assert_eq!(parse(&long), None);
    }

    #[test]
    fn takes_the_last_link_argument() {
        let args = [
            "buzz-gpui",
            "buzz://channel/a",
            "--flag",
            "buzz://channel/b",
        ]
        .map(String::from);
        assert_eq!(from_args(args), Some("buzz://channel/b".into()));
        assert_eq!(from_args(["buzz-gpui".to_string()]), None);
    }
}
