//! The Agents page (ui.md §5a): the agent inventory of this computer and of
//! every remote `buzzd`, one section per host.

mod editor;
mod harness;
mod host_dialog;

use crate::hosts::{HostConnection, Hosts};
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_host::api::{Agent, AgentAction, Harness, ProcessStatus, Request};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, AlertDialog, Avatar, AvatarSize, Badge, BadgeTone, Button, ButtonSize, Dialog,
    Icon, IconButton, IconButtonSize, IconName, Menu, MenuItem, MenuSeparator, MenuSize,
    PanelHeader, Presence, Surface, TypeStyled, tokens::radius,
};
use editor::{AgentEditor, Dismissed};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use host_dialog::AddHost;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// An agent on a host.
type AgentKey = (EntityId, String);

#[derive(Clone, PartialEq)]
enum OpenMenu {
    Page,
    Host(EntityId),
    Agent(EntityId, String),
}

enum Modal {
    Editor {
        editor: Entity<AgentEditor>,
        _subscription: Subscription,
    },
    AddHost {
        dialog: Entity<AddHost>,
        _subscription: Subscription,
    },
    Manage(Entity<HostConnection>, String),
    Delete {
        host: Entity<HostConnection>,
        id: String,
        busy: bool,
        error: Option<SharedString>,
    },
    RemoveHost {
        host: Entity<HostConnection>,
        error: Option<SharedString>,
    },
}

pub struct AgentsPage {
    hosts: Vec<Entity<HostConnection>>,
    _observers: Vec<Subscription>,
    _hosts_observer: Subscription,
    menu: Option<OpenMenu>,
    modal: Option<Modal>,
    /// Agents with a request in flight.
    pending: HashSet<AgentKey>,
    /// The last failed request per agent.
    failures: HashMap<AgentKey, SharedString>,
    /// Hosts whose harness list is open.
    harnesses_open: HashSet<EntityId>,
    /// The content column width, measured after layout, for the tile grid.
    width: Rc<Cell<Pixels>>,
}

impl AgentsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut page = Self {
            hosts: Vec::new(),
            _observers: Vec::new(),
            _hosts_observer: cx.observe_global::<Hosts>(|this, cx| this.sync_hosts(cx)),
            menu: None,
            modal: None,
            pending: HashSet::new(),
            failures: HashMap::new(),
            harnesses_open: HashSet::new(),
            width: Rc::new(Cell::new(px(0.))),
        };
        page.sync_hosts(cx);
        page
    }

    /// Follows hosts added and removed at runtime.
    fn sync_hosts(&mut self, cx: &mut Context<Self>) {
        let hosts = cx.global::<Hosts>().hosts.clone();
        let same = hosts.len() == self.hosts.len()
            && hosts
                .iter()
                .zip(&self.hosts)
                .all(|(a, b)| a.entity_id() == b.entity_id());
        if same {
            return;
        }
        self._observers = hosts
            .iter()
            .map(|host| cx.observe(host, |_, _, cx| cx.notify()))
            .collect();
        self.hosts = hosts;
        cx.notify();
    }

    fn set_menu(&mut self, menu: Option<OpenMenu>, cx: &mut Context<Self>) {
        self.menu = menu;
        cx.notify();
    }

    fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        cx.notify();
    }

    fn dismiss(cx: &mut Context<Self>) -> Dismiss {
        let page = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.set_menu(None, cx));
        })
    }

    /// Sends a request about one agent, tracking it on the agent's card.
    fn act(
        &mut self,
        host: &Entity<HostConnection>,
        id: String,
        request: Request,
        cx: &mut Context<Self>,
    ) {
        let key = (host.entity_id(), id);
        self.pending.insert(key.clone());
        self.failures.remove(&key);
        let response = host.update(cx, |host, cx| host.request(request, cx));
        cx.spawn(async move |this, cx| {
            let result = response.await;
            let _ = this.update(cx, |this, cx| {
                this.pending.remove(&key);
                if let Err(error) = result {
                    this.failures.insert(key, error.to_string().into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_editor(&mut self, editor: Entity<AgentEditor>, cx: &mut Context<Self>) {
        let subscription = cx.subscribe(&editor, |this, _, _: &Dismissed, cx| this.close_modal(cx));
        self.menu = None;
        self.modal = Some(Modal::Editor {
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn add_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hosts = self.hosts.clone();
        let editor = cx.new(|cx| AgentEditor::create(hosts, window, cx));
        self.open_editor(editor, cx);
    }

    fn edit_agent(
        &mut self,
        host: Entity<HostConnection>,
        agent: Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| AgentEditor::edit(host, agent, window, cx));
        self.open_editor(editor, cx);
    }

    fn add_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = cx.new(|cx| AddHost::new(window, cx));
        let subscription = cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close_modal(cx));
        self.menu = None;
        self.modal = Some(Modal::AddHost {
            dialog,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn delete_agent(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Delete { host, id, busy, .. }) = &mut self.modal else {
            return;
        };
        let Some(revision) = find(host, id, cx).map(|agent| agent.revision) else {
            self.modal = None;
            return;
        };
        *busy = true;
        let request = Request::Delete {
            id: id.clone(),
            revision,
        };
        let response = host.update(cx, |host, cx| host.request(request, cx));
        cx.spawn(async move |this, cx| {
            let result = response.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.modal = None,
                    Err(error) => {
                        if let Some(Modal::Delete { busy, error: e, .. }) = &mut this.modal {
                            *busy = false;
                            *e = Some(error.to_string().into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
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

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        for host in &self.hosts {
            host.update(cx, |host, cx| host.refresh(cx).detach());
        }
        cx.notify();
    }
}

fn find<'a>(host: &'a Entity<HostConnection>, id: &str, cx: &'a App) -> Option<&'a Agent> {
    host.read(cx)
        .agents
        .agents
        .iter()
        .find(|agent| agent.id == id)
}

/// Process status text, ui.md §5a.6 (c).
pub(super) fn process_label(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Running => "Process running · relay readiness unverified",
        ProcessStatus::Waiting => "Waiting to start · unlock Keychain if prompted",
        ProcessStatus::Starting => "Starting process",
        ProcessStatus::Stopping => "Stopping process",
        ProcessStatus::Failed => "Process failed",
        ProcessStatus::Stopped => "Process stopped",
    }
}

/// The short form under a tile.
fn status_label(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Running => "Running",
        ProcessStatus::Waiting => "Waiting to start",
        ProcessStatus::Starting => "Starting",
        ProcessStatus::Stopping => "Stopping",
        ProcessStatus::Failed => "Failed",
        ProcessStatus::Stopped => "Stopped",
    }
}

fn presence(status: ProcessStatus) -> Presence {
    match status {
        ProcessStatus::Running => Presence::Online,
        ProcessStatus::Waiting | ProcessStatus::Starting | ProcessStatus::Stopping => {
            Presence::Away
        }
        ProcessStatus::Failed | ProcessStatus::Stopped => Presence::Offline,
    }
}

fn stoppable(status: ProcessStatus) -> bool {
    matches!(
        status,
        ProcessStatus::Running | ProcessStatus::Starting | ProcessStatus::Waiting
    )
}

/// The tile's model line: launch model, saved model, or "Default model".
fn model_line(agent: &Agent) -> String {
    agent
        .launch_model
        .clone()
        .filter(|model| !model.is_empty())
        .or_else(|| Some(agent.harness.model.clone()).filter(|model| !model.is_empty()))
        .unwrap_or_else(|| "Default model".into())
}

fn restart_required(agent: &Agent) -> bool {
    agent.status == ProcessStatus::Running
        && agent
            .running_revision
            .is_some_and(|running| running != agent.revision)
}

/// Columns of the `.agent-grid`: `repeat(auto-fill, minmax(172px, 1fr))`
/// with a 16px gap.
fn columns(width: Pixels) -> u16 {
    ((f32::from(width) + 16.) / 188.).floor().clamp(1., 6.) as u16
}

impl AgentsPage {
    fn header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu_open = self.menu == Some(OpenMenu::Page);
        PanelHeader::new()
            .joined(true)
            .title("Agents")
            .action(
                Button::new("add-host", "Add host")
                    .size(ButtonSize::Sm)
                    .icon(IconName::Plug)
                    .on_click(cx.listener(|this, _, window, cx| this.add_host(window, cx))),
            )
            .action(
                Button::new("add-agent", "Add agent")
                    .size(ButtonSize::Sm)
                    .icon(IconName::Plus)
                    .on_click(cx.listener(|this, _, window, cx| this.add_agent(window, cx))),
            )
            .action(
                div()
                    .relative()
                    .child(
                        IconButton::new("page-actions", IconName::DotsThree)
                            .size(IconButtonSize::Sm)
                            .selected(menu_open)
                            .when(!menu_open, |this| this.tooltip("Agent page actions"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_menu(Some(OpenMenu::Page), cx)
                            })),
                    )
                    .when(menu_open, |this| {
                        this.child(overlay::dropdown(
                            window,
                            Align::End,
                            Menu::new().child(
                                MenuItem::new("refresh", "Refresh agents")
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx))),
                            ),
                            Self::dismiss(cx),
                        ))
                    }),
            )
    }

    fn host_section(
        &self,
        host: &Entity<HostConnection>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = *cx.colors();
        let state = host.read(cx);
        let label = state.label.clone();
        let connection = state.state.clone();
        let snapshot = state.agents.clone();
        let harnesses = state.harnesses.clone().unwrap_or_default();
        let host_id = host.entity_id();
        let remote = state.remote.clone();
        let connected = matches!(connection, ConnectionState::Connected { .. });
        let (badge, tone) = match &connection {
            ConnectionState::Connected { .. } => ("Connected", BadgeTone::Success),
            ConnectionState::Connecting { .. } => ("Connecting", BadgeTone::Neutral),
            ConnectionState::Disconnected { .. } => ("Offline", BadgeTone::Danger),
            ConnectionState::Rejected { .. } => ("Refused", BadgeTone::Warning),
        };
        let menu_open = self.menu == Some(OpenMenu::Host(host_id));

        let heading = div()
            .flex()
            .items_center()
            .gap(rems(0.5))
            .min_w_0()
            .child(
                Icon::new(if remote.is_some() {
                    IconName::Globe
                } else {
                    IconName::Monitor
                })
                .size(18.)
                .color(c.text_subtle),
            )
            .child(div().text_label().truncate().child(label.clone()))
            .children(remote.as_ref().map(|remote| {
                div()
                    .text_caption()
                    .text_color(c.text_subtle)
                    .truncate()
                    .child(remote.address.clone())
            }))
            .child(Badge::new(badge).tone(tone))
            .child(div().flex_1())
            .child(
                div()
                    .relative()
                    .child(
                        IconButton::new(
                            SharedString::from(format!("host-actions-{host_id}")),
                            IconName::DotsThree,
                        )
                        .size(IconButtonSize::Sm)
                        .selected(menu_open)
                        .when(!menu_open, |this| {
                            this.tooltip(format!("Actions for {label}"))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_menu(Some(OpenMenu::Host(host_id)), cx)
                        })),
                    )
                    .when(menu_open, |this| {
                        let reconnect = host.clone();
                        let refresh = host.clone();
                        let remove = host.clone();
                        let mut menu = Menu::new()
                            .child(
                                MenuItem::new("host-refresh", "Refresh agents")
                                    .icon(IconName::ArrowsClockwise)
                                    .icon_size(14.)
                                    .disabled(!connected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.menu = None;
                                        refresh.update(cx, |host, cx| host.refresh(cx).detach());
                                    })),
                            )
                            .child(
                                MenuItem::new("host-reconnect", "Reconnect")
                                    .icon(IconName::Plug)
                                    .icon_size(14.)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.menu = None;
                                        reconnect.read(cx).reconnect();
                                        cx.notify();
                                    })),
                            );
                        if remote.is_some() {
                            menu = menu.child(MenuSeparator::new()).child(
                                MenuItem::new("host-remove", "Remove host")
                                    .icon(IconName::Trash)
                                    .icon_size(14.)
                                    .danger()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.menu = None;
                                        this.modal = Some(Modal::RemoveHost {
                                            host: remove.clone(),
                                            error: None,
                                        });
                                        cx.notify();
                                    })),
                            );
                        }
                        this.child(overlay::dropdown(
                            window,
                            Align::End,
                            menu,
                            Self::dismiss(cx),
                        ))
                    }),
            );

        // Inline status lines (§5a.9): bare paragraphs at body size.
        let line = |text: String| div().text_body().child(text);
        let mut alerts = div().flex().flex_col().gap(rems(0.5));
        match &connection {
            ConnectionState::Connecting { attempt } if *attempt > 1 || remote.is_some() => {
                alerts = alerts.child(line(format!("Connecting to {label}…")));
            }
            ConnectionState::Connecting { .. } => {
                alerts = alerts.child(line("Reading local agent status…".into()));
            }
            ConnectionState::Disconnected { error, retry_in } => {
                let retry = host.clone();
                alerts = alerts
                    .child(
                        line(format!(
                            "{label} is offline: {error}. Retrying in {} s.",
                            retry_in.as_secs().max(1)
                        ))
                        .text_color(c.text_danger),
                    )
                    .when(!snapshot.agents.is_empty(), |this| {
                        this.child(
                            div()
                                .text_body_sm()
                                .text_color(c.text_subtle)
                                .child("The agent statuses below may be out of date."),
                        )
                    })
                    .child(
                        div().flex().child(
                            Button::new(
                                SharedString::from(format!("retry-{host_id}")),
                                "Retry connection",
                            )
                            .size(ButtonSize::Sm)
                            .on_click(move |_, _, cx| retry.read(cx).reconnect()),
                        ),
                    );
            }
            ConnectionState::Rejected { reason } => {
                let retry = host.clone();
                alerts = alerts
                    .child(
                        line(format!("{label} refused this app: {reason}"))
                            .text_color(c.text_danger),
                    )
                    .child(div().text_body_sm().text_color(c.text_subtle).child(
                        "Check the token with `buzzd token` on that machine, then \
                                 remove and add the host again.",
                    ))
                    .child(
                        div().flex().child(
                            Button::new(
                                SharedString::from(format!("retry-{host_id}")),
                                "Retry connection",
                            )
                            .size(ButtonSize::Sm)
                            .on_click(move |_, _, cx| retry.read(cx).reconnect()),
                        ),
                    );
            }
            ConnectionState::Connected { .. } => {}
        }
        if connected && !snapshot.runtime_available {
            alerts =
                alerts.child(
                    line(snapshot.runtime_message.clone().unwrap_or_else(|| {
                        "The agent runtime is unavailable on this host.".into()
                    }))
                    .text_color(c.text_danger),
                );
        }
        if remote.as_ref().is_some_and(|remote| remote.token.is_some()) {
            alerts = alerts.child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("This host's token is saved in config.toml, not the OS keyring."),
            );
        }

        let mut agents: Vec<&Agent> = snapshot.agents.iter().collect();
        agents.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.pubkey.cmp(&b.pubkey))
        });
        let body: AnyElement = if agents.is_empty() {
            if connected {
                div()
                    .text_body()
                    .child("No agents yet. Add an agent to get started.")
                    .into_any_element()
            } else {
                div().into_any_element()
            }
        } else {
            let mut cards = Vec::new();
            for agent in agents {
                cards.push(
                    self.card(host, agent, &harnesses, connected, window, cx)
                        .into_any_element(),
                );
            }
            div()
                .grid()
                .grid_cols(columns(self.width.get()))
                .gap(rems(1.))
                .children(cards)
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .child(heading)
            .child(alerts)
            .child(body)
            .when(connected, |this| this.child(self.harnesses(host, cx)))
    }

    fn harnesses(&self, host: &Entity<HostConnection>, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let host_id = host.entity_id();
        let Some(list) = host.read(cx).harnesses.clone() else {
            return div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("Checking harnesses…")
                .into_any_element();
        };
        let open = self.harnesses_open.contains(&host_id);
        let ready = list.iter().filter(|h| h.missing.is_empty()).count();
        let toggle = div()
            .id(SharedString::from(format!("harnesses-{host_id}")))
            .flex()
            .items_center()
            .gap(rems(0.25))
            .min_h(rems(2.))
            .cursor_pointer()
            .text_label_sm()
            .child(format!("Harnesses · {ready} of {} ready", list.len()))
            .child(
                Icon::new(if open {
                    IconName::CaretUp
                } else {
                    IconName::CaretDown
                })
                .size(16.)
                .color(c.text_subtle),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.harnesses_open.remove(&host_id) {
                    this.harnesses_open.insert(host_id);
                }
                cx.notify();
            }));
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .child(toggle)
            .when(open, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .px(rems(1.))
                        .rounded(radius::CONTAINER)
                        .border_1()
                        .border_color(c.border_standard)
                        .children(list.iter().enumerate().map(|(index, harness)| {
                            harness_row(harness, index > 0, &c).into_any_element()
                        })),
                )
            })
            .into_any_element()
    }

    fn card(
        &self,
        host: &Entity<HostConnection>,
        agent: &Agent,
        harnesses: &[Harness],
        connected: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let c = *cx.colors();
        let host_id = host.entity_id();
        let key = (host_id, agent.id.clone());
        let busy = self.pending.contains(&key);
        let menu_open = self.menu == Some(OpenMenu::Agent(host_id, agent.id.clone()));
        let element_id = |part: &str| SharedString::from(format!("{part}-{host_id}-{}", agent.id));

        let mut avatar = Avatar::new(agent.name.clone())
            .squircle()
            .size(AvatarSize::Px(80.))
            .presence(Some(presence(agent.status)));
        if let Some(picture) = agent.picture.clone().filter(|p| !p.is_empty()) {
            avatar = avatar.image(SharedString::from(picture));
        }
        let kind = harness::kind_of(&agent.harness.command, harnesses);
        let status_color = match agent.status {
            ProcessStatus::Failed => c.text_danger,
            _ => c.text_subtle,
        };
        let manage = {
            let host = host.clone();
            let id = agent.id.clone();
            cx.listener(move |this, _, _, cx| {
                this.menu = None;
                this.modal = Some(Modal::Manage(host.clone(), id.clone()));
                cx.notify();
            })
        };
        let content = div()
            .id(element_id("open"))
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(rems(0.75))
            .px(rems(0.75))
            .pt(rems(2.))
            .pb(rems(1.))
            .rounded(radius::CONTAINER)
            .cursor_pointer()
            .hover(|style| style.bg(c.affordance_subtle_hover))
            .on_click(manage)
            .child(avatar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .w_full()
                            .text_label_sm()
                            .text_center()
                            .truncate()
                            .child(agent.name.clone()),
                    )
                    .when(restart_required(agent), |this| {
                        this.child(
                            div().mt(rems(0.25)).child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(rems(0.25))
                                    .px(rems(0.5))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(c.border_warning)
                                    .bg(c.affordance_warning)
                                    .text_caption()
                                    .text_color(c.text_warning)
                                    .child(Icon::new(IconName::ArrowsClockwise).size(14.))
                                    .child("Restart required"),
                            ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(rems(0.25))
                            .w_full()
                            .mt(rems(0.25))
                            .text_caption()
                            .text_color(c.text_subtle)
                            .children(kind.map(|kind| harness::mark(kind, 14.)))
                            .child(div().min_w_0().truncate().child(model_line(agent))),
                    )
                    .child(
                        div()
                            .mt(rems(0.25))
                            .text_caption()
                            .text_color(status_color)
                            .child(if connected {
                                status_label(agent.status).to_string()
                            } else {
                                format!("Last known: {}", status_label(agent.status))
                            }),
                    ),
            );

        let failure = self.failures.get(&key).cloned();
        let problem = failure
            .clone()
            .or_else(|| agent.error.clone().map(SharedString::from));
        let action_button = |id: SharedString, label: &'static str, action: AgentAction| {
            let host = host.clone();
            let agent_id = agent.id.clone();
            Button::new(id, label)
                .size(ButtonSize::Sm)
                .disabled(busy || !connected)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let request = Request::Action {
                        id: agent_id.clone(),
                        action,
                    };
                    this.act(&host, agent_id.clone(), request, cx);
                }))
        };
        let actions = if stoppable(agent.status) {
            div()
                .flex()
                .gap(rems(0.5))
                .child(action_button(element_id("stop"), "Stop", AgentAction::Stop))
                .when(agent.status == ProcessStatus::Running, |this| {
                    this.child(action_button(
                        element_id("restart"),
                        if restart_required(agent) {
                            "Restart to apply"
                        } else {
                            "Restart"
                        },
                        AgentAction::Restart,
                    ))
                })
        } else {
            let label = if agent.status == ProcessStatus::Failed {
                "Retry start"
            } else {
                "Start"
            };
            div().flex().gap(rems(0.5)).child(
                action_button(element_id("start"), label, AgentAction::Start)
                    .prominent()
                    .disabled(busy || !connected || agent.status == ProcessStatus::Stopping),
            )
        };
        let footer = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(rems(0.5))
            .px(rems(1.))
            .pb(rems(1.))
            .children(problem.map(|text| {
                div()
                    .w_full()
                    .text_caption()
                    .text_center()
                    .text_color(c.text_danger)
                    .line_clamp(3)
                    .child(text)
            }))
            .when(agent.profile_pending, |this| {
                this.child(
                    div()
                        .text_caption()
                        .text_center()
                        .text_color(c.text_subtle)
                        .child("Profile publication is pending."),
                )
            })
            .child(actions);

        let menu = self.agent_menu(host, agent, menu_open, window, cx);
        div()
            .relative()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h(rems(15.25))
            .rounded(radius::CONTAINER)
            .border_1()
            .border_color(c.border_standard)
            .child(content)
            .child(footer)
            .child(div().absolute().top(rems(0.5)).right(rems(0.5)).child(menu))
    }

    fn agent_menu(
        &self,
        host: &Entity<HostConnection>,
        agent: &Agent,
        open: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let host_id = host.entity_id();
        let agent_id = agent.id.clone();
        let trigger = IconButton::new(
            SharedString::from(format!("agent-actions-{host_id}-{agent_id}")),
            IconName::DotsThree,
        )
        .size(IconButtonSize::Sm)
        .selected(open)
        .when(!open, |this| {
            this.tooltip(format!("Actions for {}", agent.name))
        })
        .on_click(cx.listener({
            let agent_id = agent_id.clone();
            move |this, _, _, cx| {
                this.set_menu(Some(OpenMenu::Agent(host_id, agent_id.clone())), cx)
            }
        }));
        let menu = open.then(|| {
            let item = |id: &'static str, label: &'static str| {
                MenuItem::new(id, label).size(MenuSize::Default)
            };
            let manage = {
                let host = host.clone();
                let id = agent_id.clone();
                cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.modal = Some(Modal::Manage(host.clone(), id.clone()));
                    cx.notify();
                })
            };
            let edit = {
                let host = host.clone();
                let agent = agent.clone();
                cx.listener(move |this, _, window, cx| {
                    this.edit_agent(host.clone(), agent.clone(), window, cx)
                })
            };
            let launch = {
                let host = host.clone();
                let id = agent_id.clone();
                let value = !agent.start_on_app_launch;
                cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    let request = Request::SetStartOnLaunch {
                        id: id.clone(),
                        value,
                    };
                    this.act(&host, id.clone(), request, cx);
                })
            };
            let publish = {
                let host = host.clone();
                let id = agent_id.clone();
                cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    let request = Request::PublishProfile { id: id.clone() };
                    this.act(&host, id.clone(), request, cx);
                })
            };
            let delete = {
                let host = host.clone();
                let id = agent_id.clone();
                cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.modal = Some(Modal::Delete {
                        host: host.clone(),
                        id: id.clone(),
                        busy: false,
                        error: None,
                    });
                    cx.notify();
                })
            };
            let connected = matches!(host.read(cx).state, ConnectionState::Connected { .. });
            let menu = Menu::new()
                .size(MenuSize::Wide)
                .child(item("manage", "Manage agent").on_click(manage))
                .child(MenuSeparator::new())
                .child(
                    item("edit", "Edit")
                        .icon(IconName::PencilSimple)
                        .icon_size(14.)
                        .disabled(!connected)
                        .on_click(edit),
                )
                .child(
                    item("launch", "Start on launch")
                        .icon(IconName::Lightning)
                        .icon_size(14.)
                        .checked(agent.start_on_app_launch)
                        .disabled(!connected)
                        .on_click(launch),
                )
                .when(agent.profile_pending, |this| {
                    this.child(
                        item("publish", "Retry profile publication")
                            .icon(IconName::ArrowsClockwise)
                            .icon_size(14.)
                            .disabled(!connected)
                            .on_click(publish),
                    )
                })
                .child(MenuSeparator::new())
                .child(
                    item("delete", "Delete")
                        .icon(IconName::Trash)
                        .icon_size(14.)
                        .danger()
                        .disabled(!connected)
                        .on_click(delete),
                );
            overlay::dropdown(window, Align::End, menu, Self::dismiss(cx))
        });
        div().relative().occlude().child(trigger).children(menu)
    }

    fn manage_dialog(
        &self,
        host: &Entity<HostConnection>,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let c = *cx.colors();
        let agent = find(host, id, cx)?.clone();
        let state = host.read(cx);
        let label = state.label.clone();
        let connected = matches!(state.state, ConnectionState::Connected { .. });
        let runtime_blocked = connected && !state.agents.runtime_available;
        let key = (host.entity_id(), agent.id.clone());
        let busy = self.pending.contains(&key);
        let close = cx.listener(|this, _, _, cx| this.close_modal(cx));
        let line = |text: SharedString| div().text_body_sm().child(text);
        let button = |label: &'static str, request: Request| {
            let host = host.clone();
            let id = agent.id.clone();
            Button::new(SharedString::from(format!("manage-{label}")), label)
                .size(ButtonSize::Sm)
                .surface(Surface::Floating)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.act(&host, id.clone(), request.clone(), cx)
                }))
        };
        let action = |action| Request::Action {
            id: agent.id.clone(),
            action,
        };
        let start_label = if agent.status == ProcessStatus::Failed {
            "Retry start"
        } else {
            "Start"
        };
        let block_reason = if !connected {
            Some(format!("{label} is not connected."))
        } else if runtime_blocked {
            Some("The agent runtime is unavailable on this host.".into())
        } else if agent.status == ProcessStatus::Stopping {
            Some("Waiting for the process transition.".into())
        } else {
            None
        };
        let dialog = Dialog::new("manage-agent", format!("Manage {}", agent.name))
            .on_close(close)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(format!("{} · {label}", agent.relay_url)),
                    )
                    .child(line(
                        if connected {
                            process_label(agent.status).to_string()
                        } else {
                            format!("Last known: {}", process_label(agent.status))
                        }
                        .into(),
                    ))
                    .when(agent.start_on_app_launch, |this| {
                        this.child(line("Starts with this app.".into()))
                    })
                    .children(
                        agent
                            .error
                            .clone()
                            .map(|error| line(error.into()).text_color(c.text_danger)),
                    )
                    .children(
                        self.failures
                            .get(&key)
                            .cloned()
                            .map(|error| line(error).text_color(c.text_danger)),
                    )
                    .children(agent.diagnostics.iter().map(|diagnostic| {
                        div()
                            .text_mono()
                            .text_color(c.text_subtle)
                            .child(diagnostic.clone())
                    }))
                    .when(agent.profile_pending, |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(rems(0.5))
                                .child(line(
                                    "Settings saved. Profile publication is pending.".into(),
                                ))
                                .child(
                                    button(
                                        "Retry profile",
                                        Request::PublishProfile {
                                            id: agent.id.clone(),
                                        },
                                    )
                                    .disabled(busy || !connected),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(rems(0.5))
                            .when(!stoppable(agent.status), |this| {
                                this.child(
                                    button(start_label, action(AgentAction::Start))
                                        .prominent()
                                        .disabled(busy || block_reason.is_some()),
                                )
                            })
                            .child(
                                button("Stop", action(AgentAction::Stop))
                                    .disabled(busy || !connected || !stoppable(agent.status)),
                            )
                            .when(agent.status == ProcessStatus::Running, |this| {
                                this.child(
                                    button(
                                        if restart_required(&agent) {
                                            "Restart to apply"
                                        } else {
                                            "Restart"
                                        },
                                        action(AgentAction::Restart),
                                    )
                                    .disabled(busy || !connected),
                                )
                            }),
                    )
                    .children(block_reason.map(|reason| {
                        div().text_body_sm().text_color(c.text_subtle).child(reason)
                    })),
            );
        Some(dialog.into_any_element())
    }

    fn modal(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let page = cx.entity().downgrade();
        let close: Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.close_modal(cx));
        });
        let (content, dismiss): (AnyElement, Option<Dismiss>) =
            match self.modal.as_ref()? {
                Modal::Editor { editor, .. } => (editor.clone().into_any_element(), None),
                Modal::AddHost { dialog, .. } => (dialog.clone().into_any_element(), None),
                Modal::Manage(host, id) => (self.manage_dialog(host, id, cx)?, Some(close)),
                Modal::Delete {
                    host,
                    id,
                    busy,
                    error,
                } => {
                    let name = find(host, id, cx)
                        .map(|agent| agent.name.clone())
                        .unwrap_or_else(|| "this agent".into());
                    let dialog = AlertDialog::new(format!("Delete {name}?"))
                        .description(
                            "This stops the agent and removes its settings and app credential \
                         from its host. Its relay identity and past messages remain visible. \
                         This cannot be undone here.",
                        )
                        .children(error.clone().map(|error| {
                            div().text_body_sm().text_color(c.text_danger).child(error)
                        }))
                        .action(
                            Button::new("cancel-delete", "Cancel")
                                .surface(Surface::Floating)
                                .disabled(*busy)
                                .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                        )
                        .action(
                            Button::new("confirm-delete", "Delete agent")
                                .destructive()
                                .loading(*busy)
                                .on_click(cx.listener(|this, _, _, cx| this.delete_agent(cx))),
                        );
                    (dialog.into_any_element(), (!*busy).then_some(close))
                }
                Modal::RemoveHost { host, error } => {
                    let state = host.read(cx);
                    let label = state.label.clone();
                    let address = state
                        .remote
                        .as_ref()
                        .map(|remote| remote.address.clone())
                        .unwrap_or_default();
                    let dialog = AlertDialog::new(format!("Remove {label}?"))
                        .description(format!(
                            "This disconnects from {address} and forgets its token. Agents on \
                         that host keep running, and you can add it again later."
                        ))
                        .children(error.clone().map(|error| {
                            div().text_body_sm().text_color(c.text_danger).child(error)
                        }))
                        .action(
                            Button::new("cancel-remove", "Cancel")
                                .surface(Surface::Floating)
                                .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                        )
                        .action(
                            Button::new("confirm-remove", "Remove host")
                                .destructive()
                                .on_click(cx.listener(|this, _, _, cx| this.remove_host(cx))),
                        );
                    (dialog.into_any_element(), Some(close))
                }
            };
        Some(overlay::modal(window, content, dismiss).into_any_element())
    }
}

fn harness_row(harness: &Harness, divided: bool, c: &buzz_ui::Colors) -> impl IntoElement {
    let ready = harness.missing.is_empty();
    div()
        .flex()
        .items_center()
        .gap(rems(1.))
        .py(rems(0.75))
        .text_body_sm()
        .when(divided, |this| {
            this.border_t_1().border_color(c.border_standard)
        })
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(rems(0.75))
                .child(harness::mark(harness.kind, 32.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().text_label_sm().child(harness.label.clone()))
                        .when(!ready, |this| {
                            this.child(
                                div()
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .child(harness.setup_hint.clone()),
                            )
                        }),
                ),
        )
        .child(
            div()
                .flex_none()
                .max_w(rems(16.))
                .text_right()
                .text_color(c.text_subtle)
                .child(if ready {
                    "Ready".to_string()
                } else {
                    format!("Needs {}", harness.missing.join(", "))
                }),
        )
}

impl Render for AgentsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        // Re-render when the measured width changes the column count.
        let width = self.width.clone();
        let page = cx.entity().downgrade();
        let measure = canvas(
            move |bounds, _, cx| {
                let old = width.replace(bounds.size.width);
                if columns(old) != columns(bounds.size.width) {
                    let _ = page.update(cx, |_, cx| cx.notify());
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let sections: Vec<AnyElement> = self
            .hosts
            .clone()
            .iter()
            .map(|host| self.host_section(host, window, cx).into_any_element())
            .collect();
        let modal = self.modal(window, cx);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .text_color(c.text_standard)
            .child(self.header(window, cx))
            .child(
                div()
                    .id("agents")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(rems(1.5))
                    .text_body()
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .gap(rems(2.))
                            .w_full()
                            .max_w(rems(72.))
                            .mx_auto()
                            .child(measure)
                            .children(sections),
                    ),
            )
            .children(modal)
    }
}
