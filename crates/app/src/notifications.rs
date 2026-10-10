//! Desktop notifications for new messages (relay.md §5l).
//!
//! The session decides what deserves attention ([`Session::incoming`]),
//! including channel mute; this module applies the user's side: only the
//! selected community, the viewer's preferences, the message being in
//! view, a two-minute dedupe, and the reference text format. Banners go to
//! the freedesktop notification service ([`banners`]) from the tokio
//! runtime, silent and never awaited by the UI; the chosen sound plays
//! once the service took the banner. A failure is kept for Settings →
//! Notifications to show. Clicking a banner brings the window forward on
//! the message it is about.
//!
//! [`Session::incoming`]: buzz_relay_client::Session::incoming

use crate::community::{AppState, Community, IdentityState};
use crate::runtime::Runtime;
use crate::sounds::Sound;
use crate::ui::navigation::{self, Place, Target};
use buzz_relay_client::{Category, ChannelSummary, ChannelType, IncomingMessage, Profiles, Store};
use futures::StreamExt;
use gpui_kit::{App, Entity, Global, Task};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod banners;
pub mod categories;

/// Viewer preference key for the notification choices
/// (`buzz-notification-preferences.v1:<viewer>`).
const PREFERENCES: &str = "notification-preferences";
/// A message notifies at most once within this window.
const DEDUPE: Duration = Duration::from_secs(120);
const DEDUPE_ENTRIES: usize = 2048;
const TITLE_NAME_CHARS: usize = 64;
const BODY_CHARS: usize = 200;
const SOURCE_CHARS: usize = 4096;

/// Shown when the saved choices could not be read; alerts stay off.
pub const RESTORE_ERROR: &str = "Notification preferences could not be restored. Alerts are \
                                 paused; retry loading your settings.";
/// Shown when a change could not be saved; it still applies.
pub const SAVE_ERROR: &str = "These notification choices are active, but could not be saved on \
                              this device. Retry saving.";

/// The viewer's choices, for every community. Banners are always silent;
/// the app plays the chosen sound itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub enabled: bool,
    pub mention: bool,
    pub direct: bool,
    pub thread: bool,
    /// "Notify while viewing": messages in view still notify.
    pub notify_while_viewing: bool,
    /// "Sound": alerts play their category's sound.
    pub sound: bool,
    pub sounds: CategorySounds,
    /// Plugin categories by key (`buzz.reminders/reminders`); a missing
    /// key is on.
    pub categories: std::collections::BTreeMap<String, bool>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            mention: true,
            direct: true,
            thread: true,
            notify_while_viewing: false,
            sound: true,
            sounds: CategorySounds::default(),
            categories: Default::default(),
        }
    }
}

/// The sound per category; buzz-app defaults each to flutter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CategorySounds {
    pub mention: Sound,
    pub direct: Sound,
    pub thread: Sound,
}

impl CategorySounds {
    pub fn get(&self, category: Category) -> Sound {
        match category {
            Category::Mention => self.mention,
            Category::Direct => self.direct,
            Category::Thread => self.thread,
        }
    }

    pub fn set(&mut self, category: Category, sound: Sound) {
        match category {
            Category::Mention => self.mention = sound,
            Category::Direct => self.direct = sound,
            Category::Thread => self.thread = sound,
        }
    }
}

impl Preferences {
    /// The saved choices of `viewer` (or of this device with none). Saved
    /// choices that cannot be read pause alerts rather than turn them on.
    pub fn restore(store: &Store, viewer: Option<&str>) -> Result<Self, Self> {
        let saved = match viewer {
            Some(viewer) => store.viewer_pref(viewer, PREFERENCES),
            None => store.device_pref(PREFERENCES),
        };
        let paused = || Self {
            enabled: false,
            ..Self::default()
        };
        match saved {
            Ok(None) => Ok(Self::default()),
            Ok(Some(json)) => serde_json::from_str(&json).map_err(|_| paused()),
            Err(_) => Err(paused()),
        }
    }

    pub fn save(&self, store: &Store, viewer: Option<&str>) -> Result<(), String> {
        let json = serde_json::to_string(self).expect("preferences serialize");
        match viewer {
            Some(viewer) => store.set_viewer_pref(viewer, PREFERENCES, &json),
            None => store.set_device_pref(PREFERENCES, &json),
        }
        .map_err(|error| error.to_string())
    }

    pub fn allows(&self, category: Category) -> bool {
        self.enabled && self.category(category)
    }

    /// The category's own switch, whatever Desktop alerts says.
    pub fn category(&self, category: Category) -> bool {
        match category {
            Category::Mention => self.mention,
            Category::Direct => self.direct,
            Category::Thread => self.thread,
        }
    }

    pub fn set_category(&mut self, category: Category, enabled: bool) {
        match category {
            Category::Mention => self.mention = enabled,
            Category::Direct => self.direct = enabled,
            Category::Thread => self.thread = enabled,
        }
    }

    /// The sound an alert in `category` plays, if any.
    pub fn sound_for(&self, category: Category) -> Option<Sound> {
        let sound = self.sounds.get(category);
        (self.allows(category) && self.sound && sound != Sound::Silent).then_some(sound)
    }
}

/// Whether the freedesktop notification service can show banners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServiceStatus {
    #[default]
    Unknown,
    Available,
    /// No service on the session bus.
    Unavailable,
}

/// The viewer's notification state (`NotificationsService` snapshot).
#[derive(Default)]
pub struct Notifications {
    viewer: Option<String>,
    pub preferences: Preferences,
    /// The choices could not be restored or saved ([`RESTORE_ERROR`],
    /// [`SAVE_ERROR`]).
    pub preferences_error: Option<&'static str>,
    /// The last banner the service refused.
    pub error: Option<String>,
    pub service: ServiceStatus,
}

impl Global for Notifications {}

impl Notifications {
    /// Restores the viewer's choices, and again whenever the identity
    /// changes.
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
        Self::reload(cx);
        Self::follow_viewer(cx);
        cx.observe_global::<AppState>(Self::follow_viewer).detach();
        probe(cx);
    }

    fn follow_viewer(cx: &mut App) {
        let viewer = match &cx.global::<AppState>().identity {
            IdentityState::Ready(identity) => Some(identity.pubkey_hex()),
            _ => None,
        };
        if cx.global::<Self>().viewer == viewer {
            return;
        }
        let this = cx.global_mut::<Self>();
        this.viewer = viewer;
        this.error = None;
        Self::reload(cx);
    }

    /// "Reload saved choices".
    pub fn reload(cx: &mut App) {
        let store = cx.global::<AppState>().store.clone();
        let this = cx.global_mut::<Self>();
        let (preferences, error) = match Preferences::restore(&store, this.viewer.as_deref()) {
            Ok(preferences) => (preferences, None),
            Err(paused) => (paused, Some(RESTORE_ERROR)),
        };
        this.preferences = preferences;
        this.preferences_error = error;
        cx.refresh_windows();
    }

    /// Applies `preferences` and saves them. A failed save keeps the change
    /// in effect and reports [`SAVE_ERROR`]; "Retry saving choices" calls
    /// this with the same choices.
    pub fn update(preferences: Preferences, cx: &mut App) {
        let store = cx.global::<AppState>().store.clone();
        let this = cx.global_mut::<Self>();
        let saved = preferences.save(&store, this.viewer.as_deref());
        if let Err(error) = &saved {
            tracing::warn!(%error, "could not save notification preferences");
        }
        this.preferences_error = saved.err().map(|_| SAVE_ERROR);
        this.preferences = preferences;
        cx.refresh_windows();
    }

    pub fn preferences(cx: &App) -> Preferences {
        cx.try_global::<Self>()
            .map(|this| this.preferences.clone())
            .unwrap_or_default()
    }

    pub(crate) fn report(error: String, cx: &mut App) {
        if cx.has_global::<Self>() {
            cx.global_mut::<Self>().error = Some(error);
        }
        cx.refresh_windows();
    }

    fn service(status: ServiceStatus, cx: &mut App) {
        if cx
            .try_global::<Self>()
            .is_some_and(|this| this.service != status)
        {
            cx.global_mut::<Self>().service = status;
            cx.refresh_windows();
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
    let preferences = Notifications::preferences(cx);
    // In front of the user: the message is in view in a focused window.
    let viewing = item.viewing && cx.active_window().is_some();
    if !preferences.allows(item.category) || (viewing && !preferences.notify_while_viewing) {
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
    // A reply's thread root is only a hint; the click reveals the message
    // wherever it is.
    let target = Target {
        community: Some(this.origin.to_string()),
        place: Place::Conversation {
            channel: item.channel_id.clone(),
            message: Some(item.message_id),
            thread: item
                .thread_root
                .as_deref()
                .and_then(|root| buzz_relay_client::EventId::from_hex(root).ok()),
            members: false,
        },
    };
    if !cx.default_global::<Shown>().admit(key, Instant::now()) {
        return;
    }
    let category = item.category;
    let shown = show(title, body, target, cx);
    // The sound follows the service taking the banner, if alerts for the
    // category are still on by then.
    cx.spawn(async move |cx| {
        let result = shown.await.unwrap_or(Ok(()));
        cx.update(|cx| match result {
            Ok(()) => crate::sounds::play(category, &Notifications::preferences(cx)),
            Err(error) => Notifications::report(error, cx),
        });
    })
    .detach();
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
pub(crate) fn short_text(value: &str, limit: usize) -> String {
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
pub(crate) fn plain_text(content: &str) -> String {
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

/// Shows a banner without waiting on the notification service; its click
/// comes back through navigation.
/// What became of one banner: `Err` carries the service's refusal.
type Delivery = futures::channel::oneshot::Receiver<Result<(), String>>;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(crate) fn show(title: String, body: String, target: Target, cx: &mut App) -> Delivery {
    struct Service(banners::Banners);
    impl Global for Service {}

    let (done, shown) = futures::channel::oneshot::channel();
    if cx.try_global::<Service>().is_none() {
        let Some(navigation) = navigation::sender(cx) else {
            return shown;
        };
        let banners = banners::Banners::start(&Runtime::handle(cx), navigation);
        cx.set_global(Service(banners));
    }
    cx.global::<Service>().0.show(banners::Banner {
        title,
        body,
        target,
        done: Some(done),
    });
    shown
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub(crate) fn show(title: String, _body: String, _target: Target, _cx: &mut App) -> Delivery {
    tracing::debug!(%title, "desktop notifications are not supported on this platform");
    let (done, shown) = futures::channel::oneshot::channel();
    let _ = done.send(Err("This build can’t show system notifications.".into()));
    shown
}

/// Learns whether the notification service is there, for the status line
/// in Settings → Notifications.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn probe(cx: &mut App) {
    let found = Runtime::handle(cx).spawn(banners::available());
    cx.spawn(async move |cx| {
        let status = match found.await {
            Ok(true) => ServiceStatus::Available,
            _ => ServiceStatus::Unavailable,
        };
        cx.update(|cx| Notifications::service(status, cx));
    })
    .detach();
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn probe(cx: &mut App) {
    Notifications::service(ServiceStatus::Unavailable, cx);
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
            viewing: false,
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
        assert!(parsed.enabled && !parsed.direct && !parsed.notify_while_viewing);
    }

    #[test]
    fn choices_belong_to_the_viewer_and_fail_closed() {
        let store = Store::in_memory().unwrap();
        let (alice, bob) = ("a".repeat(64), "b".repeat(64));
        assert_eq!(
            Preferences::restore(&store, Some(&alice)),
            Ok(Preferences::default())
        );
        let quiet = Preferences {
            direct: false,
            notify_while_viewing: true,
            ..Preferences::default()
        };
        quiet.save(&store, Some(&alice)).unwrap();
        assert_eq!(Preferences::restore(&store, Some(&alice)), Ok(quiet));
        // Every community shares them; another viewer has their own.
        assert_eq!(
            Preferences::restore(&store, Some(&bob)),
            Ok(Preferences::default())
        );
        // Unreadable choices pause alerts instead of turning them on.
        store
            .set_viewer_pref(&bob, PREFERENCES, r#"{"enabled":"yes"}"#)
            .unwrap();
        let paused = Preferences::restore(&store, Some(&bob)).unwrap_err();
        assert!(!paused.enabled && !paused.allows(Category::Mention));
    }
}
