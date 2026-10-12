//! The Inbox's pure rules: view and sender filters, labels and times
//! (`InboxPage.tsx`, `InboxFilters.tsx`, `dm-label.ts`,
//! `relative-timestamp.ts`, `view-state.ts`).

use crate::ui::keys::format_public_key;
use buzz_relay_client::unread::InboxItem;

/// The "Show" choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    All,
    Threads,
    Mentions,
    Dms,
    Drafts,
}

impl View {
    pub const ALL: [View; 5] = [
        View::All,
        View::Threads,
        View::Mentions,
        View::Dms,
        View::Drafts,
    ];

    pub fn label(self) -> &'static str {
        match self {
            View::All => "All",
            View::Threads => "Threads",
            View::Mentions => "Mentions",
            View::Dms => "DMs",
            View::Drafts => "Drafts",
        }
    }
}

/// The "From" choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sender {
    #[default]
    Everyone,
    Humans,
    Agents,
}

/// What a sender is known to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Human,
    Agent,
    Unknown,
}

/// The filter trigger's text: "All", or "Mentions · Agents".
pub fn summary(view: View, sender: Sender) -> String {
    let label = view.label();
    match (view, sender) {
        (View::Drafts, _) | (_, Sender::Everyone) => label.to_owned(),
        (_, Sender::Humans) => format!("{label} · People"),
        (_, Sender::Agents) => format!("{label} · Agents"),
    }
}

/// The From checkbox `people` (or agents) changed to `checked`; the other
/// kind can never be unchecked as well.
pub fn toggle_sender(current: Sender, people: bool, checked: bool) -> Sender {
    match (people, checked, current) {
        (_, true, _) => Sender::Everyone,
        (true, false, Sender::Everyone) => Sender::Agents,
        (false, false, Sender::Everyone) => Sender::Humans,
        (_, false, current) => current,
    }
}

pub fn matches_activity(item: &InboxItem, view: View, dm: bool) -> bool {
    match view {
        View::All | View::Drafts => true,
        View::Dms => dm,
        View::Mentions => item.mentioned,
        View::Threads => item.thread,
    }
}

pub fn has_unread(item: &InboxItem) -> bool {
    item.unread_count > 0 || item.manual
}

/// One label for DM rows, details and drafts: the first three people,
/// then "+N".
pub fn dm_label(
    participants: Option<&[String]>,
    name: impl Fn(&str, &str) -> String,
    missing: &str,
) -> String {
    let Some(participants) = participants else {
        return missing.to_owned();
    };
    if participants.is_empty() {
        return "Notes to self".into();
    }
    let mut label = participants
        .iter()
        .take(3)
        .map(|key| {
            let fallback = format_public_key(key).unwrap_or_else(|| "Unknown person".into());
            name(key, &fallback)
        })
        .collect::<Vec<_>>()
        .join(", ");
    if participants.len() > 3 {
        label.push_str(&format!(" +{}", participants.len() - 3));
    }
    label
}

/// `relativeTimestamp`: "just now", "5 minutes ago", … then "on Oct 3".
pub fn relative_timestamp(at: u64, now: u64) -> String {
    let age = now.saturating_sub(at);
    let ago =
        |value: u64, unit: &str| format!("{value} {unit}{} ago", if value == 1 { "" } else { "s" });
    match age {
        0..60 => "just now".into(),
        60..3600 => ago(age / 60, "minute"),
        3600..86_400 => ago(age / 3600, "hour"),
        86_400..604_800 => ago(age / 86_400, "day"),
        _ => format!("on {}", super::super::time::short_month_day(at)),
    }
}

/// A composer draft's place (`draftCoordinates`): `draft:<channel>` or
/// `draft:<channel>:thread:<root>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coordinates {
    pub channel_id: String,
    pub thread_root: Option<String>,
}

pub fn draft_coordinates(key: &str) -> Option<Coordinates> {
    let rest = key.strip_prefix("draft:")?;
    let (channel, thread) = match rest.split_once(":thread:") {
        Some((channel, root)) => (channel, Some(root)),
        None => (rest, None),
    };
    let mut chars = channel.chars();
    let first = chars.next()?;
    let valid_channel = first.is_ascii_alphanumeric()
        && channel.len() <= 256
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let valid_root = thread.is_none_or(|root| {
        root.len() == 64 && root.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    (valid_channel && valid_root).then(|| Coordinates {
        channel_id: channel.to_owned(),
        thread_root: thread.map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn senders_never_both_unchecked() {
        assert_eq!(toggle_sender(Sender::Everyone, true, false), Sender::Agents);
        assert_eq!(toggle_sender(Sender::Agents, false, false), Sender::Agents);
        assert_eq!(toggle_sender(Sender::Agents, true, true), Sender::Everyone);
        assert_eq!(summary(View::Mentions, Sender::Humans), "Mentions · People");
        assert_eq!(summary(View::Drafts, Sender::Agents), "Drafts");
    }

    #[test]
    fn dm_labels_name_three_then_count() {
        let keys: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        let name = |key: &str, _: &str| key.to_uppercase();
        assert_eq!(dm_label(Some(&keys), name, "x"), "A, B, C +2");
        assert_eq!(dm_label(Some(&[]), name, "x"), "Notes to self");
        assert_eq!(dm_label(None, name, "Direct message"), "Direct message");
    }

    #[test]
    fn times_read_like_buzz_app() {
        assert_eq!(relative_timestamp(100, 130), "just now");
        assert_eq!(relative_timestamp(0, 60), "1 minute ago");
        assert_eq!(relative_timestamp(0, 7200), "2 hours ago");
        assert_eq!(relative_timestamp(0, 86_400 * 3), "3 days ago");
    }

    #[test]
    fn draft_keys_name_their_place() {
        let root = "a".repeat(64);
        assert_eq!(
            draft_coordinates(&format!("draft:c1:thread:{root}")),
            Some(Coordinates {
                channel_id: "c1".into(),
                thread_root: Some(root),
            })
        );
        assert!(draft_coordinates("draft:-bad").is_none());
        assert!(draft_coordinates("draft:c1:thread:xyz").is_none());
        assert!(draft_coordinates("selected-channel").is_none());
    }
}
