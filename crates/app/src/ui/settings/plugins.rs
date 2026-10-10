//! Settings → Plugins (§5b.5 item 6, `Settings.tsx` plugins section): one
//! row per bundled plugin with an enable switch; Channels is required.
//! Loading plugins from a folder or Git is not part of this app.

use super::{SettingsPage, button, header, note, preference_row, rows_group, section};
use crate::plugins::{CHANNELS, Plugins, Unavailable};
use buzz_ui::{ActiveTheme, Icon, IconName, Switch, TypeStyled};
use gpui_kit::*;

impl SettingsPage {
    pub(super) fn plugins_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let plugins = cx.global::<Plugins>();
        let busy = plugins.busy();
        let mut children = vec![header("Plugins", None, cx).into_any_element()];
        match plugins.unavailable().cloned() {
            Some(Unavailable::Recovery(reason)) => children.push(recovery(
                reason,
                busy,
                plugins.error().map(str::to_owned),
                cx,
            )),
            Some(Unavailable::Storage(_)) => children.push(
                note(
                    "Plugin settings are unavailable. Profile and Appearance still work.",
                    cx,
                )
                .into_any_element(),
            ),
            None => {
                let rows = plugins
                    .rows()
                    .iter()
                    .map(|plugin| {
                        let icon = Some(
                            Icon::new(IconName::SquaresFour)
                                .size(20.)
                                .into_any_element(),
                        );
                        let row = if plugin.id == CHANNELS {
                            preference_row(
                                icon,
                                plugin.name.clone(),
                                None,
                                vec![
                                    div()
                                        .text_body_sm()
                                        .text_color(c.text_subtle)
                                        .child("Required")
                                        .into_any_element(),
                                ],
                                cx,
                            )
                        } else {
                            let id = plugin.id.clone();
                            let enabled = plugin.enabled;
                            preference_row(
                                icon,
                                plugin.name.clone(),
                                None,
                                vec![
                                    Switch::new(
                                        SharedString::from(format!("plugin-{}", plugin.id)),
                                        enabled,
                                    )
                                    .label(format!("Enable {}", plugin.name))
                                    .disabled(busy)
                                    .on_change(move |on, _, cx| Plugins::change(&id, on, cx))
                                    .into_any_element(),
                                ],
                                cx,
                            )
                        };
                        div().py(rems(0.25)).child(row).into_any_element()
                    })
                    .collect();
                children.push(rows_group(rows, cx).into_any_element());
            }
        }
        section(children)
    }
}

/// `RecoveryScreen`: the saved settings could not be read.
fn recovery(reason: String, busy: bool, error: Option<String>, cx: &App) -> AnyElement {
    let c = cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(rems(1.))
        .p(rems(2.))
        .border_1()
        .border_color(c.border_standard)
        .rounded(rems(1.5))
        .child(div().text_heading().child("Couldn’t open Buzz"))
        .child(
            div()
                .text_body()
                .child("Buzz couldn’t read its plugin configuration."),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.25))
                .child(div().text_label_sm().child("Error details"))
                .child(div().text_body_sm().text_color(c.text_subtle).child(reason)),
        )
        .child(
            div().child(
                button("plugins-retry", "Try again")
                    .disabled(busy)
                    .on_click(|_, _, cx| Plugins::retry(cx)),
            ),
        )
        .child(div().text_body().child(
            "Resetting saves a backup and restores bundled defaults. Installed plugin files \
             remain on disk and can be reinstalled.",
        ))
        .child(
            div().child(
                button("plugins-reset", "Back up & reset settings")
                    .destructive()
                    .disabled(busy)
                    .on_click(|_, _, cx| Plugins::recover(cx)),
            ),
        )
        .children(error.map(|text| div().text_body_sm().text_color(c.text_danger).child(text)))
        .into_any_element()
}
