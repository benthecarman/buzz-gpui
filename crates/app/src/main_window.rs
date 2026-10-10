//! The main window's life: opening it, and opening it again after it was
//! hidden to the tray; what minimize and close do; where it was last
//! (`window_state.rs`); the launch surface; and the tray icon, which can
//! outlive the window.
//!
//! The app quits explicitly. Closing the window quits unless the user
//! chose close-to-tray and a panel is showing the icon, so the app is never
//! left running with no way back to it. Quitting first stops this
//! computer's agents, and does not quit if that cannot be confirmed.

use crate::community::{AppState, Community};
use crate::idle::Idle;
use crate::runtime::Runtime;
use crate::ui::launch::Launch;
use crate::ui::navigation;
use crate::ui::workspace::Workspace;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use crate::tray::{Tray, TrayEvent};

/// View-state key for the tray choices (a device preference).
const PREFERENCES: &str = "tray";
/// Device preference holding where the window was.
const GEOMETRY: &str = "window-state";
/// The header every control area sits in (ui.md §2.1).
const HEADER_HEIGHT: Pixels = px(48.);
/// Geometry is saved once moving or resizing pauses this long.
const SAVE_GEOMETRY_AFTER: Duration = Duration::from_millis(500);

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
    /// Where the window was when it was last hidden or moved, saved for
    /// the next launch.
    bounds: Option<WindowBounds>,
    /// The launch surface has shown; reopening from the tray skips it.
    launched: bool,
    /// Bumped by every move or resize, so only the last one saves.
    moves: u64,
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
    let bounds = store
        .device_pref(GEOMETRY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<SavedGeometry>(&json).ok())
        .filter(SavedGeometry::valid)
        .map(|saved| restore_geometry(&saved, cx));
    cx.set_global(MainWindow {
        handle: None,
        bounds,
        launched: false,
        moves: 0,
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
    let launch = (!cx.global::<MainWindow>().launched).then(|| Launch::new(Instant::now()));
    cx.global_mut::<MainWindow>().launched = true;
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
            let workspace = cx.new(|cx| Workspace::new(window, cx));
            cx.new(|cx| Root::new(workspace, launch, window, cx))
        },
    );
    match opened {
        Ok((handle, root)) => {
            cx.global_mut::<MainWindow>().handle = Some(handle);
            let workspace = root.read(cx).workspace.clone();
            navigation::install(handle, &workspace, cx);
            Idle::set_visible(true, cx);
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
        remember_bounds(window.window_bounds(), cx);
        quit(cx);
    }
}

/// Quits once this computer's agents have stopped; if that cannot be
/// confirmed the app keeps running, as buzz-app refuses the exit.
pub fn quit(cx: &mut App) {
    save_geometry(cx);
    match crate::hosts::Hosts::shutdown_local(cx) {
        Ok(()) => cx.quit(),
        Err(error) => {
            tracing::error!(%error, "agent shutdown incomplete; app exit was refused");
            crate::ui::toasts::show(
                crate::ui::toasts::Notice::error("quit", "Buzz couldn’t quit").description(
                    format!("{error}. Your agents may still be running; try again."),
                ),
                cx,
            );
        }
    }
}

/// Closes the window and keeps running; the tray, a notification or
/// another launch opens it again.
fn hide(window: &mut Window, cx: &mut App) {
    remember_bounds(window.window_bounds(), cx);
    save_geometry(cx);
    window.remove_window();
    Idle::set_visible(false, cx);
}

fn remember_bounds(bounds: WindowBounds, cx: &mut App) {
    cx.global_mut::<MainWindow>().bounds = Some(bounds);
}

/// Saves where the window is for the next launch.
fn save_geometry(cx: &mut App) {
    let Some(bounds) = cx.global::<MainWindow>().bounds else {
        return;
    };
    let json = serde_json::to_string(&SavedGeometry::from(bounds)).expect("geometry serializes");
    if let Err(error) = cx
        .global::<AppState>()
        .store
        .set_device_pref(GEOMETRY, &json)
    {
        tracing::warn!(%error, "could not save where the window is");
    }
}

/// Follows moves and resizes, saving once they pause.
fn moved(window: &mut Window, cx: &mut App) {
    remember_bounds(window.window_bounds(), cx);
    let main = cx.global_mut::<MainWindow>();
    main.moves += 1;
    let generation = main.moves;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_GEOMETRY_AFTER).await;
        cx.update(|cx| {
            if cx.global::<MainWindow>().moves == generation {
                save_geometry(cx);
            }
        });
    })
    .detach();
}

/// The window's root: input for automatic away, captured before any
/// control handles it, and the launch surface over the workspace.
struct Root {
    workspace: Entity<Workspace>,
    launch: Option<Launch>,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    fn new(
        workspace: Entity<Workspace>,
        launch: Option<Launch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe_window_bounds(window, |_, window, cx| moved(window, cx)),
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    Idle::input(cx);
                }
            }),
        ];
        Self {
            workspace,
            launch,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let launch = self.launch.as_mut().and_then(|launch| {
            launch.update(self.workspace.read(cx).launch_content(cx), Instant::now());
            launch.render(window, cx)
        });
        if launch.is_none() {
            self.launch = None;
        }
        div()
            .relative()
            .size_full()
            .capture_any_mouse_down(|_, _, cx| Idle::input(cx))
            .capture_key_down(|_, _, cx| Idle::input(cx))
            .on_mouse_move(|_, _, cx| Idle::input(cx))
            .on_scroll_wheel(|_, _, cx| Idle::input(cx))
            .child(self.workspace.clone())
            .children(launch)
    }
}

/// Where the window was (`tauri-plugin-window-state`'s fields): the
/// windowed bounds, also kept while maximized or fullscreen.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct SavedGeometry {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    maximized: bool,
    fullscreen: bool,
}

impl SavedGeometry {
    fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.
            && self.height > 0.
    }

    fn bounds(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.x), px(self.y)),
            size(px(self.width), px(self.height)),
        )
    }
}

impl From<WindowBounds> for SavedGeometry {
    fn from(bounds: WindowBounds) -> Self {
        let (inner, maximized, fullscreen) = match bounds {
            WindowBounds::Windowed(inner) => (inner, false, false),
            WindowBounds::Maximized(inner) => (inner, true, false),
            WindowBounds::Fullscreen(inner) => (inner, false, true),
        };
        Self {
            x: inner.origin.x.into(),
            y: inner.origin.y.into(),
            width: inner.size.width.into(),
            height: inner.size.height.into(),
            maximized,
            fullscreen,
        }
    }
}

/// The saved geometry, moved so the header and both control areas are on
/// some screen; otherwise centred at the top of the primary work area.
fn restore_geometry(saved: &SavedGeometry, cx: &App) -> WindowBounds {
    let areas: Vec<Bounds<Pixels>> = cx
        .displays()
        .iter()
        .map(|display| display.visible_bounds())
        .collect();
    let fallback = cx
        .primary_display()
        .map(|display| display.visible_bounds())
        .or_else(|| areas.first().copied());
    let bounds = match fallback {
        Some(fallback) => reachable(saved.bounds(), &areas, fallback),
        None => saved.bounds(),
    };
    if saved.fullscreen {
        WindowBounds::Fullscreen(bounds)
    } else if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

/// `reachable_client`: every corner of the header strip must be in some
/// work area, though not the same one; otherwise the window is clamped to
/// the fallback area and placed at its top centre.
fn reachable(
    window: Bounds<Pixels>,
    areas: &[Bounds<Pixels>],
    fallback: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let (x, y) = (window.origin.x, window.origin.y);
    let right = x + (window.size.width - px(1.)).max(px(0.));
    let bottom = y + HEADER_HEIGHT - px(1.);
    let inside = |p: Point<Pixels>| {
        areas.iter().any(|area| {
            p.x >= area.origin.x
                && p.y >= area.origin.y
                && p.x < area.origin.x + area.size.width
                && p.y < area.origin.y + area.size.height
        })
    };
    if [
        point(x, y),
        point(right, y),
        point(x, bottom),
        point(right, bottom),
    ]
    .into_iter()
    .all(inside)
    {
        return window;
    }
    let size = size(
        window.size.width.min(fallback.size.width),
        window.size.height.min(fallback.size.height),
    );
    Bounds::new(
        point(
            fallback.origin.x + (fallback.size.width - size.width) / 2.,
            fallback.origin.y,
        ),
        size,
    )
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
                TrayEvent::Quit => quit(cx),
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
    fn saved_geometry_must_keep_the_header_reachable() {
        let screen = Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1080.)));
        let left = Bounds::new(point(px(-1280.), px(0.)), size(px(1280.), px(1024.)));
        let window = |x: f32, y: f32| Bounds::new(point(px(x), px(y)), size(px(1200.), px(800.)));
        // On one screen, or across two side by side: kept.
        assert_eq!(
            reachable(window(100., 50.), &[screen], screen),
            window(100., 50.)
        );
        assert_eq!(
            reachable(window(-600., 50.), &[left, screen], screen),
            window(-600., 50.)
        );
        // Its screen is gone: top centre of the primary work area.
        assert_eq!(
            reachable(window(-600., 50.), &[screen], screen),
            window(360., 0.)
        );
        // The header above the top edge: moved too, and clamped in size.
        let big = Bounds::new(point(px(0.), px(-20.)), size(px(4000.), px(800.)));
        assert_eq!(
            reachable(big, &[screen], screen),
            Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(800.)))
        );
    }

    #[test]
    fn geometry_keeps_the_windowed_bounds_and_mode() {
        let inner = Bounds::new(point(px(10.), px(20.)), size(px(900.), px(700.)));
        let saved = SavedGeometry::from(WindowBounds::Maximized(inner));
        assert!(saved.maximized && !saved.fullscreen && saved.valid());
        assert_eq!(saved.bounds(), inner);
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<SavedGeometry>(&json).unwrap(), saved);
        assert!(!SavedGeometry { width: 0., ..saved }.valid());
    }

    #[test]
    fn preferences_default_to_an_icon_without_hiding() {
        let parsed: Preferences = serde_json::from_str(r#"{"close_to_tray":true}"#).unwrap();
        assert!(parsed.show_icon && parsed.close_to_tray && !parsed.minimize_to_tray);
        assert!(!Preferences::default().close_to_tray);
    }
}
