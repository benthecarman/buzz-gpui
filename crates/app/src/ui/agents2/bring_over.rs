//! Bring over agents (`BringOver` in `AgentsPage.tsx`): identities earlier
//! agent systems saved on the hosts, each brought over with its key, name
//! and picture as an agent of the chosen type, which may adopt its
//! instructions and workspace. Shown in the peek slot, as
//! a new agent is.

use super::page::{radio_dot, side_body};
use super::problem;
use crate::agents2::{Agents2, Portable, Portables, types};
use buzz_host::api::PortSource;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, IconButton, IconButtonSize, IconName,
    TypeStyled, tokens::radius,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

/// Where each was saved (`SOURCES`).
fn source_label(source: PortSource) -> &'static str {
    match source {
        PortSource::Harness => "Buzz 1.0 harness agent",
        PortSource::Installed => "Buzz 0.X",
        PortSource::Development => "Buzz 0.X dev",
    }
}

pub enum BringOverEvent {
    Close,
}

pub struct BringOver {
    found: Option<Portables>,
    kind: Option<SharedString>,
    /// The agent being brought over, by key.
    pending: Option<String>,
    error: Option<SharedString>,
    _load: Task<()>,
}

impl EventEmitter<BringOverEvent> for BringOver {}

impl BringOver {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let load = match Agents2::global(cx) {
            Some(service) => {
                let task = service.update(cx, |service, cx| service.portable(cx));
                cx.spawn(async move |this, cx| {
                    let found = task.await;
                    let _ = this.update(cx, |this, cx| {
                        match found {
                            Ok(found) => this.found = Some(found),
                            Err(error) => this.error = Some(error.into()),
                        }
                        cx.notify();
                    });
                })
            }
            None => Task::ready(()),
        };
        Self {
            found: None,
            kind: types(cx).first().map(|kind| kind.key.clone()),
            pending: None,
            error: None,
            _load: load,
        }
    }

    fn port(&mut self, agent: Portable, cx: &mut Context<Self>) {
        let all = types(cx);
        let chosen = self
            .kind
            .as_ref()
            .and_then(|key| all.iter().find(|kind| &kind.key == key))
            .or(all.first())
            .cloned();
        let (Some(chosen), Some(service)) = (chosen, Agents2::global(cx)) else {
            return;
        };
        if self.pending.is_some() {
            return;
        }
        self.pending = Some(agent.agent.pubkey.clone());
        self.error = None;
        let task = service.update(cx, |service, cx| service.port(&agent, &chosen.key, cx));
        let pubkey = agent.agent.pubkey;
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        if let Some(found) = &mut this.found {
                            found.agents.retain(|item| item.agent.pubkey != pubkey);
                        }
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                this.pending = None;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for BringOver {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let all = types(cx);
        let chosen = self
            .kind
            .as_ref()
            .and_then(|key| all.iter().find(|kind| &kind.key == key))
            .or(all.first())
            .map(|kind| kind.key.clone());
        let busy = self.pending.is_some();
        let adopts = all
            .iter()
            .find(|kind| Some(&kind.key) == chosen.as_ref())
            .is_some_and(|kind| kind.kind.adopts());
        let keeps = if adopts {
            "Keeps each agent's key, name, picture and instructions in this community. Its \
             other settings stay behind."
        } else {
            "Keeps each agent's key, name and picture in this community. Its instructions and \
             settings stay behind."
        };
        let kinds = all.iter().map(|kind| {
            let checked = chosen.as_ref() == Some(&kind.key);
            let key = kind.key.clone();
            div()
                .id(SharedString::from(format!("port-type-{}", kind.key)))
                .flex()
                .items_center()
                .gap(rems(0.5))
                .when(!busy, |this| this.cursor_pointer())
                .child(radio_dot(checked, &c))
                .child(div().text_body().child(kind.kind.title()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.pending.is_none() {
                        this.kind = Some(key.clone());
                        cx.notify();
                    }
                }))
        });
        let list: AnyElement = match &self.found {
            None if self.error.is_none() => div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("Looking for agents…")
                .into_any_element(),
            None => div().into_any_element(),
            Some(found) if found.agents.is_empty() => div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("No other agents are saved on this machine.")
                .into_any_element(),
            Some(found) => {
                let labels = crate::ui::keys::public_key_labels(
                    found.agents.iter().map(|a| &a.agent.pubkey),
                );
                let hosts = found
                    .agents
                    .iter()
                    .map(|a| a.host.entity_id())
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                let rows = found.agents.iter().enumerate().map(|(index, item)| {
                    let agent = &item.agent;
                    let mut detail = vec![source_label(agent.source).to_string()];
                    detail.extend(labels.get(&agent.pubkey.to_ascii_lowercase()).cloned());
                    if !agent.relay.is_empty() {
                        detail.push(agent.relay.trim_start_matches("wss://").to_string());
                    }
                    if hosts > 1 {
                        detail.push(item.host.read(cx).label.to_string());
                    }
                    let mut avatar = Avatar::new(agent.name.clone())
                        .size(AvatarSize::Px(32.))
                        .squircle();
                    if let Some(picture) = agent.picture.as_ref().filter(|p| !p.is_empty()) {
                        avatar = avatar.image(crate::ui::media::any_avatar(picture));
                    }
                    let pending = self.pending.as_deref() == Some(agent.pubkey.as_str());
                    let port = item.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.75))
                        .py(rems(0.5))
                        .when(index > 0, |this| {
                            this.border_t_1().border_color(c.border_standard)
                        })
                        .child(div().flex_none().child(avatar))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_body().truncate().child(agent.name.clone()))
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(c.text_subtle)
                                        .truncate()
                                        .child(detail.join(" · ")),
                                ),
                        )
                        .child(
                            Button::new(("bring-over", index), "Bring over")
                                .size(ButtonSize::Sm)
                                .loading(pending)
                                .disabled(chosen.is_none() || (busy && !pending))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.port(port.clone(), cx)),
                                ),
                        )
                });
                div()
                    .flex()
                    .flex_col()
                    .rounded(radius::CONTROL)
                    .children(rows)
                    .into_any_element()
            }
        };
        side_body()
            .key_context("Agents2BringOver")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && this.pending.is_none() {
                    cx.stop_propagation();
                    cx.emit(BringOverEvent::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(rems(0.5))
                    .child(div().min_w_0().text_heading().child("Bring over agents"))
                    .child(
                        IconButton::new("bring-over-close", IconName::X)
                            .size(IconButtonSize::Sm)
                            .tooltip("Close")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(BringOverEvent::Close))),
                    ),
            )
            .child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(format!(
                        "{keeps} Stop it where it runs now first, or both will answer."
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(div().text_label_sm().child("As"))
                    .child(div().flex().flex_col().gap(rems(0.5)).children(kinds)),
            )
            .children(problem(self.error.as_ref(), &c))
            .children(self.found.iter().flat_map(|found| {
                found.warnings.iter().map(|warning| {
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(warning.clone())
                })
            }))
            .child(list)
    }
}
