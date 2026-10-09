//! Machine idle time for automatic away (relay.md §5h.2): after ten
//! minutes without input anywhere on the machine, every community
//! publishes away until input returns.
//!
//! Sampled every 30 s from GNOME's Mutter idle monitor over D-Bus, which
//! works on GNOME under both Wayland and X11. Where it is unavailable (other
//! desktops, no session bus, headless) the app is never idle.

use crate::community::AppState;
use futures::StreamExt;
use gpui_kit::{App, Global};
use std::time::Duration;

const AWAY_AFTER: Duration = Duration::from_secs(600);
const SAMPLE_EVERY: Duration = Duration::from_secs(30);

pub struct Idle {
    pub idle: bool,
}

impl Global for Idle {}

impl Idle {
    pub fn init(cx: &mut App) {
        cx.set_global(Self { idle: false });
        let (sender, mut samples) = futures::channel::mpsc::unbounded();
        let started = std::thread::Builder::new()
            .name("buzz-idle".into())
            .spawn(move || {
                let Some(monitor) = Monitor::connect() else {
                    tracing::info!("no machine idle monitor; staying active");
                    return;
                };
                loop {
                    let Some(idle) = monitor.idle_time() else {
                        tracing::info!("machine idle monitor stopped answering; staying active");
                        let _ = sender.unbounded_send(false);
                        return;
                    };
                    if sender.unbounded_send(idle >= AWAY_AFTER).is_err() {
                        return;
                    }
                    std::thread::sleep(SAMPLE_EVERY);
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "could not start the idle monitor");
            return;
        }
        cx.spawn(async move |cx| {
            while let Some(idle) = samples.next().await {
                cx.update(|cx| Self::set(idle, cx));
            }
        })
        .detach();
    }

    fn set(idle: bool, cx: &mut App) {
        if cx.global::<Self>().idle == idle {
            return;
        }
        cx.global_mut::<Self>().idle = idle;
        let communities = cx.global::<AppState>().communities.clone();
        for community in communities {
            community.read(cx).session.set_idle(idle);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
struct Monitor(zbus::blocking::Connection);

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
impl Monitor {
    fn connect() -> Option<Self> {
        let monitor = Self(zbus::blocking::Connection::session().ok()?);
        monitor.idle_time().map(|_| monitor)
    }

    fn idle_time(&self) -> Option<Duration> {
        let reply = self
            .0
            .call_method(
                Some("org.gnome.Mutter.IdleMonitor"),
                "/org/gnome/Mutter/IdleMonitor/Core",
                Some("org.gnome.Mutter.IdleMonitor"),
                "GetIdletime",
                &(),
            )
            .ok()?;
        let millis: u64 = reply.body().deserialize().ok()?;
        Some(Duration::from_millis(millis))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
struct Monitor;

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
impl Monitor {
    fn connect() -> Option<Self> {
        None
    }

    fn idle_time(&self) -> Option<Duration> {
        None
    }
}
