//! Settings → Plugins (§5b.5 item 6, `Settings.tsx` plugins section): one
//! row per bundled plugin with an enable switch; Channels is required.
//! Below them, the plugins the user added from folders
//! ([`crate::external_plugins`]), each with the command it runs.

use super::{SettingsPage, button, header, note, preference_row, rows_group, section};
use crate::external_plugins::{ExternalPlugins, Status};
use crate::plugins::{CHANNELS, Plugins, Unavailable};
use crate::ui::toasts::{self, Notice};
use buzz_ui::{ActiveTheme, Icon, IconName, Switch, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

impl SettingsPage {
    pub(super) fn plugins_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let yours = your_plugins(cx);
        let plugins = cx.global::<Plugins>();
        let busy = plugins.busy();
        let mut children = vec![
            header("Plugins", None, cx).into_any_element(),
            yours,
            div()
                .pt(rems(1.5))
                .px(super::inset())
                .text_heading()
                .child("Built in")
                .into_any_element(),
        ];
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

    /// Asks for a plugin folder and adds it, off.
    fn add_plugin_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add plugin".into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(folder) = paths.into_iter().next() else {
                return;
            };
            cx.update(|_, cx| {
                if let Err(reason) = ExternalPlugins::add(folder, cx) {
                    toasts::show(
                        Notice::error("plugin-add", "Couldn’t add the plugin").description(reason),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }
}

/// The plugins added from folders.
fn your_plugins(cx: &mut Context<SettingsPage>) -> AnyElement {
    let c = *cx.colors();
    let rows: Vec<AnyElement> = cx
        .global::<ExternalPlugins>()
        .plugins()
        .iter()
        .enumerate()
        .map(|(index, plugin)| {
            let path = plugin.path.clone();
            let (status, failed) = match (&plugin.manifest, &plugin.status) {
                (Err(reason), _) => (reason.clone(), true),
                (Ok(_), Status::Off) => ("Off".to_owned(), false),
                (Ok(_), Status::Starting) => ("Starting…".to_owned(), false),
                (Ok(_), Status::Running) => ("Running".to_owned(), false),
                (Ok(_), Status::Failed(reason)) => (
                    format!("Stopped: {}", reason.lines().next().unwrap_or_default()),
                    true,
                ),
            };
            let mut description = div().flex().flex_col().gap(rems(0.125)).child(
                div()
                    .when(failed, |status| status.text_color(c.text_danger))
                    .child(status),
            );
            if let Ok(manifest) = &plugin.manifest {
                if let Some(text) = &manifest.description {
                    description = description.child(text.clone());
                }
                description = description.child(format!(
                    "Runs {} in {}",
                    manifest.command_line(),
                    plugin.path.display()
                ));
            }
            let reload = {
                let path = path.clone();
                button(
                    SharedString::from(format!("plugin-reload-{index}")),
                    "Reload",
                )
                .ghost()
                .on_click(move |_, _, cx| ExternalPlugins::reload(&path, cx))
            };
            let remove = {
                let path = path.clone();
                button(
                    SharedString::from(format!("plugin-remove-{index}")),
                    "Remove",
                )
                .ghost()
                .on_click(move |_, _, cx| ExternalPlugins::remove(&path, cx))
            };
            let switch = Switch::new(
                SharedString::from(format!("plugin-external-{index}")),
                plugin.enabled,
            )
            .disabled(plugin.manifest.is_err() && !plugin.enabled)
            .on_change(move |on, _, cx| ExternalPlugins::set_enabled(&path, on, cx));
            div()
                .py(rems(0.25))
                .child(preference_row(
                    Some(Icon::new(IconName::Plug).size(20.).into_any_element()),
                    plugin.name(),
                    Some(description.into_any_element()),
                    vec![
                        reload.into_any_element(),
                        remove.into_any_element(),
                        switch.into_any_element(),
                    ],
                    cx,
                ))
                .into_any_element()
        })
        .collect();
    let empty = rows.is_empty();
    div()
        .flex()
        .flex_col()
        .gap(rems(0.75))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(rems(1.))
                .pl(super::inset())
                .child(div().text_heading().child("Your plugins"))
                .child(
                    button("plugin-add", "Add plugin folder…")
                        .icon(IconName::FolderSimple)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_plugin_folder(window, cx)
                        })),
                ),
        )
        .child(note(
            "A plugin is a program on this computer. It runs with your full access and is not \
             sandboxed, so turn on only plugins you trust. Saving a file in its folder restarts it.",
            cx,
        ))
        .when(!empty, |list| list.child(rows_group(rows, cx)))
        .into_any_element()
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
