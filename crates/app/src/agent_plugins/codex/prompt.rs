//! What Codex is told (buzz-app's `prompt.ts`): the Buzz base prompt as
//! developer instructions, which keep Codex's own coding prompt, and each
//! request as quoted event data. `base_prompt.md` is buzz-app's, copied
//! unchanged.

use super::config::Config;
use crate::agent_plugins::claude_code::prompt::escaped;
use crate::buzz_mcp::BuzzEvent;
use serde::Serialize;

const BASE_PROMPT: &str = include_str!("base_prompt.md");

/// Where a request belongs: a channel, and the thread when each thread is
/// its own session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversation {
    pub channel_id: String,
    pub name: String,
    pub root: Option<String>,
}

/// JSON with `<` and `>` escaped, so quoted data cannot close a section.
pub fn quoted(value: &impl Serialize) -> String {
    escaped(&serde_json::to_string(value).unwrap_or_default())
}

pub(crate) use crate::buzz_mcp::tools::root_of;

/// The Buzz guidance; with `settings`, the agent's custom instructions too.
pub fn developer_instructions(name: &str, pubkey: &str, settings: Option<&Config>) -> String {
    let custom = settings.map_or_else(String::new, |settings| {
        format!(
            "Current Buzz custom instructions replace all prior Buzz custom instructions. An empty section means no custom instructions apply.\n<agent-instructions>\n{}\n</agent-instructions>",
            settings.instructions
        )
    });
    format!(
        "You are {name} (pubkey {pubkey}), a coding agent in Buzz.
The buzz namespace provides the shared Buzz tools, including messages, reactions, reads, uploads, canvas, and encrypted memory. Buzz access and signing are handled by the host. Do not invoke the buzz CLI or look for credentials. Use Codex's native coding tools in the workspace.

{base}

{buzz}

{custom}",
        base = BASE_PROMPT.trim(),
        buzz = crate::buzz_mcp::BUZZ_PROMPT.trim(),
    )
}

#[derive(Serialize)]
struct Context<'a> {
    channel_id: &'a str,
    channel_name: &'a str,
    session_scope: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_thread: Option<&'a str>,
    reply_to: String,
}

/// One request as Codex reads it; `steering` marks a follow-up to work in
/// progress.
pub fn turn_input(
    event: &BuzzEvent,
    conversation: &Conversation,
    text: &str,
    interest: &str,
    steering: bool,
) -> String {
    let section = if steering {
        "new-message-arrived-while-you-were-working"
    } else {
        "buzz-event"
    };
    let context = Context {
        channel_id: &conversation.channel_id,
        channel_name: &conversation.name,
        session_scope: if conversation.root.is_some() {
            "thread"
        } else {
            "channel"
        },
        session_thread: conversation.root.as_deref(),
        reply_to: root_of(event),
    };
    let interest = if interest.is_empty() {
        String::new()
    } else {
        format!("\n<interest>\n{}\n</interest>", quoted(&interest))
    };
    format!(
        "<context>\n{}\n</context>\n<{section}>\n{}\nRequest: {}\n</{section}>{interest}",
        quoted(&context),
        quoted(event),
        quoted(&text),
    )
}

/// The Codex thread's name, so a session started in Buzz is easy to find:
/// its [`session_title`], then its [`session_tag`].
pub fn session_name(agent: &str, pubkey: &str, conversation: &Conversation) -> String {
    format!(
        "{} · {}",
        session_title(agent, conversation),
        session_tag(
            pubkey,
            &conversation.channel_id,
            conversation.root.as_deref()
        )
    )
}

/// Names the agent's conversation in its thread names, so its threads can
/// be found from any copy of the app, each of which saves its own
/// bindings: `buzz:` and a 64-bit FNV-1a hash of the agent and the
/// conversation.
pub fn session_tag(pubkey: &str, channel_id: &str, root: Option<&str>) -> String {
    let key =
        serde_json::to_string(&[pubkey, channel_id, root.unwrap_or("channel")]).unwrap_or_default();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("buzz:{hash:016x}")
}

/// The readable part of a conversation's thread name, which earlier
/// versions used alone: `Buzz #<channel> · thread <root prefix> · <agent>`.
pub fn session_title(agent: &str, conversation: &Conversation) -> String {
    // Each run of whitespace becomes one space, as `/\s+/g` does.
    let mut channel = String::new();
    for char in conversation.name.chars() {
        if !char.is_whitespace() {
            channel.push(char);
        } else if !channel.ends_with(' ') {
            channel.push(' ');
        }
    }
    let channel: String = channel.chars().take(100).collect();
    let thread = conversation.root.as_ref().map_or_else(String::new, |root| {
        format!(" · thread {}", root.chars().take(8).collect::<String>())
    });
    format!("Buzz #{channel}{thread} · {agent}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(tags: Vec<Vec<String>>) -> BuzzEvent {
        BuzzEvent {
            id: "1".repeat(64),
            pubkey: "a".repeat(64),
            created_at: 10,
            kind: 9,
            tags,
            content: "@Codex <b>hi</b>".into(),
            sig: String::new(),
        }
    }

    #[test]
    fn quotes_requests_with_their_conversation() {
        let root = "c".repeat(64);
        let reply = event(vec![
            vec!["h".into(), "chan".into()],
            vec!["e".into(), root.clone(), String::new(), "reply".into()],
        ]);
        let thread = Conversation {
            channel_id: "chan".into(),
            name: "general".into(),
            root: Some(root.clone()),
        };
        let input = turn_input(&reply, &thread, "<b>hi</b>", "", false);
        assert!(input.starts_with(&format!(
            "<context>\n{{\"channel_id\":\"chan\",\"channel_name\":\"general\",\"session_scope\":\"thread\",\"session_thread\":\"{root}\",\"reply_to\":\"{root}\"}}\n</context>\n<buzz-event>\n"
        )));
        assert!(input.ends_with("\nRequest: \"\\u003cb\\u003ehi\\u003c/b\\u003e\"\n</buzz-event>"));
        assert!(!input.contains("<b>"));
        let channel = Conversation {
            root: None,
            ..thread
        };
        let steer = turn_input(&reply, &channel, "more", "Watch builds", true);
        assert!(steer.contains("\"session_scope\":\"channel\""));
        assert!(!steer.contains("session_thread"));
        assert!(steer.contains("<new-message-arrived-while-you-were-working>\n"));
        assert!(steer.ends_with("\n<interest>\n\"Watch builds\"\n</interest>"));
        // A top-level request is its own thread.
        let top = event(vec![vec!["h".into(), "chan".into()]]);
        assert!(
            turn_input(&top, &channel, "x", "", false)
                .contains(&format!("\"reply_to\":\"{}\"", "1".repeat(64)))
        );
    }

    #[test]
    fn names_sessions_and_supplies_custom_instructions() {
        let conversation = Conversation {
            channel_id: "chan".into(),
            name: " team\n  chat ".into(),
            root: Some("abcdef0123456789".into()),
        };
        assert_eq!(
            session_title("Codex Test", &conversation),
            "Buzz # team chat  · thread abcdef01 · Codex Test"
        );
        let tag = session_tag("b", "chan", Some("abcdef0123456789"));
        assert_eq!(
            session_name("Codex Test", "b", &conversation),
            format!("Buzz # team chat  · thread abcdef01 · Codex Test · {tag}")
        );
        let channel = Conversation {
            root: None,
            name: "x".repeat(120),
            ..conversation
        };
        assert_eq!(
            session_title("C", &channel),
            format!("Buzz #{} · C", "x".repeat(100))
        );
        // Stable across copies of the app, and apart per agent and place.
        assert_eq!(tag, session_tag("b", "chan", Some("abcdef0123456789")));
        assert!(tag.starts_with("buzz:") && tag.len() == 21);
        assert_ne!(tag, session_tag("c", "chan", Some("abcdef0123456789")));
        assert_ne!(
            session_tag("b", "chan", None),
            session_tag("b", "chan", Some("channel2"))
        );
        // FNV-1a over the JSON key, as buzz-app hashes it.
        assert_eq!(tag, "buzz:118714305d2147cb");
        let base = developer_instructions("Codex", "b", None);
        assert!(base.starts_with("You are Codex (pubkey b), a coding agent in Buzz."));
        assert!(!base.contains("<agent-instructions>"));
        let settings = Config {
            instructions: "Use a marker".into(),
            ..Config::default()
        };
        assert!(
            developer_instructions("Codex", "b", Some(&settings))
                .ends_with("<agent-instructions>\nUse a marker\n</agent-instructions>")
        );
    }
}
