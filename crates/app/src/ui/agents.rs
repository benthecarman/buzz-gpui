//! The Agents page: agents on this computer and on remote hosts.

use crate::hosts::{HostConnection, Hosts};
use buzz_host::api::{AgentAction, ProcessStatus, Request};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, ButtonVariant, PanelHeader, TypeStyled,
};
use gpui_kit::*;

pub struct AgentsPage {
    hosts: Vec<Entity<HostConnection>>,
    _observers: Vec<Subscription>,
}

impl AgentsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let hosts = cx.global::<Hosts>().hosts.clone();
        let observers = hosts
            .iter()
            .map(|host| cx.observe(host, |_, _, cx| cx.notify()))
            .collect();
        Self {
            hosts,
            _observers: observers,
        }
    }
}

fn status_label(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Stopped => "Stopped",
        ProcessStatus::Waiting => "Waiting",
        ProcessStatus::Starting => "Starting",
        ProcessStatus::Running => "Running",
        ProcessStatus::Stopping => "Stopping",
        ProcessStatus::Failed => "Failed",
    }
}

impl Render for AgentsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors().clone();
        let mut content = div()
            .id("agents")
            .flex()
            .flex_col()
            .gap(px(24.))
            .p(px(24.))
            .flex_1()
            .overflow_y_scroll();
        for host in &self.hosts {
            let state = host.read(cx);
            let connection = match &state.state {
                ConnectionState::Connected { .. } => "Connected".to_string(),
                ConnectionState::Connecting { .. } => "Connecting…".to_string(),
                ConnectionState::Disconnected { error, .. } => format!("Offline: {error}"),
                ConnectionState::Rejected { reason } => format!("Refused: {reason}"),
            };
            let mut tiles = div().flex().flex_wrap().gap(px(12.));
            for agent in &state.agents.agents {
                let running = matches!(
                    agent.status,
                    ProcessStatus::Running | ProcessStatus::Starting
                );
                let action = if running {
                    AgentAction::Stop
                } else {
                    AgentAction::Start
                };
                let host_entity = host.clone();
                let id = agent.id.clone();
                tiles = tiles.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .w(px(240.))
                        .p(px(12.))
                        .rounded(px(16.))
                        .border_1()
                        .border_color(c.border_standard)
                        .bg(c.surface_panel)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    Avatar::new(agent.name.clone())
                                        .squircle()
                                        .size(AvatarSize::Default),
                                )
                                .child(div().text_label_sm().child(agent.name.clone())),
                        )
                        .child(
                            div()
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child(status_label(agent.status)),
                        )
                        .children(agent.error.clone().map(|error| {
                            div().text_caption().text_color(c.text_danger).child(error)
                        }))
                        .child(
                            Button::new(
                                SharedString::from(format!("act-{}", agent.id)),
                                if running { "Stop" } else { "Start" },
                            )
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Outline)
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                host_entity.update(cx, |host, cx| {
                                    host.request(Request::Action { id, action }, cx).detach();
                                });
                            }),
                        ),
                );
            }
            if state.agents.agents.is_empty() {
                tiles = tiles.child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child("No agents on this host yet."),
                );
            }
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(div().text_heading().child(state.label.clone()))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .child(connection),
                            ),
                    )
                    .child(tiles),
            );
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .text_color(c.text_standard)
            .child(
                PanelHeader::new()
                    .joined(true)
                    .label(Some(buzz_ui::IconName::Robot), "Agents"),
            )
            .child(content)
    }
}
