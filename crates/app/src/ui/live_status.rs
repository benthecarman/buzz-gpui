//! Connection and live-update notices (`LiveStatus.tsx`, the relay
//! connection error of `service.ts`): one toast at a time, in the toast
//! column, with the action that recovers.

use crate::community::Community;
use buzz_relay_client::SessionStatus;
use buzz_ui::{Button, ButtonSize, Toast, ToastTone};
use gpui_kit::*;

/// The notice for the current community and channel, if one is needed.
pub fn toast(
    community: &Entity<Community>,
    channel: Option<&str>,
    window: &Window,
    cx: &App,
) -> Option<AnyElement> {
    let snapshot = community.read(cx).snapshot.clone();
    let toast = match &snapshot.status {
        // The relay never answered: nothing live, maybe a saved snapshot.
        SessionStatus::Failed(error) => {
            let description = if snapshot.cached {
                format!(
                    "{error} Showing saved conversations. Reconnect to update saved conversations."
                )
            } else {
                error.clone()
            };
            let session = community.read(cx).session.clone();
            Toast::new("relay-connection", "Relay connection failed")
                .tone(ToastTone::Error)
                .description(description)
                .action(
                    Button::new("relay-connection-retry", "Try again")
                        .size(ButtonSize::Sm)
                        .on_click(move |_, _, _| session.retry()),
                )
        }
        SessionStatus::Ready => {
            // Startup and bounded automatic quota recovery stay quiet.
            let reason = snapshot.live_status.issue(channel)?;
            let session = community.read(cx).session.clone();
            Toast::new("live-status", "Live updates need attention")
                .tone(ToastTone::Warning)
                .description(format!(
                    "{reason} Only currently accessible messages remain readable."
                ))
                .action(
                    Button::new("live-status-retry", "Retry live updates")
                        .size(ButtonSize::Sm)
                        .on_click(move |_, _, _| session.retry_live()),
                )
        }
        SessionStatus::Connecting => return None,
    };
    let right = window.viewport_size().width - px(16.) - px(384.);
    Some(
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(point(right, px(128.)))
                .child(toast),
        )
        .with_priority(2)
        .into_any_element(),
    )
}
