//! Settings → Updates (buzz-app's `UpdateSettings`): "Software updates",
//! the version, and the update status in an `EmptyState` card.
//!
//! This build has no updater yet (GAPS.md, Decided: updates later), so a
//! check always ends in buzz-app's "unavailable on this build" state.

use super::{SettingsPage, button, header, section};
use buzz_ui::{ActiveTheme, Icon, IconName, TypeStyled, tokens::radius};
use gpui_kit::*;
use std::time::Duration;

/// What the status line says (`messages` in `UpdateSettings.tsx`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum UpdateState {
    #[default]
    Idle,
    Checking,
    Unavailable,
}

impl UpdateState {
    fn message(self) -> &'static str {
        match self {
            Self::Idle => "Check if a new version is available.",
            Self::Checking => "Checking for updates…",
            Self::Unavailable => {
                "Automatic updates aren't available on this build. Download the latest release \
                 manually."
            }
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::Unavailable => "Check again",
            Self::Idle | Self::Checking => "Check for updates",
        }
    }
}

impl SettingsPage {
    /// Checks for an update: this build can only say it has no updater.
    pub(super) fn check_for_update(&mut self, cx: &mut Context<Self>) {
        self.update_state = UpdateState::Checking;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.update_state = UpdateState::Unavailable;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn updates_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // Opening the section checks (`showInline`).
        if self.update_state == UpdateState::Idle {
            self.check_for_update(cx);
        }
        let c = *cx.colors();
        let state = self.update_state;
        let card = div()
            .w_full()
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::CONTAINER)
            .flex()
            .flex_col()
            .items_center()
            .gap(rems(1.))
            .py(rems(2.))
            .px(rems(1.5))
            .text_center()
            .child(
                div()
                    .flex()
                    .text_color(c.text_subtle)
                    .child(Icon::new(IconName::ArrowsClockwise).size(32.)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .max_w(rems(24.))
                    .child(
                        div()
                            .text_label_sm()
                            .child(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(state.message()),
                    ),
            )
            .child(
                div().mt(rems(0.5)).child(
                    button("check-updates", state.action())
                        .loading(state == UpdateState::Checking)
                        .on_click(cx.listener(|this, _, _, cx| this.check_for_update(cx))),
                ),
            );
        section(vec![
            header("Software updates", None, cx).into_any_element(),
            card.into_any_element(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn copy_follows_the_reference() {
        assert_eq!(UpdateState::Idle.action(), "Check for updates");
        assert_eq!(UpdateState::Unavailable.action(), "Check again");
        assert!(
            UpdateState::Unavailable
                .message()
                .starts_with("Automatic updates aren't available on this build.")
        );
    }
}
