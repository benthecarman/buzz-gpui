//! Desktop notifications for new messages (relay.md §5l).
//!
//! The session decides what deserves attention ([`Session::incoming`]);
//! this module applies the user's side: only the selected community, the
//! community's preferences, channel visibility, a two-minute dedupe, and
//! the reference text format. Banners go to the freedesktop notification
//! service on a worker thread, silent and fire-and-forget: a missing
//! service is logged, never retried and never fatal.
//!
//! [`Session::incoming`]: buzz_relay_client::Session::incoming

use crate::community::{AppState, Community};
use crate::runtime::Runtime;
use buzz_relay_client::{
    Category, ChannelSummary, ChannelType, IncomingMessage, Partition, Profiles, Store,
};
use futures::StreamExt;
use gpui_kit::{App, Entity, Global, Task};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// View-state key for a community's notification preferences.
const PREFERENCES: &str = "notifications";
/// A message notifies at most once within this window.
const DEDUPE: Duration = Duration::from_secs(120);
const DEDUPE_ENTRIES: usize = 2048;
const TITLE_NAME_CHARS: usize = 64;
const BODY_CHARS: usize = 200;
const SOURCE_CHARS: usize = 4096;

/// Per-community choices (`buzz-notification-preferences.v1`, without
/// sounds: banners are always silent here).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub enabled: bool,
    pub mention: bool,
    pub direct: bool,
    pub thread: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            mention: true,
            direct: true,
            thread: true,
        }
    }
}

impl Preferences {
    pub fn load(store: &Store, partition: &Partition) -> Self {
        store
            .view_state(partition, PREFERENCES)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    #[allow(dead_code)] // Settings will call this.
    pub fn save(&self, store: &Store, partition: &Partition) {
        let json = serde_json::to_string(self).expect("preferences serialize");
        if let Err(error) = store.set_view_state(partition, PREFERENCES, Some(&json)) {
            tracing::warn!(%error, "could not save notification preferences");
        }
    }

    fn allows(&self, category: Category) -> bool {
        self.enabled
            && match category {
                Category::Mention => self.mention,
                Category::Direct => self.direct,
                Category::Thread => self.thread,
            }
    }
}

/// Recently shown message ids, shared by every community.
#[derive(Default)]
struct Shown(HashMap<(String, String), Instant>);

impl Global for Shown {}

impl Shown {
    /// Records `key`; false if it was shown within [`DEDUPE`].
    fn admit(&mut self, key: (String, String), now: Instant) -> bool {
        self.0.retain(|_, at| now.duration_since(*at) < DEDUPE);
        if self.0.contains_key(&key) || self.0.len() >= DEDUPE_ENTRIES {
            return false;
        }
        self.0.insert(key, now);
        true
    }
}

/// Forwards `community`'s incoming messages to [`deliver`] until either
/// goes away.
pub fn listen(community: &Entity<Community>, cx: &mut App) -> Task<()> {
    let mut receiver = community.read(cx).session.incoming();
    let (sender, mut items) = futures::channel::mpsc::unbounded();
    Runtime::handle(cx).spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(item) => {
                    if sender.unbounded_send(item).is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    let community = community.downgrade();
    cx.spawn(async move |cx| {
        while let Some(item) = items.next().await {
            let Some(community) = community.upgrade() else {
                return;
            };
            cx.update(|cx| deliver(&community, &item, cx));
        }
    })
}

fn deliver(community: &Entity<Community>, item: &IncomingMessage, cx: &mut App) {
    // Like the reference, only the selected community notifies.
    if AppState::current(cx).is_none_or(|current| current != *community) {
        return;
    }
    let this = community.read(cx);
    // Already in front of the user.
    if this.reading.as_deref() == Some(item.channel_id.as_str()) && cx.active_window().is_some() {
        return;
    }
    let store = cx.global::<AppState>().store.clone();
    if !Preferences::load(&store, &this.partition()).allows(item.category) {
        return;
    }
    let key = (this.origin.to_string(), item.message_id.to_hex());
    let channel = this
        .snapshot
        .channels
        .iter()
        .find(|channel| channel.id == item.channel_id)
        .cloned();
    let (title, body) = text(item, channel.as_ref(), &this.snapshot.profiles);
    if !cx.default_global::<Shown>().admit(key, Instant::now()) {
        return;
    }
    show(title, body);
}

/// Title and body (`content.ts`).
fn text(
    item: &IncomingMessage,
    channel: Option<&ChannelSummary>,
    profiles: &Profiles,
) -> (String, String) {
    let sender_key = item.workflow_owner.as_deref().unwrap_or(&item.author);
    let name = profiles
        .get(sender_key)
        .filter(|profile| !profile.name_is_fallback)
        .map(|profile| short_text(&profile.name, TITLE_NAME_CHARS))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| sender_key.chars().take(10).collect());
    let sender = match item.workflow_owner {
        Some(_) => format!("Workflow · owned by {name}"),
        None => name,
    };
    let destination = match channel {
        Some(channel) if channel.channel_type == Some(ChannelType::Dm) => {
            "a direct message".to_string()
        }
        _ => {
            let name = channel
                .map(|channel| short_text(&channel.name, TITLE_NAME_CHARS))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| item.channel_id.chars().take(8).collect());
            format!("#{name}")
        }
    };
    let title = match item.category {
        Category::Mention => format!("{sender} mentioned you in {destination}"),
        Category::Direct => format!("{sender} sent you a direct message"),
        Category::Thread => format!("{sender} replied in {destination}"),
    };
    let body = plain_text(&item.preview);
    let body = if body.is_empty() {
        "New message".to_string()
    } else {
        body
    };
    (title, body)
}

/// One line without control characters, at most `limit` code points with
/// an ellipsis.
fn short_text(value: &str, limit: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let points: Vec<char> = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    if points.len() > limit {
        let mut text: String = points[..limit - 1].iter().collect();
        text.push('…');
        text
    } else {
        points.into_iter().collect()
    }
}

/// CommonMark flattened to prose: HTML and definitions dropped, images
/// labelled, at most [`BODY_CHARS`] code points.
fn plain_text(content: &str) -> String {
    let source: String = content.chars().take(SOURCE_CHARS).collect();
    let Ok(tree) = markdown::to_mdast(&source, &markdown::ParseOptions::default()) else {
        return short_text(&source, BODY_CHARS);
    };
    short_text(&prose(&tree), BODY_CHARS)
}

fn prose(node: &markdown::mdast::Node) -> String {
    use markdown::mdast::Node;
    match node {
        Node::Html(_) | Node::Definition(_) => String::new(),
        Node::Image(_) | Node::ImageReference(_) => "[Image]".into(),
        Node::Break(_) | Node::ThematicBreak(_) => " ".into(),
        Node::Text(text) => text.value.clone(),
        Node::InlineCode(code) => code.value.clone(),
        Node::Code(code) => code.value.clone(),
        Node::InlineMath(math) => math.value.clone(),
        Node::Math(math) => math.value.clone(),
        _ => {
            let separator = match node {
                Node::Paragraph(_)
                | Node::Heading(_)
                | Node::Emphasis(_)
                | Node::Strong(_)
                | Node::Link(_)
                | Node::LinkReference(_) => "",
                _ => " ",
            };
            node.children()
                .map(|children| {
                    children
                        .iter()
                        .map(prose)
                        .collect::<Vec<_>>()
                        .join(separator)
                })
                .unwrap_or_default()
        }
    }
}

/// Submits a banner on the notification worker, so a slow or missing
/// notification service never blocks the UI.
fn show(title: String, body: String) {
    use std::sync::OnceLock;
    use std::sync::mpsc::{Sender, channel};
    static WORKER: OnceLock<Option<Sender<(String, String)>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (sender, banners) = channel::<(String, String)>();
        std::thread::Builder::new()
            .name("buzz-notify".into())
            .spawn(move || {
                for (title, body) in banners {
                    submit(&title, &body);
                }
            })
            .map_err(|error| tracing::warn!(%error, "could not start notifications"))
            .ok()
            .map(|_| sender)
    });
    if let Some(sender) = worker {
        let _ = sender.send((title, body));
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn submit(title: &str, body: &str) {
    let result = notify_rust::Notification::new()
        .appname("Buzz")
        .icon("buzz-gpui")
        .summary(title)
        .body(body)
        .hint(notify_rust::Hint::SuppressSound(true))
        .show();
    if let Err(error) = result {
        tracing::warn!(%error, "could not show a notification");
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn submit(title: &str, _body: &str) {
    tracing::debug!(%title, "desktop notifications are not supported on this platform");
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_relay_client::Profile;
    use nostr::prelude::EventId;
    use std::sync::Arc;

    fn item(category: Category, preview: &str) -> IncomingMessage {
        IncomingMessage {
            channel_id: "0e9d217a-2d7e".into(),
            message_id: EventId::from_byte_array([0; 32]),
            created_at: 0,
            author: "ab".repeat(32),
            workflow_owner: None,
            preview: preview.into(),
            category,
            mentioned: category == Category::Mention,
            thread_root: None,
        }
    }

    fn channel(name: &str, channel_type: ChannelType) -> ChannelSummary {
        ChannelSummary {
            id: "0e9d217a-2d7e".into(),
            name: name.into(),
            channel_type: Some(channel_type),
            ..Default::default()
        }
    }

    fn named(name: &str) -> Profiles {
        let profile = Profile {
            name: name.into(),
            ..Default::default()
        };
        Arc::new(HashMap::from([("ab".repeat(32), Arc::new(profile))]))
    }

    #[test]
    fn titles_follow_the_reference() {
        let general = channel("general", ChannelType::Stream);
        let dm = channel("bob", ChannelType::Dm);
        let profiles = named("Bob");
        assert_eq!(
            text(&item(Category::Mention, "hi"), Some(&general), &profiles).0,
            "Bob mentioned you in #general"
        );
        assert_eq!(
            text(&item(Category::Mention, "hi"), Some(&dm), &profiles).0,
            "Bob mentioned you in a direct message"
        );
        assert_eq!(
            text(&item(Category::Direct, "hi"), Some(&dm), &profiles).0,
            "Bob sent you a direct message"
        );
        assert_eq!(
            text(&item(Category::Thread, "hi"), Some(&general), &profiles).0,
            "Bob replied in #general"
        );
        // Unknown names and channels fall back to key fragments.
        assert_eq!(
            text(&item(Category::Thread, "hi"), None, &Profiles::default()).0,
            "ababababab replied in #0e9d217a"
        );
        let mut workflow = item(Category::Mention, "hi");
        workflow.workflow_owner = Some("ab".repeat(32));
        assert_eq!(
            text(&workflow, Some(&general), &profiles).0,
            "Workflow · owned by Bob mentioned you in #general"
        );
    }

    #[test]
    fn bodies_are_flattened_and_bounded() {
        assert_eq!(
            plain_text("**Hello** _there_\n\nsecond"),
            "Hello there second"
        );
        assert_eq!(plain_text("look ![cat](https://x/cat.png)"), "look [Image]");
        assert_eq!(plain_text("<b>raw</b> text"), "raw text");
        let long = "word ".repeat(100);
        let body = plain_text(&long);
        assert_eq!(body.chars().count(), BODY_CHARS);
        assert!(body.ends_with('…'));
        assert_eq!(
            text(&item(Category::Direct, ""), None, &Profiles::default()).1,
            "New message"
        );
    }

    #[test]
    fn dedupes_for_two_minutes() {
        let mut shown = Shown::default();
        let now = Instant::now();
        let key = ("ws://relay".to_string(), "id".to_string());
        assert!(shown.admit(key.clone(), now));
        assert!(!shown.admit(key.clone(), now + Duration::from_secs(119)));
        assert!(shown.admit(key, now + DEDUPE));
    }

    #[test]
    fn preferences_gate_categories() {
        let mut preferences = Preferences::default();
        assert!(preferences.allows(Category::Thread));
        preferences.thread = false;
        assert!(!preferences.allows(Category::Thread));
        assert!(preferences.allows(Category::Mention));
        preferences.enabled = false;
        assert!(!preferences.allows(Category::Mention));
        // Missing fields keep their defaults.
        let parsed: Preferences = serde_json::from_str(r#"{"direct":false}"#).unwrap();
        assert!(parsed.enabled && !parsed.direct);
    }
}
