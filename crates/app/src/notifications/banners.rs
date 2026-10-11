//! Banners through the freedesktop notification service
//! (`org.freedesktop.Notifications`) over one session-bus connection.
//!
//! A click is reported as `ActivationToken` and then `ActionInvoked`. The
//! token is what lets the window come to the front on Wayland: GNOME only
//! raises a window for an app that isn't focused when the app presents a
//! token it was handed, so both reach navigation together.

use crate::ui::navigation::{Arrival, Request, Target};
use futures::StreamExt;
use futures::channel::mpsc::UnboundedSender;
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;
use zbus::zvariant::Value;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Clicks are remembered for this many recent banners; some services keep
/// banners until they are dismissed.
const REMEMBERED: usize = 64;

pub struct Banner {
    pub title: String,
    pub body: String,
    pub target: Target,
    /// Told whether the service took the banner, with its refusal.
    pub done: Option<futures::channel::oneshot::Sender<Result<(), String>>>,
}

impl Banner {
    fn finish(&mut self, result: Result<(), String>) {
        if let Some(done) = self.done.take() {
            let _ = done.send(result);
        }
    }
}

/// Whether a notification service is on the session bus.
pub async fn available() -> bool {
    match connect().await {
        Ok(proxy) => proxy
            .call::<_, _, Vec<String>>("GetCapabilities", &())
            .await
            .is_ok(),
        Err(_) => false,
    }
}

/// Hands banners to the task that owns the bus connection, starting it on
/// first use.
pub struct Banners {
    sender: mpsc::UnboundedSender<Banner>,
}

impl Banners {
    pub fn start(runtime: &tokio::runtime::Handle, navigation: UnboundedSender<Arrival>) -> Self {
        let (sender, banners) = mpsc::unbounded_channel();
        runtime.spawn(run(banners, navigation));
        Self { sender }
    }

    /// Never waits on the service.
    pub fn show(&self, banner: Banner) {
        let _ = self.sender.send(banner);
    }
}

/// Banners shown and not yet closed, and the activation tokens of the ones
/// being clicked.
#[derive(Default)]
struct Shown {
    targets: HashMap<u32, Target>,
    order: VecDeque<u32>,
    tokens: HashMap<u32, String>,
}

impl Shown {
    fn insert(&mut self, id: u32, target: Target) {
        if self.order.len() >= REMEMBERED
            && let Some(oldest) = self.order.pop_front()
        {
            self.targets.remove(&oldest);
            self.tokens.remove(&oldest);
        }
        self.order.push_back(id);
        self.targets.insert(id, target);
    }

    fn token(&mut self, id: u32, token: String) {
        if self.targets.contains_key(&id) {
            self.tokens.insert(id, token);
        }
    }

    /// The clicked banner's place and token.
    fn invoked(&mut self, id: u32, action: &str) -> Option<Arrival> {
        let token = self.tokens.remove(&id);
        if action != "default" {
            return None;
        }
        let target = self.targets.get(&id)?.clone();
        Some(Arrival {
            request: Request::Open(target),
            activation_token: token,
        })
    }

    fn closed(&mut self, id: u32) {
        self.targets.remove(&id);
        self.tokens.remove(&id);
        self.order.retain(|shown| *shown != id);
    }
}

async fn run(mut banners: mpsc::UnboundedReceiver<Banner>, navigation: UnboundedSender<Arrival>) {
    let proxy = match connect().await {
        Ok(proxy) => proxy,
        Err(error) => {
            tracing::warn!(%error, "desktop notifications are unavailable");
            // Each banner reports the refusal.
            let error = error.to_string();
            while let Some(mut banner) = banners.recv().await {
                banner.finish(Err(error.clone()));
            }
            return;
        }
    };
    let capabilities = match proxy
        .call::<_, _, Vec<String>>("GetCapabilities", &())
        .await
    {
        Ok(capabilities) => capabilities,
        Err(error) => {
            tracing::warn!(%error, "could not read the notification service's capabilities");
            Vec::new()
        }
    };
    // A click opens the message, so a service without actions is refused
    // (`notifications/linux.rs`).
    let clickable = capabilities.iter().any(|c| c == "actions");
    let markup = capabilities.iter().any(|c| c == "body-markup");
    // Subscribed before the first banner, so no click is missed.
    let signals = match clickable {
        true => proxy.receive_all_signals().await,
        false => Err(zbus::Error::Unsupported),
    };
    let mut signals = match signals {
        Ok(signals) => signals,
        Err(error) => {
            tracing::warn!(%error, "notification clicks are unavailable");
            while let Some(mut banner) = banners.recv().await {
                banner.finish(Err(
                    "The desktop notification service does not support clicks".into(),
                ));
            }
            return;
        }
    };
    let mut shown = Shown::default();
    loop {
        tokio::select! {
            banner = banners.recv() => {
                let Some(mut banner) = banner else { return };
                match notify(&proxy, &banner, markup).await {
                    Ok(id) => {
                        banner.finish(Ok(()));
                        shown.insert(id, banner.target);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not show a notification");
                        banner.finish(Err(error.to_string()));
                    }
                }
            }
            signal = signals.next() => {
                let Some(signal) = signal else {
                    tracing::warn!("the notification service went away");
                    return;
                };
                let header = signal.header();
                let body = signal.body();
                match header.member().map(|member| member.as_str()) {
                    Some("ActivationToken") => {
                        if let Ok((id, token)) = body.deserialize::<(u32, String)>() {
                            shown.token(id, token);
                        }
                    }
                    Some("ActionInvoked") => {
                        if let Ok((id, action)) = body.deserialize::<(u32, String)>()
                            && let Some(arrival) = shown.invoked(id, &action)
                        {
                            let _ = navigation.unbounded_send(arrival);
                        }
                    }
                    Some("NotificationClosed") => {
                        if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>() {
                            shown.closed(id);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn connect() -> zbus::Result<zbus::Proxy<'static>> {
    let connection = zbus::Connection::session().await?;
    zbus::Proxy::new_owned(connection, SERVICE, PATH, SERVICE).await
}

async fn notify(proxy: &zbus::Proxy<'_>, banner: &Banner, markup: bool) -> zbus::Result<u32> {
    let actions = ["default", "Open"];
    let hints: HashMap<&str, Value<'_>> = HashMap::from([
        // Names the app, its icon and its notification settings.
        ("desktop-entry", Value::from("buzz-gpui")),
        ("category", Value::from("im.received")),
        // Sounds, when enabled, are the app's own.
        ("suppress-sound", Value::from(true)),
    ]);
    proxy
        .call(
            "Notify",
            &(
                "Buzz",
                0u32,
                "buzz-gpui",
                banner.title.as_str(),
                notification_body(&banner.body, markup).as_ref(),
                &actions[..],
                hints,
                -1i32,
            ),
        )
        .await
}

/// The body is plain text. A service that advertises `body-markup` parses
/// it as markup, so escape it there; typed `<harness>` must stay literal.
fn notification_body(body: &str, markup: bool) -> Cow<'_, str> {
    if !markup || !body.contains(['&', '<', '>']) {
        return Cow::Borrowed(body);
    }
    let mut escaped = String::with_capacity(body.len());
    for character in body.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::navigation::Place;

    #[test]
    fn markup_services_receive_escaped_plain_text() {
        let preview = "<harness> & a > b";
        assert_eq!(notification_body(preview, false), preview);
        assert_eq!(
            notification_body(preview, true),
            "&lt;harness&gt; &amp; a &gt; b"
        );
        assert!(matches!(
            notification_body("plain", true),
            Cow::Borrowed("plain")
        ));
    }

    fn target(channel: &str) -> Target {
        Target {
            community: Some("ws://relay".into()),
            place: Place::Conversation {
                channel: channel.into(),
                message: None,
                thread: None,
                members: false,
            },
        }
    }

    #[test]
    fn a_click_carries_its_token() {
        let mut shown = Shown::default();
        shown.insert(7, target("general"));
        shown.token(7, "token".into());
        let arrival = shown.invoked(7, "default").unwrap();
        assert_eq!(arrival.activation_token.as_deref(), Some("token"));
        assert!(matches!(arrival.request, Request::Open(t) if t == target("general")));
        // A second click (some services keep the banner) has no token left.
        let again = shown.invoked(7, "default").unwrap();
        assert_eq!(again.activation_token, None);
    }

    #[test]
    fn unknown_and_closed_banners_open_nothing() {
        let mut shown = Shown::default();
        shown.token(1, "stray".into());
        assert!(shown.invoked(1, "default").is_none());
        shown.insert(2, target("general"));
        shown.closed(2);
        assert!(shown.invoked(2, "default").is_none());
        shown.insert(3, target("general"));
        assert!(shown.invoked(3, "other").is_none());
    }

    #[test]
    fn forgets_the_oldest_banners() {
        let mut shown = Shown::default();
        for id in 0..(REMEMBERED as u32 + 1) {
            shown.insert(id, target("general"));
        }
        assert!(shown.invoked(0, "default").is_none());
        assert!(shown.invoked(REMEMBERED as u32, "default").is_some());
    }
}
