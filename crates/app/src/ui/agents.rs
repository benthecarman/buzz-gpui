//! The Agents page (ui.md §5a): the agent inventory of this computer and of
//! every remote `buzzd`, one section per host, then what only the
//! community relays know.
//!
//! buzz-app's `AgentsPage`, `AgentControlPanel`, `UnifiedInventory` and
//! `InventoryView`. Each identity is listed once, in the section of the host
//! that holds it; relay-only identities, archived ones and profiles without
//! identities follow the hosts.

mod card;
mod create;
mod defaults;
mod dialogs;
mod discovery;
mod draft;
mod edit;
mod editor;
mod fields;
mod form;
pub(crate) mod harness;
mod host_dialog;
mod import;
mod inventory;
mod relay;
pub(crate) mod review;
pub(crate) mod wake;
mod share;
pub(crate) mod snapshot_preview;
pub(crate) mod snapshots;

pub(crate) use defaults::{DefaultsCard, DefaultsDialog};
pub(crate) use editor::{Dismissed, Saved};
pub(crate) use host_dialog::AddHost;

use crate::community::AppState;
use crate::hosts::control::Status;
use crate::hosts::{HostConnection, Hosts};
use crate::ui::overlay::{self, Align, Dismiss};
use crate::ui::settings::OpenHostSettings;
use buzz_host::api::{Agent, ImportSource, ProcessStatus};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, Badge, BadgeTone, Button, ButtonSize, Icon, IconButton, IconButtonSize, IconName,
    Menu, MenuItem, MenuSeparator, PanelHeader, Toast, ToastTone, TypeStyled, tokens::radius,
};
use discovery::{Discovery, ReadStatus};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use inventory::{Decision, Group, Row};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

/// An agent on a host.
type AgentKey = (EntityId, String);

#[derive(Clone, PartialEq)]
enum OpenMenu {
    Page,
    Host(EntityId),
    /// A card's actions, by identity.
    Card(String),
    /// A card's old Buzz installation choice.
    Source(String),
}

/// Use here or Clone, for one identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handover {
    Use,
    Clone,
}

enum Modal {
    Create {
        dialog: Entity<create::CreateDialog>,
        _subscriptions: Vec<Subscription>,
    },
    Edit {
        dialog: Entity<edit::EditDialog>,
        _subscriptions: Vec<Subscription>,
    },
    Defaults {
        dialog: Entity<DefaultsDialog>,
        _subscriptions: [Subscription; 2],
    },
    /// The tile's Manage dialog, for every setup of one identity.
    Manage(String),
    Delete {
        host: Entity<HostConnection>,
        id: String,
        busy: bool,
        error: Option<SharedString>,
    },
    Archive {
        pubkey: String,
        name: String,
        running: bool,
    },
    Remove {
        pubkey: String,
        name: String,
    },
    Handover {
        pubkey: String,
        action: Handover,
        /// The setup a Use here finishes, or a Clone reads.
        setup: Option<(Entity<HostConnection>, String)>,
        /// An old Buzz install a Clone reads instead.
        source: Option<(Entity<HostConnection>, ImportSource)>,
        pending: bool,
        error: Option<SharedString>,
    },
    Import {
        dialog: Entity<import::ImportDialog>,
        _subscriptions: [Subscription; 2],
    },
}

/// What `useManagedAgentActions` remembers about a failed Start or Stop.
#[derive(Clone, Debug)]
struct Outcome {
    status: ProcessStatus,
    version: Option<u64>,
    checking: bool,
    notice: Option<SharedString>,
}

struct PageToast {
    text: SharedString,
    generation: usize,
    /// Archive's Undo.
    undo: Option<String>,
}

pub struct AgentsPage {
    hosts: Vec<Entity<HostConnection>>,
    discovery: Entity<Discovery>,
    _observers: Vec<Subscription>,
    _hosts_observer: Subscription,
    _app_observer: Subscription,
    menu: Option<OpenMenu>,
    modal: Option<Modal>,
    outcomes: HashMap<AgentKey, Outcome>,
    /// The agent an import just added: its card asks for review.
    imported: Option<AgentKey>,
    /// The installation chosen per identity, when several hold it.
    sources: HashMap<String, ImportSource>,
    /// Identities whose "Identity & sources" details are open.
    details: HashSet<String>,
    archived_open: Option<bool>,
    was_all_archived: bool,
    /// Hosts whose team repair accordion is open.
    repair_open: HashSet<EntityId>,
    /// Each host's repair list, once opened.
    repairs: HashMap<EntityId, Entity<import::ImportDialog>>,
    /// The content column width, measured after layout, for the tile grid.
    width: Rc<Cell<Pixels>>,
    toast: Option<PageToast>,
    toasts: usize,
    /// Holds focus inside the page's own dialogs so Escape reaches them.
    modal_focus: FocusHandle,
}

const TOAST_DURATION: std::time::Duration = std::time::Duration::from_secs(5);

/// Columns of the `.agent-grid`: `repeat(auto-fill, minmax(172px, 1fr))`
/// with a 16px gap.
fn columns(width: Pixels) -> u16 {
    ((f32::from(width) + 16.) / 188.).floor().clamp(1., 6.) as u16
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

/// The tile's model line: launch model, an environment override, the saved
/// model, or "Default model".
fn model_line(agent: &Agent) -> String {
    if let Some(model) = agent.launch_model.clone().filter(|m| !m.is_empty()) {
        return model;
    }
    if agent.launch_model_env.is_some() {
        return "Custom model".into();
    }
    Some(agent.harness.model.clone())
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| "Default model".into())
}

fn restart_required(agent: &Agent) -> bool {
    !agent.restart_diff.is_empty()
}

/// Everything the inventory shows, joined for one render.
struct View {
    destination: String,
    rows: BTreeMap<String, Row>,
    archived: BTreeSet<String>,
}

impl View {
    fn decide(&self, row: &Row) -> Decision {
        inventory::decide(row, &self.destination)
    }

    /// The host whose section lists the identity.
    fn host_of(&self, row: &Row, decision: &Decision) -> Option<usize> {
        match decision.group {
            Group::LocalHere => row
                .setups
                .get(&self.destination)
                .map(|setup| setup.host)
                .or_else(|| row.unconfigured.first().map(|setup| setup.host))
                .or(row.local.as_ref().map(|(host, _)| *host)),
            Group::LocalOther => row.local.as_ref().map(|(host, _)| *host),
            Group::Import => row.sources.first().map(|(host, _)| *host),
            Group::Relay => None,
        }
    }
}

impl AgentsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let discovery = cx.new(Discovery::new);
        let mut page = Self {
            hosts: Vec::new(),
            _observers: vec![cx.observe(&discovery, |_, _, cx| cx.notify())],
            discovery,
            _hosts_observer: cx.observe_global::<Hosts>(|this, cx| this.sync_hosts(cx)),
            _app_observer: cx.observe_global::<AppState>(|this, cx| this.sync_community(cx)),
            menu: None,
            modal: None,
            outcomes: HashMap::new(),
            imported: None,
            sources: HashMap::new(),
            details: HashSet::new(),
            archived_open: None,
            was_all_archived: false,
            repair_open: HashSet::new(),
            repairs: HashMap::new(),
            width: Rc::new(Cell::new(px(0.))),
            toast: None,
            toasts: 0,
            modal_focus: cx.focus_handle(),
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
        let mut observers: Vec<Subscription> = hosts
            .iter()
            .map(|host| cx.observe(host, |this, _, cx| this.host_changed(cx)))
            .collect();
        observers.push(cx.observe(&self.discovery, |_, _, cx| cx.notify()));
        self._observers = observers;
        self.hosts = hosts;
        let hosts = self.hosts.clone();
        self.discovery
            .update(cx, |discovery, cx| discovery.refresh(&hosts, cx));
        cx.notify();
    }

    /// A different community or identity: discovery starts over, and
    /// archive Undo offers end with the view they belonged to.
    fn sync_community(&mut self, cx: &mut Context<Self>) {
        let hosts = self.hosts.clone();
        let before = self.discovery.read(cx).destination();
        self.discovery
            .update(cx, |discovery, cx| discovery.sync(&hosts, cx));
        if self.discovery.read(cx).destination() != before {
            self.toast = None;
            self.sources.clear();
        }
        cx.notify();
    }

    fn host_changed(&mut self, cx: &mut Context<Self>) {
        // Notices describe one attempt; a newer action or status retires
        // them (`useManagedAgentActions`).
        let hosts = self.hosts.clone();
        self.outcomes.retain(|(host_id, id), outcome| {
            hosts
                .iter()
                .find(|host| host.entity_id() == *host_id)
                .is_some_and(|host| {
                    let host = host.read(cx);
                    host.control.action_versions.get(id).copied() == outcome.version
                        && host
                            .agent(id)
                            .is_some_and(|agent| agent.status == outcome.status)
                })
        });
        if let Some((host_id, id)) = &self.imported
            && self.hosts.iter().any(|host| {
                host.entity_id() == *host_id
                    && host.read(cx).agent(id).is_some_and(|agent| agent.enabled)
            })
        {
            self.imported = None;
        }
        self.follow_presence(cx);
        cx.notify();
    }

    /// Follows the presence of every agent key on the page.
    fn follow_presence(&self, cx: &mut Context<Self>) {
        let Some(community) = AppState::current(cx) else {
            return;
        };
        let mut keys: Vec<String> = self
            .hosts
            .iter()
            .flat_map(|host| {
                host.read(cx)
                    .agents
                    .agents
                    .iter()
                    .map(|agent| agent.pubkey.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        keys.sort();
        keys.dedup();
        community.update(cx, |community, _| community.set_presence_extra(keys));
    }

    fn set_menu(&mut self, menu: Option<OpenMenu>, cx: &mut Context<Self>) {
        self.menu = menu;
        cx.notify();
    }

    fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        cx.notify();
    }

    fn show_toast(&mut self, text: SharedString, undo: Option<String>, cx: &mut Context<Self>) {
        self.toasts += 1;
        let generation = self.toasts;
        self.toast = Some(PageToast {
            text,
            generation,
            undo,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .toast
                    .as_ref()
                    .is_some_and(|toast| toast.generation == generation)
                {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss(cx: &mut Context<Self>) -> Dismiss {
        let page = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.set_menu(None, cx));
        })
    }

    fn host(&self, ix: usize) -> Option<&Entity<HostConnection>> {
        self.hosts.get(ix)
    }

    /// Add agent, Duplicate or Clone. `host` defaults to the first
    /// connected one.
    fn create_dialog(
        &mut self,
        host: Option<Entity<HostConnection>>,
        kind: create::Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hosts = self.hosts.clone();
        let Some(host) = host.or_else(|| {
            hosts
                .iter()
                .find(|host| editor::connected(host, cx))
                .or(hosts.first())
                .cloned()
        }) else {
            return;
        };
        let destination = self.discovery.read(cx).destination();
        let dialog =
            cx.new(|cx| create::CreateDialog::new(hosts, host, kind, destination, window, cx));
        let subscriptions = vec![
            cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close_modal(cx)),
            cx.subscribe(&dialog, |this, _, create::Created(host, id), _| {
                this.imported = Some((host.entity_id(), id.clone()));
            }),
            cx.subscribe_in(
                &dialog,
                window,
                |this, _, _: &create::OpenHarnesses, window, cx| {
                    this.close_modal(cx);
                    window.dispatch_action(Box::new(OpenHostSettings), cx);
                },
            ),
        ];
        self.menu = None;
        self.modal = Some(Modal::Create {
            dialog,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn add_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.create_dialog(None, create::Kind::Add, window, cx);
    }

    fn edit_agent(
        &mut self,
        host: Entity<HostConnection>,
        agent: Agent,
        name: String,
        initial: Option<draft::Draft>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog =
            cx.new(|cx| edit::EditDialog::new(host, agent, name, initial, None, window, cx));
        let subscriptions = vec![
            cx.subscribe(&dialog, |this, _, _: &Dismissed, cx| this.close_modal(cx)),
            cx.subscribe(&dialog, |this, _, Saved(text), cx| {
                this.show_toast(text.clone(), None, cx)
            }),
            cx.subscribe_in(
                &dialog,
                window,
                |this, _, _: &create::OpenHarnesses, window, cx| {
                    this.close_modal(cx);
                    window.dispatch_action(Box::new(OpenHostSettings), cx);
                },
            ),
        ];
        self.menu = None;
        self.modal = Some(Modal::Edit {
            dialog,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

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
                this.show_toast(text.clone(), None, cx)
            }),
        ];
        self.menu = None;
        self.modal = Some(Modal::Defaults {
            dialog,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Refresh agents: every host's status and every relay read.
    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        for host in &self.hosts {
            host.update(cx, |host, cx| host.refresh_status(cx).detach());
        }
        let hosts = self.hosts.clone();
        self.discovery
            .update(cx, |discovery, cx| discovery.refresh(&hosts, cx));
        cx.notify();
    }

    /// Names as the page shows them: the community's profile name, else the
    /// name a membership's relay supplied, else the saved one.
    fn view(&self, cx: &App) -> View {
        let discovery = self.discovery.read(cx);
        let destination = discovery.destination();
        let profiles =
            AppState::current(cx).map(|community| community.read(cx).snapshot.profiles.clone());
        let snapshots: Vec<_> = self
            .hosts
            .iter()
            .map(|host| &host.read(cx).agents)
            .collect();
        let (relay_identities, _) = relay::tiles(&discovery.library);
        let resolve = |key: &str, fallback: &str| {
            profiles
                .as_ref()
                .and_then(|profiles| profiles.get(key))
                .filter(|profile| !profile.name_is_fallback)
                .map(|profile| profile.name.clone())
                .or_else(|| {
                    discovery
                        .source_profile(key)
                        .filter(|(_, profile)| !profile.name_is_fallback)
                        .map(|(_, profile)| profile.name)
                })
                .unwrap_or_else(|| fallback.to_string())
        };
        let mut rows = inventory::identities(
            if discovery.library_status == ReadStatus::Ready {
                &relay_identities
            } else {
                &[]
            },
            &discovery.known,
            &snapshots,
            resolve,
        );
        // A configured setup's own name labels it; collisions get a suffix.
        let names: BTreeMap<String, String> = rows
            .iter()
            .map(|(key, row)| {
                let name = row
                    .local_setups(&destination)
                    .first()
                    .map(|setup| setup.agent.name.clone())
                    .or_else(|| {
                        row.unconfigured
                            .first()
                            .map(|setup| setup.agent.name.clone())
                    })
                    .unwrap_or_else(|| row.display_name.clone());
                (key.clone(), name)
            })
            .collect();
        let labels = inventory::disambiguate(&names);
        for (key, row) in rows.iter_mut() {
            if let Some(label) = labels.get(key) {
                row.display_name = label.clone();
            }
        }
        let mut archived = BTreeSet::new();
        rows.retain(|key, row| {
            let removed = discovery.removed.contains(key) && row.local.is_none();
            if !removed {
                let hidden = match discovery.runs.get(key) {
                    // A request keeps the card in the section it started in.
                    Some(run) => !run.archive,
                    None => discovery.archived(key),
                };
                if hidden {
                    archived.insert(key.clone());
                }
            }
            !removed
        });
        View {
            destination,
            rows,
            archived,
        }
    }
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
                    .on_click(|_, window, cx| {
                        // Hosts are managed in Settings → Agents.
                        window.dispatch_action(Box::new(OpenHostSettings), cx)
                    }),
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
                        let loading = self.discovery.read(cx).library_status == ReadStatus::Loading;
                        this.child(overlay::dropdown(
                            window,
                            Align::End,
                            Menu::new().child(
                                MenuItem::new("refresh", "Refresh agents")
                                    .disabled(loading)
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh_all(cx))),
                            ),
                            Self::dismiss(cx),
                        ))
                    }),
            )
    }

    fn host_heading(
        &self,
        host: &Entity<HostConnection>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let state = host.read(cx);
        let label = state.label.clone();
        let remote = state.remote.clone();
        let host_id = host.entity_id();
        let connected = matches!(state.state, ConnectionState::Connected { .. });
        let (badge, tone) = match &state.state {
            ConnectionState::Connected { .. } => ("Connected", BadgeTone::Success),
            ConnectionState::Connecting { .. } => ("Connecting", BadgeTone::Neutral),
            ConnectionState::Disconnected { .. } => ("Offline", BadgeTone::Danger),
            ConnectionState::Rejected { .. } => ("Refused", BadgeTone::Warning),
        };
        let menu_open = self.menu == Some(OpenMenu::Host(host_id));
        div()
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
                        let defaults = host.clone();
                        let mut menu = Menu::new()
                            .child(
                                MenuItem::new("host-refresh", "Refresh agents")
                                    .icon(IconName::ArrowsClockwise)
                                    .icon_size(14.)
                                    .disabled(!connected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.menu = None;
                                        refresh.update(cx, |host, cx| {
                                            host.refresh_status(cx).detach()
                                        });
                                    })),
                            )
                            .child(
                                MenuItem::new("host-defaults", "Agent defaults")
                                    .icon(IconName::SlidersHorizontal)
                                    .icon_size(14.)
                                    .disabled(!connected)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.agent_defaults(defaults.clone(), window, cx);
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
                                MenuItem::new("host-settings", "Manage hosts")
                                    .icon(IconName::Gear)
                                    .icon_size(14.)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_menu(None, cx);
                                        window.dispatch_action(Box::new(OpenHostSettings), cx);
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
            )
    }

    /// Inline status lines (§5a.9): bare paragraphs at body size.
    fn host_alerts(&self, host: &Entity<HostConnection>, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let state = host.read(cx);
        let label = state.label.clone();
        let host_id = host.entity_id();
        let line = |text: String| div().text_body().child(text);
        let quiet = |text: String| div().text_body_sm().text_color(c.text_subtle).child(text);
        let reconnect = |host: &Entity<HostConnection>| {
            let host = host.clone();
            div().flex().child(
                Button::new(
                    SharedString::from(format!("retry-{host_id}")),
                    "Retry connection",
                )
                .size(ButtonSize::Sm)
                .on_click(move |_, _, cx| host.read(cx).reconnect()),
            )
        };
        let mut alerts = div().flex().flex_col().gap(rems(0.5));
        match &state.state {
            ConnectionState::Connecting { attempt } if *attempt > 1 || state.remote.is_some() => {
                alerts = alerts.child(line(format!("Connecting to {label}…")));
            }
            ConnectionState::Disconnected { error, retry_in } => {
                alerts = alerts
                    .child(
                        line(format!(
                            "{label} is offline: {error}. Retrying in {} s.",
                            retry_in.as_secs().max(1)
                        ))
                        .text_color(c.text_danger),
                    )
                    .when(!state.agents.agents.is_empty(), |this| {
                        this.child(quiet("The agent statuses below may be out of date.".into()))
                    })
                    .child(reconnect(host));
            }
            ConnectionState::Rejected { reason } => {
                alerts = alerts
                    .child(
                        line(format!("{label} refused this app: {reason}"))
                            .text_color(c.text_danger),
                    )
                    .child(quiet(
                        "Check the token with `buzzd token` on that machine, then remove and \
                         add the host again in Settings → Agents."
                            .into(),
                    ))
                    .child(reconnect(host));
            }
            _ => {}
        }
        let status = state.status();
        if status == Status::Loading {
            alerts = alerts.child(line("Reading local agent status…".into()));
        }
        if matches!(state.state, ConnectionState::Connected { .. }) {
            if let Some(error) = state.control.error.clone() {
                alerts = alerts.child(line(error));
            }
            if status == Status::Error {
                if !state.agents.agents.is_empty() {
                    alerts =
                        alerts.child(quiet("The agent statuses below may be out of date.".into()));
                }
                let refresh = host.clone();
                alerts = alerts.child(
                    div().flex().child(
                        Button::new(
                            SharedString::from(format!("retry-status-{host_id}")),
                            "Retry status",
                        )
                        .on_click(move |_, _, cx| {
                            refresh.update(cx, |host, cx| host.refresh(cx).detach())
                        }),
                    ),
                );
            }
            if state.control.busy {
                alerts = alerts.child(line(if state.remote.is_some() {
                    format!("Waiting for {label}…")
                } else {
                    "Waiting for the desktop app…".into()
                }));
            }
            if !state.agents.runtime_available && status == Status::Ready {
                alerts = alerts.child(
                    line(state.agents.runtime_message.clone().unwrap_or_else(|| {
                        "The agent runtime is unavailable on this host.".into()
                    }))
                    .text_color(c.text_danger),
                );
            }
            for warning in &state.agents.inventory_warnings {
                alerts = alerts.child(line(format!(
                    "{warning} Retry local discovery by reopening the app."
                )));
            }
        }
        if state
            .remote
            .as_ref()
            .is_some_and(|remote| remote.token.is_some())
        {
            alerts = alerts.child(quiet(
                "This host's token is saved in config.toml, not the OS keyring.".into(),
            ));
        }
        alerts
    }

    /// One group: a heading, community subsections, and tiles or rows.
    fn group(
        &self,
        view: &View,
        group: Group,
        mut rows: Vec<&Row>,
        nested: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        inventory::sort(&mut rows);
        let sections = inventory::community_sections(group, rows);
        let mut column = div()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .child(div().text_label_sm().child(group.heading()));
        for (community, rows) in sections {
            let cards: Vec<AnyElement> = rows
                .into_iter()
                .map(|row| {
                    let decision = view.decide(row);
                    self.card(
                        view,
                        row,
                        decision,
                        community.as_deref(),
                        nested,
                        window,
                        cx,
                    )
                })
                .collect();
            let list = if group == Group::LocalHere {
                div()
                    .grid()
                    .grid_cols(columns(self.width.get()))
                    .gap(rems(1.))
                    .children(cards)
            } else {
                div()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(radius::CONTAINER)
                    .border_1()
                    .border_color(c.border_standard)
                    .children(cards)
            };
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap(rems(0.5))
                    .children(community.map(|community| {
                        div()
                            .text_label()
                            .text_color(c.text_subtle)
                            .child(community)
                    }))
                    .child(list),
            );
        }
        column.into_any_element()
    }

    /// The groups of `rows`, in page order.
    fn groups(
        &self,
        view: &View,
        rows: Vec<&Row>,
        nested: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut by_group: BTreeMap<Group, Vec<&Row>> = BTreeMap::new();
        for row in rows {
            by_group
                .entry(view.decide(row).group)
                .or_default()
                .push(row);
        }
        by_group
            .into_iter()
            .map(|(group, rows)| self.group(view, group, rows, nested, window, cx))
            .collect()
    }

    fn host_section(
        &self,
        ix: usize,
        view: &View,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host = self.hosts[ix].clone();
        let mine = |row: &&Row| view.host_of(row, &view.decide(row)) == Some(ix);
        let any = view.rows.values().any(|row| mine(&row));
        let rows: Vec<&Row> = view
            .rows
            .values()
            .filter(|row| !view.archived.contains(&row.pubkey))
            .filter(mine)
            .collect();
        let state = host.read(cx);
        let connected = matches!(state.state, ConnectionState::Connected { .. });
        let empty = !any && connected && state.status() != Status::Loading;
        let repair = state
            .agents
            .agents
            .iter()
            .any(|agent| agent.needs_team_import);
        let groups = self.groups(view, rows, false, window, cx);
        div()
            .flex()
            .flex_col()
            .gap(rems(1.5))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(self.host_heading(&host, window, cx))
                    .child(self.host_alerts(&host, cx)),
            )
            .when(empty, |this| {
                this.child(
                    div()
                        .text_body()
                        .child("No agents yet. Add an agent to get started."),
                )
            })
            .children(groups)
            .when(repair, |this| this.child(self.repair(&host, cx)))
            .into_any_element()
    }

    /// The relay's status lines, then what only the relays know.
    fn shared_sections(
        &self,
        view: &View,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let c = *cx.colors();
        let discovery = self.discovery.read(cx);
        let line = |text: String| div().text_body().child(text).into_any_element();
        let mut parts = Vec::new();
        if discovery.pending() {
            parts.push(line("Checking community inventory…".into()));
        }
        for community in discovery.profile_errors() {
            parts.push(line(format!(
                "Agent names and pictures could not be checked for {community}. Refresh to retry."
            )));
        }
        let errors = discovery.errors();
        for community in &errors {
            parts.push(line(format!(
                "Community inventory could not be checked for {community}. Refresh to retry."
            )));
        }
        if discovery.pending() || !errors.is_empty() {
            parts.push(line(
                "Showing known community associations. Community checks are not current.".into(),
            ));
        }
        if let Some(error) = discovery.library_error.clone() {
            parts.push(line(error));
        }
        let library_ready = discovery.library_status == ReadStatus::Ready;
        let (_, profiles) = relay::tiles(&discovery.library);
        let active: Vec<&Row> = view
            .rows
            .values()
            .filter(|row| !view.archived.contains(&row.pubkey))
            .collect();
        let all_archived = active.is_empty() && !view.archived.is_empty();
        if all_archived {
            parts.push(line(
                "All your agents are archived in this community.".into(),
            ));
        }
        let relay_only: Vec<&Row> = active
            .into_iter()
            .filter(|row| view.decide(row).group == Group::Relay)
            .collect();
        if !relay_only.is_empty() {
            parts.push(self.group(view, Group::Relay, relay_only, false, window, cx));
        }
        if !view.archived.is_empty() {
            let open = self.archived_open.unwrap_or(all_archived);
            let archived: Vec<&Row> = view
                .rows
                .values()
                .filter(|row| view.archived.contains(&row.pubkey))
                .collect();
            let count = archived.len();
            let header = div()
                .id("archived-toggle")
                .flex()
                .items_center()
                .justify_between()
                .py(rems(0.5))
                .px(rems(0.5))
                .mx(rems(-0.5))
                .rounded(radius::CONTROL)
                .cursor_pointer()
                .hover(|this| this.bg(c.affordance_subtle_hover))
                .child(div().text_label().child(format!("Archived ({count})")))
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
                    this.archived_open = Some(!open);
                    cx.notify();
                }));
            let body = open.then(|| {
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.5))
                    .children(self.groups(view, archived, true, window, cx))
            });
            parts.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(header)
                    .children(body)
                    .into_any_element(),
            );
        }
        let mut profiles = if library_ready { profiles } else { Vec::new() };
        if !profiles.is_empty() {
            profiles.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(a.id.cmp(&b.id))
            });
            let cards: Vec<AnyElement> = profiles
                .iter()
                .map(|profile| self.profile_card(profile, cx))
                .collect();
            parts.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .child(div().text_heading().child("Profiles without identities"))
                    .child(
                        div()
                            .grid()
                            .grid_cols(columns(self.width.get()))
                            .gap(rems(1.))
                            .children(cards),
                    )
                    .into_any_element(),
            );
        }
        parts
    }

    fn modal(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let page = cx.entity().downgrade();
        let close: Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.close_modal(cx));
        });
        let (content, dismiss): (AnyElement, Option<Dismiss>) = match self.modal.as_ref()? {
            Modal::Create { dialog, .. } => (dialog.clone().into_any_element(), None),
            Modal::Edit { dialog, .. } => (dialog.clone().into_any_element(), None),
            Modal::Defaults { dialog, .. } => (dialog.clone().into_any_element(), None),
            Modal::Import { dialog, .. } => (dialog.clone().into_any_element(), None),
            Modal::Manage(pubkey) => {
                let view = self.view(cx);
                let row = view.rows.get(pubkey)?.clone();
                (self.manage_dialog(&view, &row, window, cx), Some(close))
            }
            Modal::Delete { busy, .. } => {
                let busy = *busy;
                (self.delete_dialog(cx)?, (!busy).then_some(close))
            }
            Modal::Archive { .. } => (self.archive_dialog(cx)?, Some(close)),
            Modal::Remove { .. } => (self.remove_dialog(cx)?, Some(close)),
            Modal::Handover { pending, .. } => {
                let pending = *pending;
                (self.handover_dialog(cx)?, (!pending).then_some(close))
            }
        };
        let content = match self.modal.as_ref()? {
            Modal::Create { .. }
            | Modal::Edit { .. }
            | Modal::Defaults { .. }
            | Modal::Import { .. } => content,
            _ => div()
                .track_focus(&self.modal_focus)
                .key_context(buzz_ui::DIALOG_CONTEXT)
                .when_some(dismiss.clone(), |this, close| {
                    this.on_action(move |_: &buzz_ui::Cancel, window, cx| close(window, cx))
                })
                .child(content)
                .into_any_element(),
        };
        Some(overlay::modal(window, content, dismiss).into_any_element())
    }
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

        let view = self.view(cx);
        // Entering the all-archived state opens the Archived section again.
        let all_archived =
            !view.archived.is_empty() && view.rows.keys().all(|key| view.archived.contains(key));
        if all_archived != self.was_all_archived {
            self.was_all_archived = all_archived;
            if all_archived {
                self.archived_open = None;
            }
        }
        let mut sections: Vec<AnyElement> = (0..self.hosts.len())
            .map(|ix| self.host_section(ix, &view, window, cx))
            .collect();
        sections.extend(self.shared_sections(&view, window, cx));
        let modal = self.modal(window, cx);
        // The page's own dialogs take focus when they open.
        let own = matches!(
            self.modal,
            Some(
                Modal::Manage(_)
                    | Modal::Delete { .. }
                    | Modal::Archive { .. }
                    | Modal::Remove { .. }
                    | Modal::Handover { .. }
            )
        );
        if own && !self.modal_focus.contains_focused(window, cx) {
            window.focus(&self.modal_focus, cx);
        }
        let toast = self.toast.as_ref().map(|toast| {
            let right = window.viewport_size().width - px(16.) - px(384.);
            let mut notice = Toast::new(("agents-toast", toast.generation), toast.text.clone())
                .tone(ToastTone::Success)
                .on_dismiss(cx.listener(|this, _, _, cx| {
                    this.toast = None;
                    cx.notify();
                }));
            if let Some(pubkey) = toast.undo.clone() {
                notice = notice.action(
                    Button::new("archive-undo", "Undo")
                        .size(ButtonSize::Sm)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toast = None;
                            this.archive(&pubkey, false, cx);
                        })),
                );
            }
            deferred(
                anchored()
                    .position_mode(AnchoredPositionMode::Window)
                    .position(point(right, px(128.)))
                    .child(notice),
            )
            .with_priority(3)
        });
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
            .children(toast)
    }
}
