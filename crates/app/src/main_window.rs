//! The main window's life: opening it, and opening it again after it was
//! hidden to the tray; what minimize and close do; and the tray icon,
//! which can outlive the window.
//!
//! The app quits explicitly. Closing the window quits unless the user
//! chose close-to-tray and a panel is showing the icon, so the app is never
//! left running with no way back to it.

use crate::community::{AppState, Community};
use crate::runtime::Runtime;
use crate::ui::navigation;
use crate::ui::workspace::Workspace;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use crate::tray::{Tray, TrayEvent};

/// View-state key for the tray choices (a device preference).
const PREFERENCES: &str = "tray";

/// The tray choices, for this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub show_icon: bool,
    /// The window's minimize button hides it to the tray.
    pub minimize_to_tray: bool,
    /// Closing the window keeps the app running in the tray.
    pub close_to_tray: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            show_icon: true,
            minimize_to_tray: false,
            close_to_tray: false,
        }
    }
}

const DEFAULT_SIZE: Size<Pixels> = Size {
    width: px(1200.),
    height: px(800.),
};

struct MainWindow {
    handle: Option<AnyWindowHandle>,
    /// Where the window was when it was last hidden.
    bounds: Option<WindowBounds>,
    preferences: Preferences,
    /// A panel is showing the tray icon.
    shown: bool,
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    tray: Option<Arc<Tray>>,
    /// Mentions and direct messages unread, last told to the tray.
    attention: u32,
    _communities: Vec<Subscription>,
}

impl Global for MainWindow {}

/// Loads the tray choices and puts the icon up if wanted. Call before
/// [`open`].
pub fn init(cx: &mut App) {
    cx.set_quit_mode(QuitMode::Explicit);
    let store = cx.global::<AppState>().store.clone();
    let preferences = store
        .device_pref(PREFERENCES)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    cx.set_global(MainWindow {
        handle: None,
        bounds: None,
        preferences,
        shown: false,
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        tray: None,
        attention: 0,
        _communities: Vec::new(),
    });
    cx.observe_global::<AppState>(watch_communities).detach();
    watch_communities(cx);
    apply_preferences(cx);
}

/// Whether the main window is open.
pub fn is_open(cx: &App) -> bool {
    cx.try_global::<MainWindow>()
        .and_then(|main| main.handle)
        .is_some_and(|handle| cx.windows().contains(&handle))
}

/// Opens the main window, where it was last.
pub fn open(cx: &mut App) {
    if is_open(cx) {
        return;
    }
    let bounds = cx
        .global::<MainWindow>()
        .bounds
        .unwrap_or_else(|| WindowBounds::Windowed(Bounds::centered(None, DEFAULT_SIZE, cx)));
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(bounds),
            window_min_size: Some(size(px(480.), px(400.))),
            app_id: Some("buzz-gpui".into()),
            // For the taskbar, Alt+Tab and the window list; the header
            // draws no title.
            titlebar: Some(TitlebarOptions {
                title: Some("Buzz".into()),
                ..Default::default()
            }),
            // The header is the title bar, with buzz-app's own controls.
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        },
        cx,
        |window, cx| {
            // The compositor's close (a shortcut, the overview) goes the
            // way of the close button.
            window.on_window_should_close(cx, |window, cx| {
                close(window, cx);
                false
            });
            cx.new(|cx| Workspace::new(window, cx))
        },
    );
    match opened {
        Ok((handle, workspace)) => {
            cx.global_mut::<MainWindow>().handle = Some(handle);
            navigation::install(handle, &workspace, cx);
        }
        Err(error) => {
            tracing::error!(%error, "could not open the window");
            cx.quit();
        }
    }
}

/// The minimize button: hides to the tray if chosen, else minimizes.
pub fn minimize(window: &mut Window, cx: &mut App) {
    let main = cx.global::<MainWindow>();
    if main.preferences.show_icon && main.preferences.minimize_to_tray && main.shown {
        hide(window, cx);
    } else {
        window.minimize_window();
    }
}

/// The close button, or the compositor's close: hides to the tray if
/// chosen, else quits.
pub fn close(window: &mut Window, cx: &mut App) {
    let main = cx.global::<MainWindow>();
    if main.preferences.show_icon && main.preferences.close_to_tray && main.shown {
        hide(window, cx);
    } else {
        cx.quit();
    }
}

/// Closes the window and keeps running; the tray, a notification or
/// another launch opens it again.
fn hide(window: &mut Window, cx: &mut App) {
    cx.global_mut::<MainWindow>().bounds = Some(window.window_bounds());
    window.remove_window();
}

pub fn preferences(cx: &App) -> Preferences {
    cx.global::<MainWindow>().preferences
}

/// Whether a panel is showing the tray icon, so hiding to it is safe.
pub fn tray_shown(cx: &App) -> bool {
    cx.global::<MainWindow>().shown
}

pub fn set_preferences(preferences: Preferences, cx: &mut App) {
    cx.global_mut::<MainWindow>().preferences = preferences;
    let json = serde_json::to_string(&preferences).expect("preferences serialize");
    if let Err(error) = cx
        .global::<AppState>()
        .store
        .set_device_pref(PREFERENCES, &json)
    {
        tracing::warn!(%error, "could not save the tray settings");
    }
    apply_preferences(cx);
}

/// Puts the icon up or takes it down to match the choices.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn apply_preferences(cx: &mut App) {
    let main = cx.global::<MainWindow>();
    match (main.preferences.show_icon, main.tray.is_some()) {
        (true, false) => start_tray(cx),
        (false, true) => {
            let main = cx.global_mut::<MainWindow>();
            main.tray = None;
            main.shown = false;
            cx.refresh_windows();
        }
        _ => {}
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn apply_preferences(_cx: &mut App) {}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn start_tray(cx: &mut App) {
    use futures::StreamExt;
    let Some(navigation) = navigation::sender(cx) else {
        return;
    };
    let (events, mut received) = futures::channel::mpsc::unbounded();
    let runtime = Runtime::handle(cx);
    let started = runtime.spawn(Tray::start(navigation, events));
    cx.spawn(async move |cx| {
        let tray = match started.await {
            Ok(Ok(tray)) => Arc::new(tray),
            Ok(Err(error)) => {
                tracing::warn!(%error, "could not put up the tray icon");
                return;
            }
            Err(_) => return,
        };
        let kept = cx.update(|cx| {
            let main = cx.global_mut::<MainWindow>();
            // Turned off while it was starting.
            if !main.preferences.show_icon || main.tray.is_some() {
                return false;
            }
            main.tray = Some(tray.clone());
            main.attention = u32::MAX;
            true
        });
        if !kept {
            return;
        }
        cx.update(update_attention);
        let weak = Arc::downgrade(&tray);
        drop(tray);
        while let Some(event) = received.next().await {
            // Taken down: the events of the old icon no longer count.
            let current = cx.update(|cx| {
                cx.global::<MainWindow>()
                    .tray
                    .as_ref()
                    .zip(weak.upgrade())
                    .is_some_and(|(now, this)| Arc::ptr_eq(now, &this))
            });
            if !current {
                return;
            }
            cx.update(|cx| match event {
                TrayEvent::Shown(shown) => {
                    cx.global_mut::<MainWindow>().shown = shown;
                    cx.refresh_windows();
                }
                TrayEvent::Quit => cx.quit(),
            });
        }
    })
    .detach();
}

/// Follows every community's unread counts for the tray icon.
fn watch_communities(cx: &mut App) {
    let communities = cx.global::<AppState>().communities.clone();
    let subscriptions = communities
        .iter()
        .map(|community| cx.observe(community, |_, cx| update_attention(cx)))
        .collect();
    cx.global_mut::<MainWindow>()._communities = subscriptions;
    update_attention(cx);
}

#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
fn attention(communities: &[Entity<Community>], cx: &App) -> u32 {
    communities
        .iter()
        .map(|community| {
            let unread = &community.read(cx).snapshot.unread;
            unread.mentions + unread.direct
        })
        .sum()
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn update_attention(cx: &mut App) {
    let count = attention(&cx.global::<AppState>().communities.clone(), cx);
    let main = cx.global_mut::<MainWindow>();
    let Some(tray) = main.tray.clone() else {
        return;
    };
    if main.attention == count {
        return;
    }
    main.attention = count;
    Runtime::handle(cx).spawn(async move {
        if let Err(error) = tray.set_attention(count).await {
            tracing::debug!(%error, "could not update the tray icon");
        }
    });
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn update_attention(_cx: &mut App) {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn preferences_default_to_an_icon_without_hiding() {
        let parsed: Preferences = serde_json::from_str(r#"{"close_to_tray":true}"#).unwrap();
        assert!(parsed.show_icon && parsed.close_to_tray && !parsed.minimize_to_tray);
        assert!(!Preferences::default().close_to_tray);
    }
}
