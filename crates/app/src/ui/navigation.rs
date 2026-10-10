//! Where the window is and how it got there (`features/navigation`):
//! navigation targets, the Back/Forward history behind the header arrows,
//! and the routing of links, notification clicks and later launches to
//! the workspace.

use super::pages::Page;
use super::workspace::Workspace;
use buzz_relay_client::EventId;
use buzz_ui::ActiveTheme;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::*;
use std::time::Duration;

actions!(
    buzz_navigation,
    [
        /// Mod+[ : the previous place in history.
        GoBack,
        /// Mod+] : the next place in history.
        GoForward,
        /// Mod+Shift+A : the Messages page.
        GoHome,
    ]
);

/// History keeps at most this many places (`createMemoryHistory`).
const CAPACITY: usize = 100;
/// How long a revealed message stays tinted.
pub const HIGHLIGHT: Duration = Duration::from_millis(2000);

/// A revealed message's tint, bleeding a little past the row.
pub fn highlighted(row: AnyElement, cx: &App) -> AnyElement {
    div()
        .mx(px(-8.))
        .px(px(8.))
        .rounded(px(8.))
        .bg(cx.colors().affordance_accent)
        .child(row)
        .into_any_element()
}

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
    /// A channel, optionally revealing a message. `thread` equal to
    /// `message` opens that thread; any other value is a hint only, as the
    /// root is re-derived from signed ancestry.
    Conversation {
        channel: String,
        message: Option<EventId>,
        thread: Option<EventId>,
    },
}

impl Target {
    pub fn channel(community: Option<String>, channel: impl Into<String>) -> Self {
        Self {
            community,
            place: Place::Conversation {
                channel: channel.into(),
                message: None,
                thread: None,
            },
        }
    }
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
    /// one, which adds nothing.
    pub fn push(&mut self, target: Target) -> bool {
        if self.current() == Some(&target) {
            return false;
        }
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

/// Binds the shortcuts and starts delivering [`Request`]s. Requests sent
/// before [`install`] wait for the window.
pub fn init(cx: &mut App) -> UnboundedSender<Arrival> {
    let (sender, mut arrivals) = unbounded::<Arrival>();
    cx.set_global(Navigator {
        sender: sender.clone(),
        window: None,
    });
    cx.on_action(|_: &GoBack, cx| with_workspace(cx, Workspace::go_back));
    cx.on_action(|_: &GoForward, cx| with_workspace(cx, Workspace::go_forward));
    cx.on_action(|_: &GoHome, cx| with_workspace(cx, Workspace::go_home));
    cx.bind_keys([
        KeyBinding::new("secondary-[", GoBack, None),
        KeyBinding::new("secondary-]", GoForward, None),
        KeyBinding::new("secondary-shift-a", GoHome, None),
    ]);
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
fn with_workspace(
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
