//! Where the window is and how it got there (`features/navigation`):
//! navigation targets, the Back/Forward history behind the header arrows,
//! and the routing of links, notification clicks and later launches to
//! the workspace.

use super::pages::Page;
use super::workspace::Workspace;
use buzz_relay_client::EventId;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::*;

actions!(
    buzz_navigation,
    [
        /// Mod+[ : the previous place in history.
        GoBack,
        /// Mod+] : the next place in history.
        GoForward,
        /// Mod+Shift+A : the Messages page.
        GoHome,
        /// Mod+W : closes the channel's shown tab, else the window.
        CloseTab,
        /// Mod+K : Search Buzz.
        SearchBuzz,
        /// Mod+F : Search this conversation.
        SearchConversation,
    ]
);

/// History keeps at most this many places (`createMemoryHistory`).
const CAPACITY: usize = 100;

/// A place the window can show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The community's relay origin, as `RelayOrigin` displays it. `None`
    /// before any community exists.
    pub community: Option<String>,
    pub place: Place,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A page with no conversation of its own: Messages before a channel
    /// is chosen, Agents, Settings.
    Page(Page),
    /// Settings at a section (`kind: "settings", section`): each section
    /// is a visit.
    Settings(super::settings::Section),
    /// A channel, optionally revealing a message. `thread` equal to
    /// `message` opens that thread; any other value names the thread the
    /// message is in (history records it for a message selected in an
    /// open thread), a hint only, as the root is re-derived from signed
    /// ancestry.
    Conversation {
        channel: String,
        message: Option<EventId>,
        thread: Option<EventId>,
        /// The members dialog is open over it (`panel: "members"`), so
        /// Back closes it. Sessions have no members surface.
        members: bool,
    },
    /// Messages without a conversation of its own (`routes.ts`).
    Channels(ChannelRoute),
    /// A page at one of its own routes (`target.route.params`), such as a
    /// Projects entity. The page validates the params.
    Route {
        page: Page,
        params: serde_json::Value,
    },
}

/// What a conversation place shows beside its channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beside {
    /// The thread from this root.
    Thread(EventId),
    /// The message, selected in the thread already open.
    InThread(EventId),
    /// The message in the timeline when it is a top-level row there, else
    /// in its thread.
    Find(EventId),
    /// No thread.
    Nothing,
}

impl Beside {
    /// For a place's `message` and `thread`, with `open` the root of the
    /// thread open beside the channel once it has loaded. A message that
    /// names the open thread goes back there without a search of the
    /// timeline, where a reply never shows; the panel still checks the
    /// message's own ancestry before it selects it.
    pub fn of(message: Option<EventId>, thread: Option<EventId>, open: Option<EventId>) -> Self {
        match (message, thread) {
            (Some(message), Some(thread)) if message == thread => Self::Thread(message),
            (Some(message), Some(thread)) if open == Some(thread) => Self::InThread(message),
            (Some(message), _) => Self::Find(message),
            (None, _) => Self::Nothing,
        }
    }
}

/// The Channels page's routes that are not a conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelRoute {
    /// Writing a new direct message.
    NewMessage,
    /// No conversation: after the open one was left with none to follow.
    Empty,
    /// A session draft under `parent`; buzz-app no longer nests sessions
    /// and explains where to start one.
    #[allow(dead_code)]
    NewSession { parent: String },
}

// Nothing in this app links to a session draft yet; Sessions will.
#[allow(dead_code)]
impl ChannelRoute {
    /// Validates route params as `isChannelRoute` does: `"new-message"`,
    /// `"empty"`, or exactly `{kind: "new-session", parentId}` with a
    /// non-blank parent.
    pub fn parse(params: &serde_json::Value) -> Option<Self> {
        match params {
            serde_json::Value::String(route) if route == "new-message" => Some(Self::NewMessage),
            serde_json::Value::String(route) if route == "empty" => Some(Self::Empty),
            serde_json::Value::Object(map) => {
                let parent = map.get("parentId")?.as_str()?;
                (map.get("kind")?.as_str()? == "new-session"
                    && !parent.trim().is_empty()
                    && map.keys().all(|key| key == "kind" || key == "parentId"))
                .then(|| Self::NewSession {
                    parent: parent.to_owned(),
                })
            }
            _ => None,
        }
    }
}

impl Target {
    pub fn channel(community: Option<String>, channel: impl Into<String>) -> Self {
        Self {
            community,
            place: Place::Conversation {
                channel: channel.into(),
                message: None,
                thread: None,
                members: false,
            },
        }
    }
}

/// `current` and `next` show the same message in the same way, telling
/// apart at most by which thread they say it is in. That is a hint (a
/// thread other than the message); a thread equal to the message opens it
/// instead, a different place.
fn same_message(current: &Target, next: &Target) -> bool {
    let (
        Place::Conversation {
            channel,
            message: Some(message),
            thread,
            members,
        },
        Place::Conversation {
            channel: next_channel,
            message: next_message,
            thread: next_thread,
            members: next_members,
        },
    ) = (&current.place, &next.place)
    else {
        return false;
    };
    let hint = |thread: &Option<EventId>| *thread != Some(*message);
    current.community == next.community
        && channel == next_channel
        && Some(*message) == *next_message
        && members == next_members
        && hint(thread)
        && hint(next_thread)
}

/// The places visited, as a browser keeps them: pushing drops anything
/// ahead of the current place.
#[derive(Default)]
pub struct History {
    entries: Vec<Target>,
    index: usize,
}

impl History {
    pub fn current(&self) -> Option<&Target> {
        self.entries.get(self.index)
    }

    /// Records a new place. Returns false when it is already the current
    /// one, which adds nothing. A place that only names the thread of the
    /// current message (a link opened a reply without saying its thread,
    /// then the thread loaded) is the same place: it fills in the current
    /// entry, so Back never lands on a spot that looks the same.
    pub fn push(&mut self, target: Target) -> bool {
        let Some(current) = self.entries.get_mut(self.index) else {
            return self.add(target);
        };
        if *current == target {
            return false;
        }
        if same_message(current, &target) {
            // The newer thread wins; a place naming none keeps the known one.
            if let (
                Place::Conversation { thread, .. },
                Place::Conversation {
                    thread: Some(next), ..
                },
            ) = (&mut current.place, &target.place)
            {
                *thread = Some(*next);
            }
            return false;
        }
        self.add(target)
    }

    fn add(&mut self, target: Target) -> bool {
        self.entries.truncate(self.index + 1);
        self.entries.push(target);
        if self.entries.len() > CAPACITY {
            self.entries.remove(0);
        }
        self.index = self.entries.len() - 1;
        true
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    pub fn back(&mut self) -> Option<Target> {
        if !self.can_go_back() {
            return None;
        }
        self.index -= 1;
        self.current().cloned()
    }

    pub fn forward(&mut self) -> Option<Target> {
        if !self.can_go_forward() {
            return None;
        }
        self.index += 1;
        self.current().cloned()
    }
}

/// Something from outside the window: a later launch, the OS or a
/// notification click. Each brings the window to the front.
#[derive(Debug)]
pub enum Request {
    Focus,
    Url(String),
    Open(Target),
}

/// A [`Request`] with the activation token that came with it, which lets
/// the window come to the front on Wayland (see
/// `gpui_linux::set_activation_token`).
#[derive(Debug)]
pub struct Arrival {
    pub request: Request,
    #[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
    pub activation_token: Option<String>,
}

impl From<Request> for Arrival {
    fn from(request: Request) -> Self {
        Self {
            request,
            activation_token: None,
        }
    }
}

/// Reaches the one main window from anywhere in the app.
struct Navigator {
    sender: UnboundedSender<Arrival>,
    window: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
}

impl Global for Navigator {}

/// Handles the navigation shortcuts, which `crate::shortcuts` binds, and
/// starts delivering [`Request`]s. Requests sent before [`install`] wait
/// for the window.
pub fn init(cx: &mut App) -> UnboundedSender<Arrival> {
    let (sender, mut arrivals) = unbounded::<Arrival>();
    cx.set_global(Navigator {
        sender: sender.clone(),
        window: None,
    });
    cx.on_action(|_: &GoBack, cx| with_workspace(cx, Workspace::go_back));
    cx.on_action(|_: &GoForward, cx| with_workspace(cx, Workspace::go_forward));
    cx.on_action(|_: &GoHome, cx| with_workspace(cx, Workspace::go_home));
    cx.on_action(|_: &CloseTab, cx| with_workspace(cx, Workspace::close_tab_or_window));
    // Bound app-wide, so they work with nothing in the window focused.
    cx.on_action(|_: &SearchBuzz, cx| with_workspace(cx, Workspace::search_buzz));
    cx.on_action(|_: &SearchConversation, cx| with_workspace(cx, Workspace::search_conversation));
    // Bindings come from `crate::shortcuts`, which can rebind them.
    cx.spawn(async move |cx| {
        while let Some(arrival) = arrivals.next().await {
            cx.update(|cx| {
                // Hidden to the tray: open it again first.
                crate::main_window::open(cx);
                with_workspace(cx, move |workspace, window, cx| {
                    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                    if let Some(token) = arrival.activation_token {
                        gpui_linux::set_activation_token(token);
                    }
                    // A window comes forward on macOS only with its app.
                    #[cfg(target_os = "macos")]
                    cx.activate(true);
                    window.activate_window();
                    workspace.handle(arrival.request, window, cx);
                })
            });
        }
    })
    .detach();
    sender
}

/// Names the main window once it is open.
pub fn install(window: AnyWindowHandle, workspace: &Entity<Workspace>, cx: &mut App) {
    cx.global_mut::<Navigator>().window = Some((window, workspace.downgrade()));
}

/// Where to send [`Request`]s from other threads.
pub fn sender(cx: &App) -> Option<UnboundedSender<Arrival>> {
    cx.try_global::<Navigator>()
        .map(|navigator| navigator.sender.clone())
}

/// Runs `f` on the workspace once the current update finishes, so it works
/// from global action handlers too, which run while the window is busy.
pub(crate) fn with_workspace(
    cx: &mut App,
    f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) {
    cx.defer(move |cx| {
        let Some((window, workspace)) = cx
            .try_global::<Navigator>()
            .and_then(|navigator| navigator.window.clone())
        else {
            return;
        };
        let _ = window.update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| f(workspace, window, cx))
        });
    });
}

fn workspace(cx: &App) -> Option<Entity<Workspace>> {
    cx.try_global::<Navigator>()?.window.as_ref()?.1.upgrade()
}

/// A `buzz://` link clicked inside the window.
pub fn open_url(url: &str, window: &mut Window, cx: &mut App) {
    if let Some(workspace) = workspace(cx) {
        let url = url.to_string();
        workspace.update(cx, |workspace, cx| workspace.open_url(&url, window, cx));
    }
}

/// A web link clicked in a message (`onOpenLink`): a channel panel that
/// shows it (GitHub) opens beside the conversation. A click with a
/// modifier, or a link no panel takes, goes to the browser.
pub fn open_web_link(url: &str, modifiers: Modifiers, window: &mut Window, cx: &mut App) {
    let opened = !modifiers.modified()
        && workspace(cx).is_some_and(|workspace| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_panel_link(url, window, cx)
            })
        });
    if !opened {
        cx.open_url(url);
    }
}

/// [`open_url`] for a `buzz://` link in a message: `false` when this
/// window cannot take it, so the link can say so.
pub fn try_open_url(url: &str, window: &mut Window, cx: &mut App) -> bool {
    if workspace(cx).is_none() || crate::deep_links::parse(url).is_none() {
        return false;
    }
    open_url(url, window, cx);
    true
}

/// A mention clicked inside the window: shows the person's card at the
/// pointer.
pub fn show_profile(pubkey: String, at: Point<Pixels>, window: &mut Window, cx: &mut App) {
    if let Some(workspace) = workspace(cx) {
        workspace.update(cx, |workspace, cx| {
            workspace.show_profile(pubkey, at, window, cx)
        });
    }
}

/// Opens a conversation the session just opened or joined, such as a
/// direct message, once the channel list has it.
pub fn open_conversation(id: String, window: &mut Window, cx: &mut App) {
    if let Some(workspace) = workspace(cx) {
        workspace.update(cx, |workspace, cx| {
            workspace.open_conversation(id, window, cx)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn channel(name: &str) -> Target {
        Target::channel(Some("ws://relay".into()), name)
    }

    #[test]
    fn pushes_and_walks_back_and_forward() {
        let mut history = History::default();
        assert!(!history.can_go_back() && !history.can_go_forward());
        assert_eq!(history.back(), None);
        assert!(history.push(channel("a")));
        assert!(!history.can_go_back());
        assert!(history.push(channel("b")));
        assert!(history.push(channel("c")));
        assert_eq!(history.back(), Some(channel("b")));
        assert_eq!(history.back(), Some(channel("a")));
        assert_eq!(history.back(), None);
        assert!(history.can_go_forward());
        assert_eq!(history.forward(), Some(channel("b")));
        assert_eq!(history.current(), Some(&channel("b")));
    }

    #[test]
    fn pushing_drops_the_forward_entries() {
        let mut history = History::default();
        for name in ["a", "b", "c"] {
            history.push(channel(name));
        }
        history.back();
        history.back();
        assert!(history.push(channel("d")));
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), Some(channel("a")));
    }

    #[test]
    fn the_current_place_is_not_pushed_twice() {
        let mut history = History::default();
        assert!(history.push(channel("a")));
        assert!(!history.push(channel("a")));
        assert!(!history.can_go_back());
        let thread = Target {
            community: Some("ws://relay".into()),
            place: Place::Conversation {
                channel: "a".into(),
                message: Some(EventId::from_byte_array([1; 32])),
                thread: Some(EventId::from_byte_array([1; 32])),
                members: false,
            },
        };
        assert!(history.push(thread));
        let other_community = Target::channel(Some("ws://other".into()), "a");
        assert!(history.push(other_community));
        assert!(history.push(Target {
            community: Some("ws://other".into()),
            place: Place::Page(Page::AGENTS),
        }));
        assert_eq!(
            history.back(),
            Some(Target::channel(Some("ws://other".into()), "a"))
        );
    }

    #[test]
    fn a_root_link_keeps_the_thread_and_back_returns_to_the_reply() {
        use crate::ui::thread::{Opening, Shown};
        let root = EventId::from_byte_array([1; 32]);
        let reply = EventId::from_byte_array([2; 32]);
        let at = |(message, thread): (EventId, Option<EventId>)| Target {
            community: Some("ws://relay".into()),
            place: Place::Conversation {
                channel: "a".into(),
                message: Some(message),
                thread,
                members: false,
            },
        };
        let beside = |target: &Target, shown: &Shown| match target.place {
            Place::Conversation {
                message, thread, ..
            } => Beside::of(message, thread, shown.root),
            _ => unreachable!(),
        };
        let mut history = History::default();
        // A panel opened on a reply; once the root loads, history knows
        // the thread the reply is in.
        let mut shown = Shown {
            message: reply,
            exact: true,
            root: None,
        };
        assert_eq!(shown.place(), (reply, None));
        shown.root = Some(root);
        assert_eq!(shown.place(), (reply, Some(root)));
        history.push(at(shown.place()));

        // A link to the thread's root selects it in the same panel.
        let link = at((root, Some(root)));
        assert!(history.push(link.clone()));
        assert_eq!(beside(&link, &shown), Beside::Thread(root));
        assert_eq!(shown.open(root, false), Opening::Select);
        shown.message = root;
        // The selected root is recorded as its thread, which is where the
        // window already is.
        assert_eq!(shown.place(), (root, Some(root)));
        assert!(!history.push(at(shown.place())));
        assert_eq!(shown.open(root, false), Opening::Shown);

        // Back selects the reply again in the same panel.
        let back = history.back().unwrap();
        assert_eq!(beside(&back, &shown), Beside::InThread(reply));
        assert_eq!(shown.open(reply, true), Opening::Select);
        shown.message = reply;
        // Forward selects the root again.
        let forward = history.forward().unwrap();
        assert_eq!(beside(&forward, &shown), Beside::Thread(root));
        assert_eq!(shown.open(root, false), Opening::Select);

        // A place naming another thread, or none, looks for the message.
        let other = EventId::from_byte_array([3; 32]);
        assert_eq!(
            Beside::of(Some(reply), Some(other), Some(root)),
            Beside::Find(reply)
        );
        assert_eq!(
            Beside::of(Some(reply), None, Some(root)),
            Beside::Find(reply)
        );
        assert_eq!(
            Beside::of(Some(reply), Some(root), None),
            Beside::Find(reply)
        );
        assert_eq!(Beside::of(None, None, Some(root)), Beside::Nothing);
    }

    #[test]
    fn naming_the_thread_of_the_current_message_adds_no_visit() {
        let root = EventId::from_byte_array([1; 32]);
        let reply = EventId::from_byte_array([2; 32]);
        let at = |message, thread, members| Target {
            community: Some("ws://relay".into()),
            place: Place::Conversation {
                channel: "a".into(),
                message: Some(message),
                thread,
                members,
            },
        };
        let mut history = History::default();
        history.push(channel("a"));
        // A link opened the reply without naming its thread.
        assert!(history.push(at(reply, None, false)));
        history.push(channel("b"));
        assert_eq!(history.back(), Some(at(reply, None, false)));
        // Recorded once the thread loads: the same entry, filled in, with
        // the places ahead kept.
        assert!(!history.push(at(reply, Some(root), false)));
        assert_eq!(history.current(), Some(&at(reply, Some(root), false)));
        assert!(history.can_go_forward());
        // A place naming no thread keeps the one known.
        assert!(!history.push(at(reply, None, false)));
        assert_eq!(history.current(), Some(&at(reply, Some(root), false)));
        // Back goes straight to the channel, not to the same spot again.
        assert_eq!(history.back(), Some(channel("a")));

        // The thread opened from its root, or the members dialog over the
        // message, is another place.
        let mut history = History::default();
        history.push(at(root, None, false));
        assert!(history.push(at(root, Some(root), false)));
        history.push(at(reply, None, false));
        assert!(history.push(at(reply, Some(root), true)));
        assert!(history.push(at(reply, Some(reply), true)));
    }

    #[test]
    fn parses_channel_routes() {
        use serde_json::json;
        assert_eq!(
            ChannelRoute::parse(&json!("new-message")),
            Some(ChannelRoute::NewMessage)
        );
        assert_eq!(
            ChannelRoute::parse(&json!("empty")),
            Some(ChannelRoute::Empty)
        );
        assert_eq!(
            ChannelRoute::parse(&json!({"kind": "new-session", "parentId": "c1"})),
            Some(ChannelRoute::NewSession {
                parent: "c1".into()
            })
        );
        for invalid in [
            json!("other"),
            json!({"kind": "new-session", "parentId": " "}),
            json!({"kind": "new-session", "parentId": "c1", "extra": 1}),
            json!({"kind": "other", "parentId": "c1"}),
            json!(null),
        ] {
            assert_eq!(ChannelRoute::parse(&invalid), None, "{invalid}");
        }
    }

    #[test]
    fn keeps_a_bounded_history() {
        let mut history = History::default();
        for index in 0..CAPACITY + 10 {
            history.push(channel(&index.to_string()));
        }
        let mut steps = 0;
        while history.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, CAPACITY - 1);
        assert_eq!(history.current(), Some(&channel("10")));
    }
}
