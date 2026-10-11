//! Banners through the system's notification center on macOS and Windows,
//! which GPUI posts: `UNUserNotificationCenter` and WinRT toasts. Both are
//! silent; the app plays the chosen sound itself.
//!
//! Each banner is tagged with a fresh ID, and a click comes back with that
//! tag, so the banner's place is remembered for recent banners. macOS asks
//! for permission on the first banner, and posts nothing for a binary that
//! is not inside `Buzz.app`. Windows shows toasts under the app identity
//! set at launch (`main`), the macOS bundle identifier.

use crate::ui::navigation::{Arrival, Request, Target};
use futures::channel::mpsc::UnboundedSender;
use gpui_kit::{App, Global, SharedString, SystemNotification};
use std::collections::{HashMap, VecDeque};

/// Clicks are remembered for this many recent banners; notification
/// centers keep them until they are dismissed.
const REMEMBERED: usize = 64;

/// Banners shown and not yet clicked.
#[derive(Default)]
struct Shown {
    targets: HashMap<SharedString, Target>,
    order: VecDeque<SharedString>,
}

impl Global for Shown {}

impl Shown {
    fn insert(&mut self, tag: SharedString, target: Target) {
        if self.order.len() >= REMEMBERED
            && let Some(oldest) = self.order.pop_front()
        {
            self.targets.remove(&oldest);
        }
        self.order.push_back(tag.clone());
        self.targets.insert(tag, target);
    }

    fn clicked(&mut self, tag: &SharedString) -> Option<Target> {
        self.order.retain(|shown| shown != tag);
        self.targets.remove(tag)
    }
}

/// Whether the system will show banners for this process: on macOS only
/// from inside an app bundle.
pub fn available() -> bool {
    !cfg!(target_os = "macos")
        || std::env::current_exe()
            .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}

/// Sends clicked banners' places to navigation. Call once, before the
/// first [`show`].
pub fn start(navigation: UnboundedSender<Arrival>, cx: &mut App) {
    cx.set_global(Shown::default());
    cx.on_system_notification_response(move |response, cx| {
        // Action buttons are not offered; only the banner itself.
        if response.action_id.is_some() {
            return;
        }
        if let Some(target) = cx.global_mut::<Shown>().clicked(&response.tag) {
            let _ = navigation.unbounded_send(Request::Open(target).into());
        }
    });
}

/// Posts one banner. The system shows it, or not, on its own; only a
/// process macOS will not show banners for is refused here.
pub fn show(title: String, body: String, target: Target, cx: &mut App) -> Result<(), String> {
    if !available() {
        return Err("Notifications need Buzz to run from Buzz.app.".into());
    }
    let tag = SharedString::from(uuid::Uuid::new_v4().to_string());
    cx.global_mut::<Shown>().insert(tag.clone(), target);
    cx.show_system_notification(SystemNotification {
        tag,
        title: title.into(),
        body: body.into(),
        actions: Vec::new(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::navigation::Place;

    fn target(channel: &str) -> Target {
        Target {
            community: None,
            place: Place::Channel {
                id: channel.into(),
                message: None,
                thread: None,
            },
        }
    }

    #[test]
    fn a_click_opens_the_banner_once() {
        let mut shown = Shown::default();
        shown.insert("a".into(), target("one"));
        assert_eq!(shown.clicked(&"a".into()), Some(target("one")));
        assert_eq!(shown.clicked(&"a".into()), None);
    }

    #[test]
    fn only_recent_banners_are_remembered() {
        let mut shown = Shown::default();
        for n in 0..=REMEMBERED {
            shown.insert(n.to_string().into(), target("x"));
        }
        assert_eq!(shown.clicked(&"0".into()), None);
        assert!(shown.clicked(&REMEMBERED.to_string().into()).is_some());
    }
}
