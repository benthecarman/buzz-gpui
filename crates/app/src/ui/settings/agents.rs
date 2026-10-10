//! Settings → Agents (§5a.11, adapted): the agent hosts, the harnesses
//! this computer can run, saved agent activity, and the "Remember
//! mentioned agents" message preference.

use super::{
    Modal, SettingsPage, button, form_group, header, inline_header, preference_row, rows_group,
    section,
};
use crate::community::AppState;
use crate::hosts::{HostConnection, Hosts};
use crate::ui::agents::{AddHost, DefaultsDialog, Dismissed, Saved, harness_row};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, AlertDialog, Badge, BadgeTone, Button, ButtonSize, Icon, IconButton,
    IconButtonSize, IconName, Spinner, Surface, Switch, TypeStyled,
};
use gpui_kit::*;
use std::ops::Range;

/// Device preference: keep the agents a message mentioned selected for the
/// next one (buzz-app's `buzz-remember-mentioned-agents.v1`, on unless
/// "off").
const REMEMBER_AGENTS: &str = "remember-mentioned-agents";
/// `followupDraft`'s limits.
const FOLLOWUP_AGENTS: usize = 32;
const FOLLOWUP_CHARS: usize = 16_000;

/// Whether the composer keeps mentioned agents for the next message.
pub fn remember_mentioned_agents(cx: &App) -> bool {
    cx.global::<AppState>()
        .store
        .device_pref(REMEMBER_AGENTS)
        .ok()
        .flatten()
        .is_none_or(|value| value != "\"off\"")
}

fn set_remember_mentioned_agents(on: bool, cx: &App) -> Result<(), String> {
    cx.global::<AppState>()
        .store
        .set_device_pref(REMEMBER_AGENTS, if on { "\"on\"" } else { "\"off\"" })
        .map_err(|error| error.to_string())
}

/// buzz-app's `followupDraft`: one `@Name ` chip per agent, first use
/// first, deduplicated, at most 32 and 16,000 characters. Each item is
/// `(pubkey, chip text)`; the result is the text and each chip's range.
pub fn followup_draft(
    agents: &[(String, String)],
) -> (String, Vec<(Range<usize>, String, String)>) {
    let mut text = String::new();
    let mut chips: Vec<(Range<usize>, String, String)> = Vec::new();
    for (pubkey, chip) in agents {
        if chips.len() == FOLLOWUP_AGENTS || chips.iter().any(|(_, id, _)| id == pubkey) {
            continue;
        }
        let start = text.len();
        if text.chars().count() + chip.chars().count() + 1 > FOLLOWUP_CHARS {
            break;
        }
        text.push_str(chip);
        chips.push((start..text.len(), pubkey.clone(), chip.clone()));
        text.push(' ');
    }
    (text, chips)
}

impl SettingsPage {
    fn add_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = cx.new(|cx| AddHost::new(window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close_modal(cx));
        self.modal = Some(Modal::AddHost {
            dialog,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Opens the same "Agent defaults" dialog as the Agents page.
    fn agent_defaults(
        &mut self,
        host: Entity<HostConnection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| DefaultsDialog::new(host, window, cx));
        let subscriptions = [
            cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close_modal(cx)),
            cx.subscribe(&dialog, |this, _, Saved(text), cx| {
                this.show_toast(text.clone(), cx)
            }),
        ];
        self.modal = Some(Modal::Defaults {
            dialog,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn remove_host(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::RemoveHost { host, error }) = &mut self.modal else {
            return;
        };
        let host = host.clone();
        match Hosts::remove(&host, cx) {
            Ok(()) => self.modal = None,
            Err(message) => *error = Some(message.into()),
        }
        cx.notify();
    }

    pub(super) fn agents_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let hosts = cx.global::<Hosts>().hosts.clone();
        let rows = hosts
            .iter()
            .enumerate()
            .map(|(index, host)| self.host_row(index, host, cx).into_any_element())
            .collect();
        let hosts_block = div()
            .child(inline_header(
                "Hosts",
                Some(
                    "Agents run on this computer or on machines running buzzd. Remove a host to \
                     disconnect from it; its agents keep running there."
                        .into(),
                ),
                vec![
                    button("settings-add-host", "Add host")
                        .outline()
                        .size(ButtonSize::Sm)
                        .icon(IconName::Plus)
                        .on_click(cx.listener(|this, _, window, cx| this.add_host(window, cx)))
                        .into_any_element(),
                ],
                cx,
            ))
            .child(rows_group(rows, cx));

        // Harnesses of the embedded host, as in buzz-app.
        let local = hosts
            .iter()
            .find(|host| host.read(cx).remote.is_none())
            .cloned();
        let harnesses = local.as_ref().map(|host| {
            let list = host.read(cx).harnesses.clone();
            let checking = list.is_none();
            let refresh = host.clone();
            let body: AnyElement = match list {
                None => div()
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(Spinner::new())
                    .child("Checking harnesses…")
                    .into_any_element(),
                Some(list) if list.is_empty() => div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("No harnesses found on this computer.")
                    .into_any_element(),
                Some(list) => div()
                    .flex()
                    .flex_col()
                    .children(list.iter().enumerate().map(|(index, harness)| {
                        harness_row(harness, index > 0, &c).into_any_element()
                    }))
                    .into_any_element(),
            };
            div()
                .child(inline_header(
                    "Harnesses",
                    Some("Agent programs this computer can run.".into()),
                    vec![
                        IconButton::new("harnesses-refresh", IconName::ArrowsClockwise)
                            .size(IconButtonSize::Sm)
                            .disabled(checking)
                            .tooltip("Check again")
                            .on_click(move |_, _, cx| {
                                refresh.update(cx, |host, cx| host.refresh(cx).detach())
                            })
                            .into_any_element(),
                    ],
                    cx,
                ))
                .child(form_group(vec![body], cx).py(rems(0.)))
        });
        let mut parts = vec![
            header("Agents", None, cx).into_any_element(),
            hosts_block.into_any_element(),
        ];
        parts.extend(harnesses.map(IntoElement::into_any_element));
        parts.push(self.archive_section(window, cx).into_any_element());
        let remember = remember_mentioned_agents(cx);
        parts.push(
            div()
                .child(inline_header("Messages", None, Vec::new(), cx))
                .child(rows_group(
                    vec![
                        preference_row(
                            None,
                            "Remember mentioned agents",
                            Some(
                                "Keep the same agents selected for your next message in this \
                                 channel or thread."
                                    .into_any_element(),
                            ),
                            vec![
                                Switch::new("remember-agents", remember)
                                    .on_change({
                                        let page = cx.entity().downgrade();
                                        move |on, _, cx| {
                                            let saved = set_remember_mentioned_agents(on, cx);
                                            let _ = page.update(cx, |this, cx| {
                                                if let Err(error) = saved {
                                                    tracing::warn!(%error, "could not save the preference");
                                                    this.show_toast_tone(
                                                        "Agent preference wasn’t saved",
                                                        buzz_ui::ToastTone::Error,
                                                        cx,
                                                    );
                                                }
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .into_any_element(),
                            ],
                            cx,
                        )
                        .into_any_element(),
                    ],
                    cx,
                ))
                .into_any_element(),
        );
        section(parts)
    }

    fn host_row(
        &self,
        index: usize,
        host: &Entity<HostConnection>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = host.read(cx);
        let (badge, tone) = match &state.state {
            ConnectionState::Connected { .. } => ("Connected", BadgeTone::Success),
            ConnectionState::Connecting { .. } => ("Connecting", BadgeTone::Neutral),
            ConnectionState::Disconnected { .. } => ("Offline", BadgeTone::Danger),
            ConnectionState::Rejected { .. } => ("Refused", BadgeTone::Warning),
        };
        let description: SharedString = match &state.remote {
            None => "Agents on this computer".into(),
            Some(remote) if remote.token.is_some() => {
                format!("{} · token saved in config.toml", remote.address).into()
            }
            Some(remote) => remote.address.clone().into(),
        };
        let remote = state.remote.is_some();
        let connected = matches!(state.state, ConnectionState::Connected { .. });
        let label = state.label.clone();
        let reconnect = host.clone();
        let remove = host.clone();
        let defaults = host.clone();
        let mut trailing = vec![
            Badge::new(badge).tone(tone).into_any_element(),
            IconButton::new(
                SharedString::from(format!("host-defaults-{index}")),
                IconName::SlidersHorizontal,
            )
            .size(IconButtonSize::Sm)
            .disabled(!connected)
            .tooltip("Agent defaults")
            .on_click(cx.listener(move |this, _, window, cx| {
                this.agent_defaults(defaults.clone(), window, cx)
            }))
            .into_any_element(),
            IconButton::new(
                SharedString::from(format!("host-reconnect-{index}")),
                IconName::Plug,
            )
            .size(IconButtonSize::Sm)
            .tooltip("Reconnect")
            .on_click(move |_, _, cx| reconnect.read(cx).reconnect())
            .into_any_element(),
        ];
        if remote {
            trailing.push(
                button(SharedString::from(format!("host-remove-{index}")), "Remove")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.modal = Some(Modal::RemoveHost {
                            host: remove.clone(),
                            error: None,
                        });
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        preference_row(
            Some(
                Icon::new(if remote {
                    IconName::Globe
                } else {
                    IconName::Monitor
                })
                .size(20.)
                .into_any_element(),
            ),
            label,
            Some(description.into_any_element()),
            trailing,
            cx,
        )
    }

    pub(super) fn remove_host_dialog(
        &self,
        host: &Entity<HostConnection>,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let state = host.read(cx);
        let label = state.label.clone();
        let address = state
            .remote
            .as_ref()
            .map(|remote| remote.address.clone())
            .unwrap_or_default();
        AlertDialog::new(format!("Remove {label}?"))
            .description(format!(
                "This disconnects from {address} and forgets its token. Agents on that host \
                 keep running, and you can add it again later."
            ))
            .children(
                error.map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .action(
                Button::new("cancel-remove", "Cancel")
                    .surface(Surface::Floating)
                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
            )
            .action(
                Button::new("confirm-remove", "Remove host")
                    .destructive()
                    .on_click(cx.listener(|this, _, _, cx| this.remove_host(cx))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::followup_draft;
    use core::prelude::v1::test;

    #[test]
    fn followups_chip_each_agent_once() {
        let agents = vec![
            ("a".to_string(), "@Scout".to_string()),
            ("b".to_string(), "@Echo".to_string()),
            ("a".to_string(), "@Scout".to_string()),
        ];
        let (text, chips) = followup_draft(&agents);
        assert_eq!(text, "@Scout @Echo ");
        assert_eq!(chips.len(), 2);
        assert_eq!(chips[0], (0..6, "a".to_string(), "@Scout".to_string()));
        assert_eq!(&text[chips[1].0.clone()], "@Echo");
        let many: Vec<(String, String)> = (0..40)
            .map(|i| (format!("{i}"), format!("@Agent{i}")))
            .collect();
        assert_eq!(followup_draft(&many).1.len(), 32);
    }
}
