//! What "Copy message" and "Copy link" put on the clipboard
//! (`message-copy.ts`).

use super::identity::Names;
use buzz_relay_client::Profiles;
use buzz_relay_client::fold::{MessageRow, message_markdown};
use markdown::mdast::Node;
use nostr::nips::nip19::ToBech32;

/// `messageCopyLink`: `buzz://message?channel&id`, with `thread` for a
/// reply. `None` until the relay has the message.
pub fn link(row: &MessageRow) -> Option<String> {
    if !super::message::delivered(row) {
        return None;
    }
    let mut url = url::Url::parse("buzz://message").expect("static URL");
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("channel", &row.channel)
            .append_pair("id", &row.id.to_hex());
        if let Some(root) = row.thread_root_id {
            query.append_pair("thread", &root.to_hex());
        }
    }
    Some(url.to_string())
}

fn literal(node: &Node) -> bool {
    matches!(
        node,
        Node::Code(_)
            | Node::InlineCode(_)
            | Node::Link(_)
            | Node::LinkReference(_)
            | Node::Image(_)
            | Node::ImageReference(_)
            | Node::Definition(_)
            | Node::Html(_)
    )
}

fn literal_ranges(node: &Node, out: &mut Vec<(usize, usize)>) {
    if literal(node) {
        if let Some(position) = node.position() {
            out.push((position.start.offset, position.end.offset));
        }
        return;
    }
    for child in node.children().into_iter().flatten() {
        literal_ranges(child, out);
    }
}

/// `messageCopyText`: the body with bound mentions as `[@Name](nostr:npub…)`
/// links (never inside code or other literal syntax), then each
/// attachment's URL, blank lines between.
pub fn text(row: &MessageRow, profiles: &Profiles, names: &Names) -> String {
    let mut content = row.content.clone();
    if let Some(tree) = message_markdown(&content) {
        let mut ranges = Vec::new();
        literal_ranges(&tree, &mut ranges);
        content = bind_mentions(row, profiles, names, &ranges);
    }
    std::iter::once(content.as_str())
        .chain(row.attachments.iter().map(|a| a.url.as_str()))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `profileMentionParts` over the raw body, rewritten as links.
fn bind_mentions(
    row: &MessageRow,
    profiles: &Profiles,
    names: &Names,
    literals: &[(usize, usize)],
) -> String {
    let text = &row.content;
    if row.edited {
        return text.clone();
    }
    let mut labels: Vec<(String, Vec<String>)> = Vec::new();
    let library: std::collections::HashMap<&str, &str> = names.agent_names().collect();
    for key in row.mentions.iter().chain(&row.mention_references) {
        let hex = key.to_hex();
        let npub = key.to_bech32().expect("keys encode");
        let target = format!("nostr:{npub}");
        for name in [
            profiles.get(&hex).map(|p| p.name.clone()),
            library.get(hex.as_str()).map(|n| n.to_string()),
        ]
        .into_iter()
        .flatten()
        {
            if name.is_empty() || name.contains(['\r', '\n']) {
                continue;
            }
            match labels.iter_mut().find(|(n, _)| *n == name) {
                Some((_, targets)) => {
                    if !targets.contains(&target) {
                        targets.push(target.clone());
                    }
                }
                None => labels.push((name, vec![target.clone()])),
            }
        }
    }
    if labels.is_empty() {
        return text.clone();
    }
    labels.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    let seams = &row.attachment_seams;
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let previous = text[..i].chars().next_back();
        let boundary =
            !previous.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '@' | '/' | '\\'));
        if rest.starts_with('@')
            && boundary
            && let Some((name, targets)) = labels.iter().find(|(name, _)| {
                let end = i + 1 + name.len();
                rest[1..].starts_with(name.as_str())
                    && !text[end..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && !seams.iter().any(|s| i < *s && *s < end)
            })
        {
            let end = i + 1 + name.len();
            let inside = literals.iter().any(|&(s, e)| i < e && end > s);
            if targets.len() == 1 && !inside {
                out.push('[');
                for c in text[i..end].chars() {
                    if matches!(c, '\\' | '[' | ']') {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push_str("](");
                out.push_str(&targets[0]);
                out.push(')');
            } else {
                out.push_str(&text[i..end]);
            }
            i = end;
            continue;
        }
        let c = rest.chars().next().expect("not at the end");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::identity::{self, AgentLibrary};
    use buzz_relay_client::fold::{Attachment, AttachmentKind, Delivery, LocalDelivery};
    use buzz_relay_client::{EventId, Profile};
    use core::prelude::v1::test;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn copies_the_body_then_attachment_urls() {
        let mut row = super::super::markdown::tests::row("Look");
        let names = identity::names(
            &Profiles::default(),
            &AgentLibrary::default(),
            "",
            None,
            None,
        );
        assert_eq!(text(&row, &Profiles::default(), &names), "Look");
        row.attachments = vec![Attachment {
            url: "https://x.test/media/a.png".into(),
            kind: AttachmentKind::Image,
            mime: None,
            size: None,
            name: None,
            duration: None,
            waveform: None,
            voice_note: false,
            dimensions: None,
            blurhash: None,
            preview_url: None,
            insecure: false,
        }];
        assert_eq!(
            text(&row, &Profiles::default(), &names),
            "Look\n\nhttps://x.test/media/a.png"
        );
    }

    #[test]
    fn bound_mentions_copy_as_identity_links_outside_code() {
        let sam = buzz_identity::Identity::ephemeral().public_key();
        let profiles: Profiles = Arc::new(HashMap::from([(
            sam.to_hex(),
            Arc::new(Profile {
                name: "Sam".into(),
                ..Default::default()
            }),
        )]));
        let names = identity::names(&profiles, &AgentLibrary::default(), "", None, None);
        let mut row = super::super::markdown::tests::row("hi @Sam and `@Sam`");
        row.mentions = vec![sam];
        let npub = sam.to_bech32().unwrap();
        assert_eq!(
            text(&row, &profiles, &names),
            format!("hi [@Sam](nostr:{npub}) and `@Sam`")
        );
    }

    #[test]
    fn links_keep_the_thread_for_replies() {
        let mut row = super::super::markdown::tests::row("x");
        let root = EventId::from_byte_array([4; 32]);
        row.thread_root_id = Some(root);
        let link = link(&row).unwrap();
        assert!(link.contains(&format!("thread={}", root.to_hex())));
        row.delivery = Some(LocalDelivery {
            status: Delivery::Sending,
            error: None,
        });
        assert_eq!(super::link(&row), None);
    }
}
