//! What the Messages page shows without an open channel
//! (`ChannelsPage.tsx`, `App.tsx`): the connection splash, a destination
//! being checked or one that failed, and the empty selection.

use buzz_relay_client::{ChannelSummary, ChannelType, SessionSnapshot, SessionStatus};
use buzz_ui::{ActiveTheme, Button, ButtonSize, Icon, IconName, TypeStyled};
use gpui_kit::*;

/// What fills the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Empty {
    /// The community is not connected and nothing is saved to show.
    Splash,
    /// A routed channel is being looked up.
    Checking,
    /// Nothing is selected.
    Select,
}

/// The splash shows while the session is not ready and has no channels
/// to show (`session.status !== "ready" && !session.cached`).
pub fn empty(snapshot: Option<&SessionSnapshot>, checking: bool) -> Empty {
    if checking {
        return Empty::Checking;
    }
    match snapshot {
        Some(snapshot)
            if snapshot.status != SessionStatus::Ready && snapshot.channels.is_empty() =>
        {
            Empty::Splash
        }
        _ => Empty::Select,
    }
}

/// The empty page: "Your channels, one conversation." while the community
/// is not connected, else a status line in the page's corner; `select` is
/// what the page says with nothing selected.
pub fn render(empty: Empty, select: &'static str, cx: &App) -> AnyElement {
    let c = cx.colors();
    match empty {
        Empty::Splash => div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(px(32.))
            .bg(c.surface_panel)
            .child(
                div()
                    .p(px(20.))
                    .mb(px(16.))
                    .rounded(px(20.))
                    .bg(c.affordance_subtle)
                    .text_color(c.text_standard)
                    .child(Icon::new(IconName::Plug).size(30.)),
            )
            .child(
                div()
                    .text_title()
                    .text_color(c.text_standard)
                    .child("Your channels, one conversation."),
            )
            .child(
                div()
                    .max_w(px(550.))
                    .mt(px(8.))
                    .text_body()
                    .text_color(c.text_subtle)
                    .text_center()
                    .child(
                        "Connection details and retry are in the sidebar. Your profile and \
                         settings work without a community.",
                    ),
            )
            .into_any_element(),
        Empty::Checking | Empty::Select => div()
            .flex_1()
            .py(px(32.))
            .px(px(16.))
            .text_body_sm()
            .text_color(c.text_subtle)
            .child(match empty {
                Empty::Checking => "Checking conversation access…",
                _ => select,
            })
            .into_any_element(),
    }
}

/// "This destination couldn’t open" (`App.tsx`), with Retry navigation and
/// Open Settings.
pub fn failure(
    on_retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_settings: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let c = cx.colors();
    div()
        .flex_1()
        .p(px(20.))
        .child(
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(12.))
                .my(px(20.))
                .p(px(20.))
                .rounded(px(16.))
                .border_1()
                .border_color(c.border_warning)
                .bg(c.affordance_warning)
                .text_body_sm()
                .text_color(c.text_warning)
                .child(div().text_heading().child("This destination couldn’t open"))
                .child(
                    "The destination is unavailable or isn’t supported yet. Your target has been \
                 kept for retry.",
                )
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(
                            Button::new("retry-navigation", "Retry navigation")
                                .size(ButtonSize::Md)
                                .on_click(on_retry),
                        )
                        .child(
                            Button::new("failure-settings", "Open Settings")
                                .size(ButtonSize::Md)
                                .on_click(on_settings),
                        ),
                ),
        )
        .into_any_element()
}

/// The channel a community opens on without a remembered one: the first
/// listed that is not an agent session (`ChannelsPage`'s default).
pub fn default_channel(snapshot: &SessionSnapshot) -> Option<ChannelSummary> {
    snapshot
        .channels
        .iter()
        .find(|channel| {
            channel.channel_type != Some(ChannelType::Session)
                && !channel.archived
                && !snapshot.hidden_dms.contains(&channel.id)
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn the_default_channel_skips_sessions() {
        let channel = |id: &str, kind| ChannelSummary {
            id: id.into(),
            channel_type: Some(kind),
            ..Default::default()
        };
        let channels = vec![
            channel("s", ChannelType::Session),
            channel("g", ChannelType::Stream),
        ];
        let snapshot = SessionSnapshot {
            channels: channels.into(),
            ..test_snapshot()
        };
        assert_eq!(default_channel(&snapshot).unwrap().id, "g");
        // Saved channels still show while connecting.
        assert_eq!(empty(Some(&snapshot), false), Empty::Select);
        assert_eq!(empty(Some(&snapshot), true), Empty::Checking);
        let offline = test_snapshot();
        assert_eq!(empty(Some(&offline), false), Empty::Splash);
        let ready = SessionSnapshot {
            status: SessionStatus::Ready,
            ..offline
        };
        assert_eq!(empty(Some(&ready), false), Empty::Select);
    }

    fn test_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            status: SessionStatus::Connecting,
            live: false,
            relay: None,
            roster: buzz_relay_client::RosterStatus::Loading,
            channels: std::sync::Arc::new([]),
            profiles: Default::default(),
            unread: Default::default(),
            presence: Default::default(),
            typing: Default::default(),
            thread_typing: Default::default(),
            typing_anywhere: Default::default(),
            sidebar: Default::default(),
            hidden_dms: std::sync::Arc::new([]),
            emoji: Default::default(),
        }
    }
}
