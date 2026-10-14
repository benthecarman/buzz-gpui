//! The Agents2 page (buzz-app's `Agents2Page.tsx`). Three frames, one at a
//! time:
//! - Inventory: every agent as a tile. Selecting one (click, or arrow keys)
//!   opens its peek beside the grid without leaving it; Enter or a
//!   double-click builds.
//! - Peek: identity, the type's own read-only glance, and what wakes it.
//! - Build: the identity rail on the left, the type's tabs and the app's
//!   Attention tab on the right.
//!
//! New agents are made in the peek slot, so making one never covers the
//! grid, and so are agents brought over from an earlier agent system.

use super::attention::summary as attention_summary;
use super::bring_over::{BringOver, BringOverEvent};
use super::{AgentDelete, AgentTabs, Removed, empty_state, problem};
use crate::agents2::{Agent, AgentChange, Agents2, RegisteredType, Status, type_of, types};
use crate::hosts::HostConnection;
use crate::ui::agents::form::field;
use crate::ui::overlay::{self, Align, Dismiss};
use crate::ui::settings::avatar::{
    AvatarDismissed, AvatarDone, AvatarEditor, Preview, popover as avatar_popover,
};
use buzz_host_rpc::ConnectionState;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, Colors, IconButton, IconButtonSize,
    IconName, Menu, MenuItem, PanelHeader, Select, Surface, TextField, TypeStyled,
    tokens::{duration, easing, radius},
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nostr::nips::nip19::ToBech32;
use std::cell::Cell;
use std::rc::Rc;

/// Tiles are a fixed 10rem, so opening the side panel never resizes one.
const TILE_REM: f32 = 10.;
const GAP_REM: f32 = 0.75;
const GRID_PADDING_REM: f32 = 1.5;
/// Below this viewport width the side panel narrows and Build stacks.
const NARROW_REM: f32 = 52.;

/// What the peek slot shows instead of a selected agent's peek.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Creating,
    Porting,
}

#[derive(Clone, Debug, PartialEq)]
enum Frame {
    Browse {
        selected: Option<String>,
        side: Option<Side>,
    },
    Build {
        pubkey: String,
    },
}

/// `npub…` and the key's last `suffix` characters (`formatPublicKey`).
fn short_key(pubkey: &str, suffix: usize) -> String {
    nostr::prelude::PublicKey::from_hex(pubkey)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .map(|npub| {
            let length = suffix.clamp(3, npub.len() - 5);
            format!("npub…{}", &npub[npub.len() - length..])
        })
        .unwrap_or_default()
}

/// The type's one-line summary of an agent; its title when it has none.
fn summary_of(agent: &Agent, kind: Option<&RegisteredType>) -> String {
    match kind {
        Some(kind) => kind.summary_of(agent),
        None => "Type unavailable".into(),
    }
}

/// The next selection for a key in the listbox: arrows move by one or by
/// a row, Home and End go to the ends. `None` when the key does nothing.
pub(crate) fn step(key: &str, from: Option<usize>, count: usize, columns: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let last = count as isize - 1;
    let columns = columns.max(1) as isize;
    let delta = match key {
        "right" => 1,
        "left" => -1,
        "down" => columns,
        "up" => -columns,
        "home" => return Some(0),
        "end" => return Some(last as usize),
        _ => return None,
    };
    let Some(from) = from else {
        return Some(0);
    };
    let from = from as isize;
    let next = from + delta;
    Some(if next < 0 || next > last {
        if delta.abs() == 1 {
            next.clamp(0, last) as usize
        } else {
            from as usize
        }
    } else {
        next as usize
    })
}

pub struct Agents2Page {
    frame: Frame,
    grid: FocusHandle,
    new_agent: Option<Entity<NewAgent>>,
    bring_over: Option<Entity<BringOver>>,
    build: Option<Entity<Build>>,
    /// Counts side panel openings, so each one animates in.
    side_opened: usize,
    side_key: Option<String>,
    width: Rc<Cell<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

impl Agents2Page {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions =
            vec![cx.observe_global::<crate::agents2::registry::AgentTypes>(|_, cx| cx.notify())];
        if let Some(service) = Agents2::global(cx) {
            subscriptions.push(cx.observe(&service, |_, _, cx| cx.notify()));
        }
        Self {
            frame: Frame::Browse {
                selected: None,
                side: None,
            },
            grid: cx.focus_handle(),
            new_agent: None,
            bring_over: None,
            build: None,
            side_opened: 0,
            side_key: None,
            width: Rc::new(Cell::new(px(0.))),
            _subscriptions: subscriptions,
        }
    }

    fn browse(&mut self, selected: Option<String>, cx: &mut Context<Self>) {
        self.frame = Frame::Browse {
            selected,
            side: None,
        };
        self.new_agent = None;
        self.bring_over = None;
        self.build = None;
        cx.notify();
    }

    fn build(&mut self, pubkey: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| Build::new(pubkey.clone(), window, cx));
        cx.subscribe(&view, |this, _, _: &Removed, cx| this.browse(None, cx))
            .detach();
        self.build = Some(view);
        self.new_agent = None;
        self.bring_over = None;
        self.frame = Frame::Build { pubkey };
        cx.notify();
    }

    fn new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| NewAgent::new(window, cx));
        cx.subscribe_in(
            &form,
            window,
            |this, _, event: &NewAgentEvent, window, cx| match event {
                NewAgentEvent::Close => this.browse(None, cx),
                NewAgentEvent::Created(pubkey) => this.build(pubkey.clone(), window, cx),
            },
        )
        .detach();
        self.new_agent = Some(form);
        self.bring_over = None;
        self.frame = Frame::Browse {
            selected: None,
            side: Some(Side::Creating),
        };
        cx.notify();
    }

    fn bring_over(&mut self, cx: &mut Context<Self>) {
        let view = cx.new(BringOver::new);
        cx.subscribe(&view, |this, _, event: &BringOverEvent, cx| match event {
            BringOverEvent::Close => this.browse(None, cx),
        })
        .detach();
        self.bring_over = Some(view);
        self.new_agent = None;
        self.frame = Frame::Browse {
            selected: None,
            side: Some(Side::Porting),
        };
        cx.notify();
    }

    fn columns(&self, window: &Window) -> usize {
        let rem = f32::from(window.rem_size());
        let inner = f32::from(self.width.get()) - 2. * GRID_PADDING_REM * rem;
        (((inner + GAP_REM * rem) / ((TILE_REM + GAP_REM) * rem)).floor() as usize).max(1)
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Frame::Browse { selected, .. } = &self.frame else {
            return;
        };
        let Some(service) = Agents2::global(cx) else {
            return;
        };
        let agents: Vec<String> = service
            .read(cx)
            .agents()
            .iter()
            .map(|agent| agent.pubkey.clone())
            .collect();
        let index = selected
            .as_ref()
            .and_then(|pubkey| agents.iter().position(|p| p == pubkey));
        let key = event.keystroke.key.as_str();
        match key {
            "enter" => {
                if let Some(pubkey) = selected.clone() {
                    cx.stop_propagation();
                    self.build(pubkey, window, cx);
                }
            }
            "escape" => {
                if selected.is_some() {
                    cx.stop_propagation();
                    self.browse(None, cx);
                }
            }
            _ => {
                if let Some(next) = step(key, index, agents.len(), self.columns(window)) {
                    cx.stop_propagation();
                    self.browse(agents.get(next).cloned(), cx);
                }
            }
        }
    }

    fn header(
        &self,
        building: Option<&Agent>,
        side: Option<Side>,
        cx: &mut Context<Self>,
    ) -> PanelHeader {
        let service = Agents2::global(cx);
        let ready = service
            .as_ref()
            .is_some_and(|service| *service.read(cx).status() == Status::Ready);
        let header = PanelHeader::new().joined(true);
        match building {
            Some(agent) => {
                let pubkey = agent.pubkey.clone();
                header
                    .leading(
                        IconButton::new("all-agents", IconName::ArrowLeft)
                            .size(IconButtonSize::Sm)
                            .tooltip("All agents")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.browse(Some(pubkey.clone()), cx)
                            })),
                    )
                    .leading(
                        div()
                            .px(rems(0.5))
                            .text_label()
                            .truncate()
                            .child(agent.name.clone()),
                    )
            }
            None => header
                .title("Agents")
                .when(!types(cx).is_empty() && ready, |this| {
                    this.action(
                        Button::new("bring-over", "Bring over")
                            .size(ButtonSize::Sm)
                            .disabled(side == Some(Side::Porting))
                            .on_click(cx.listener(|this, _, _, cx| this.bring_over(cx))),
                    )
                    .action(
                        Button::new("new-agent", "New agent")
                            .size(ButtonSize::Sm)
                            .icon(IconName::Plus)
                            .disabled(side == Some(Side::Creating))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_agent(window, cx)),
                            ),
                    )
                }),
        }
    }

    /// One agent's tile, built while the service's agents are borrowed.
    fn tile(agent: &Agent, active: bool, page: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let c = *cx.colors();
        let page = page.clone();
        let kind = type_of(&agent.agent_type, cx);
        let pubkey = agent.pubkey.clone();
        div()
            .id(SharedString::from(format!("agent2-{}", agent.pubkey)))
            .flex()
            .flex_col()
            .items_center()
            .gap(rems(0.25))
            .w(rems(TILE_REM))
            .min_w_0()
            .pt(rems(1.25))
            .px(rems(0.75))
            .pb(rems(1.))
            .border_1()
            .border_color(if active {
                c.border_focus
            } else {
                c.border_standard
            })
            .rounded(radius::CONTAINER)
            .bg(if active {
                c.affordance_selected
            } else {
                c.surface_panel
            })
            .text_center()
            .cursor_pointer()
            .when(!active, |this| {
                this.hover(|this| this.bg(c.affordance_panel_hover))
            })
            .child(
                div()
                    .size(rems(4.5))
                    .mb(rems(0.5))
                    .when(kind.is_none(), |this| this.opacity(0.5))
                    .child(
                        Avatar::new(agent.name.clone())
                            .size(AvatarSize::Px(72.))
                            .squircle()
                            .when_some(agent_picture(agent, cx), |avatar, picture| {
                                avatar.image(picture)
                            }),
                    ),
            )
            .child(
                div()
                    .max_w_full()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(agent.name.clone()),
            )
            .child(
                div()
                    .max_w_full()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .line_clamp(2)
                    .child(summary_of(agent, kind.as_ref())),
            )
            .on_click(move |event: &ClickEvent, window, cx| {
                let _ = page.update(cx, |this, cx| {
                    window.focus(&this.grid, cx);
                    if event.click_count() >= 2 {
                        this.build(pubkey.clone(), window, cx);
                    } else {
                        this.browse((!active).then(|| pubkey.clone()), cx);
                    }
                });
            })
            .into_any_element()
    }

    fn peek(&self, agent: &Agent, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let kind = type_of(&agent.agent_type, cx);
        let summary = summary_of(agent, kind.as_ref());
        let peek = kind
            .as_ref()
            .and_then(|kind| kind.kind.peek(agent, window, cx));
        let line = match &kind {
            None => "Type unavailable".to_string(),
            Some(kind) => {
                let title = kind.kind.title().to_string();
                if peek.is_none() && summary != title {
                    format!("{title} · {summary}")
                } else {
                    title
                }
            }
        };
        let channels = Agents2::global(cx)
            .map(|service| service.read(cx).channel_choices(cx))
            .unwrap_or_default();
        let pubkey = agent.pubkey.clone();
        side_body()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(rems(0.75))
                    .child(
                        div().flex_none().size(rems(4.)).child(
                            Avatar::new(agent.name.clone())
                                .size(AvatarSize::Px(64.))
                                .squircle()
                                .when_some(agent_picture(agent, cx), |avatar, picture| {
                                    avatar.image(picture)
                                }),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(rems(0.125))
                            .child(div().text_heading().truncate().child(agent.name.clone()))
                            .child(div().text_body_sm().text_color(c.text_subtle).child(line)),
                    )
                    .child(
                        IconButton::new("peek-close", IconName::X)
                            .size(IconButtonSize::Sm)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.browse(None, cx))),
                    ),
            )
            .child(
                Button::new("peek-open", "Open")
                    .prominent()
                    .full_width()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.build(pubkey.clone(), window, cx)
                    })),
            )
            .map(|this| match (peek, &kind) {
                (Some(peek), _) => this.child(
                    div()
                        .py(rems(0.75))
                        .px(rems(1.))
                        .rounded(radius::CONTAINER)
                        .bg(c.surface_inset)
                        .child(peek),
                ),
                (None, None) => this.child(div().text_body_sm().text_color(c.text_subtle).child(
                    "Enable the plugin that provides this agent's type to run or change it.",
                )),
                (None, Some(_)) => this,
            })
            .child(section(
                "Wakes on",
                attention_summary(agent, &channels, &c),
                &c,
            ))
    }
}

/// The community the agent was created for, if it is added.
fn agent_community(agent: &Agent, cx: &App) -> Option<Entity<crate::community::Community>> {
    let relay = crate::ui::agents::inventory::origin(&agent.relay);
    cx.global::<crate::community::AppState>()
        .communities
        .iter()
        .find(|community| community.read(cx).origin.http() == relay)
        .cloned()
}

/// The agent's profile picture: the one its host saved, so a change shows
/// before its community has the new profile, else (a host from before
/// pictures) the one its community's profiles have.
fn agent_picture(agent: &Agent, cx: &App) -> Option<ImageSource> {
    let community = agent_community(agent, cx)?;
    let community = community.read(cx);
    let picture = match &agent.picture {
        Some(picture) => picture.clone(),
        None => community
            .snapshot
            .profiles
            .get(&agent.pubkey)?
            .picture
            .clone()?,
    };
    (!picture.is_empty()).then(|| crate::ui::media::avatar(&picture, community.session.media()))
}

pub(super) fn side_body() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(1.25))
        .pt(rems(1.))
        .px(rems(1.25))
        .pb(rems(1.5))
}

fn section(title: &'static str, content: impl IntoElement, c: &Colors) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(0.5))
        .child(div().text_caption().text_color(c.text_subtle).child(title))
        .child(content)
}

impl Render for Agents2Page {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let Some(service) = Agents2::global(cx) else {
            return div();
        };
        let status = service.read(cx).status().clone();
        // Only the shown agent is copied; the grid borrows the rest.
        let find = |pubkey: Option<&String>, cx: &App| {
            pubkey.and_then(|pubkey| service.read(cx).find(pubkey).cloned())
        };
        // A deleted or out-of-scope agent's Build falls back to the grid.
        let building = match &self.frame {
            Frame::Build { pubkey } => find(Some(pubkey), cx),
            _ => None,
        };
        if building.is_none() && self.build.is_some() {
            self.build = None;
            self.frame = Frame::Browse {
                selected: None,
                side: None,
            };
        }
        let (selected, side_shown) = match &self.frame {
            Frame::Browse { selected, side } => (find(selected.as_ref(), cx), *side),
            Frame::Build { .. } => (None, None),
        };
        let has_types = !types(cx).is_empty();
        let narrow =
            f32::from(window.viewport_size().width) < NARROW_REM * f32::from(window.rem_size());

        let body: AnyElement = match (&status, &building, &self.build) {
            (Status::Unavailable, ..) => div()
                .p(rems(1.5))
                .child(empty_state(
                    "\u{2726}",
                    "Agents runs in the desktop app",
                    "Agent keys stay in the desktop app's native custody.",
                    None,
                    &c,
                ))
                .into_any_element(),
            (Status::Error(error), ..) => div()
                .p(rems(1.5))
                .child(empty_state(
                    "!",
                    "Agents could not load",
                    if error.is_empty() {
                        "Try again.".to_string()
                    } else {
                        error.clone()
                    },
                    None,
                    &c,
                ))
                .into_any_element(),
            (_, Some(_), Some(view)) => view.clone().into_any_element(),
            _ => {
                let side_key = match side_shown {
                    Some(Side::Creating) => Some("new".to_string()),
                    Some(Side::Porting) => Some("port".to_string()),
                    None => selected.as_ref().map(|agent| agent.pubkey.clone()),
                };
                if side_key != self.side_key {
                    if side_key.is_some() {
                        self.side_opened += 1;
                    }
                    self.side_key = side_key.clone();
                }
                let side: Option<AnyElement> = match side_shown {
                    Some(Side::Creating) => {
                        self.new_agent.clone().map(|form| form.into_any_element())
                    }
                    Some(Side::Porting) => {
                        self.bring_over.clone().map(|view| view.into_any_element())
                    }
                    None => selected
                        .as_ref()
                        .map(|agent| self.peek(agent, window, cx).into_any_element()),
                };
                let inventory: AnyElement = if !service.read(cx).agents().is_empty() {
                    let index_selected = selected.as_ref().map(|a| a.pubkey.as_str());
                    let page = cx.entity().downgrade();
                    let tiles: Vec<AnyElement> = service
                        .read(cx)
                        .agents()
                        .iter()
                        .map(|agent| {
                            let active = index_selected == Some(agent.pubkey.as_str());
                            Self::tile(agent, active, &page, cx)
                        })
                        .collect();
                    let width = self.width.clone();
                    let page = cx.entity().downgrade();
                    let measure = canvas(
                        move |bounds, _, cx| {
                            if width.replace(bounds.size.width) != bounds.size.width {
                                let _ = page.update(cx, |_, cx| cx.notify());
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full();
                    div()
                        .id("agents2-grid")
                        .relative()
                        .track_focus(&self.grid)
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            this.key_down(event, window, cx)
                        }))
                        .flex()
                        .flex_wrap()
                        .content_start()
                        .gap(rems(GAP_REM))
                        .p(rems(GRID_PADDING_REM))
                        .child(measure)
                        .children(tiles)
                        .into_any_element()
                } else if status == Status::Ready && side_shown.is_none() {
                    empty_state(
                        "\u{2726}",
                        "No agents yet",
                        if has_types {
                            "An agent has its own key and wakes when it is mentioned or something it watches happens."
                        } else {
                            "Enable an agent type plugin to make one."
                        },
                        has_types.then(|| {
                            Button::new("empty-new-agent", "New agent")
                                .prominent()
                                .on_click(cx.listener(|this, _, window, cx| this.new_agent(window, cx)))
                                .into_any_element()
                        }),
                        &c,
                    )
                    .into_any_element()
                } else {
                    div().into_any_element()
                };
                let reduce = cx.reduce_motion();
                let side = side.map(|side| {
                    let panel = div()
                        .id("agents2-side")
                        .flex_none()
                        .w(rems(if narrow { 18. } else { 22. }))
                        .h_full()
                        .min_h_0()
                        .overflow_y_scroll()
                        .border_l_1()
                        .border_color(c.border_standard)
                        .bg(c.surface_panel)
                        .relative()
                        .child(side);
                    let enter = Animation::new(duration::FAST)
                        .with_easing(|t| easing::cubic_bezier(easing::ENTER, t));
                    panel.with_animation(
                        ElementId::Name(format!("agents2-side-{}", self.side_opened).into()),
                        enter,
                        move |panel, delta| {
                            if reduce {
                                panel
                            } else {
                                panel.opacity(delta).left(rems(1. - delta))
                            }
                        },
                    )
                });
                div()
                    .flex()
                    .size_full()
                    .min_h_0()
                    .child(
                        div()
                            .id("agents2-inventory")
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(inventory),
                    )
                    .children(side)
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .text_color(c.text_standard)
            .text_body()
            .child(self.header(building.as_ref(), side_shown, cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}

// ----- New agent -----

pub enum NewAgentEvent {
    Close,
    Created(String),
}

pub struct NewAgent {
    name: Entity<InputState>,
    kind: Option<SharedString>,
    host: Option<Entity<HostConnection>>,
    host_open: bool,
    pending: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NewAgentEvent> for NewAgent {}

impl NewAgent {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx));
        name.update(cx, |state, cx| state.focus(window, cx));
        let subscriptions =
            vec![
                cx.subscribe_in(
                    &name,
                    window,
                    |this, _, event: &InputEvent, window, cx| match event {
                        InputEvent::PressEnter { .. } => this.create(window, cx),
                        InputEvent::Change => cx.notify(),
                        _ => {}
                    },
                ),
            ];
        Self {
            name,
            kind: types(cx).first().map(|kind| kind.key.clone()),
            host: None,
            host_open: false,
            pending: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn create(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let all = types(cx);
        let chosen = self
            .kind
            .as_ref()
            .and_then(|key| all.iter().find(|kind| &kind.key == key))
            .or(all.first())
            .cloned();
        let name = self.name.read(cx).value().trim().to_string();
        let (Some(chosen), Some(service)) = (chosen, Agents2::global(cx)) else {
            return;
        };
        if name.is_empty() || self.pending {
            return;
        }
        self.pending = true;
        self.error = None;
        let host = self.host.clone();
        let task = service.update(cx, |service, cx| {
            service.create(&chosen.key, &name, host, cx)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(agent) => cx.emit(NewAgentEvent::Created(agent.pubkey)),
                    Err(error) => {
                        this.error = Some(error.into());
                        this.pending = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The Host field, when more than one host can hold the key (as the
    /// Add agent dialog's).
    fn host_field(
        &self,
        hosts: &[Entity<HostConnection>],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let selected = self
            .host
            .clone()
            .or_else(|| {
                hosts
                    .iter()
                    .find(|host| host.read(cx).remote.is_none())
                    .cloned()
            })
            .or_else(|| hosts.first().cloned());
        let label = selected.as_ref().map(|host| host.read(cx).label.clone());
        let mut menu = Menu::new();
        for (index, host) in hosts.iter().enumerate() {
            let checked = selected.as_ref().map(Entity::entity_id) == Some(host.entity_id());
            let online = matches!(host.read(cx).state, ConnectionState::Connected { .. });
            let pick = host.clone();
            menu = menu.child(
                MenuItem::new(("agents2-host", index), host.read(cx).label.clone())
                    .checked(checked)
                    .disabled(!online)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.host = Some(pick.clone());
                        this.host_open = false;
                        cx.notify();
                    })),
            );
        }
        let weak = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.host_open = false;
                cx.notify();
            });
        });
        let select = Select::new("agents2-host-select")
            .placeholder("Choose where it runs")
            .when_some(label, |this, label| this.value(label))
            .surface(Surface::Floating)
            .disabled(self.pending)
            .on_click(cx.listener(|this, _, _, cx| {
                this.host_open = !this.host_open;
                cx.notify();
            }));
        field(
            "Host",
            Some("The machine that keeps this agent's key.".into()),
            overlay::select_menu(
                select,
                self.host_open,
                Align::Start,
                Some(rems(16.)),
                || menu.into_any_element(),
                dismiss,
                window,
            )
            .w_full(),
            &c,
        )
    }
}

impl Render for NewAgent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let all = types(cx);
        let chosen = self
            .kind
            .as_ref()
            .and_then(|key| all.iter().find(|kind| &kind.key == key))
            .or(all.first())
            .map(|kind| kind.key.clone());
        let name_empty = self.name.read(cx).value().trim().is_empty();
        let hosts = Agents2::global(cx)
            .map(|service| service.read(cx).hosts(cx))
            .unwrap_or_default();
        let cards: Vec<AnyElement> = all
            .iter()
            .map(|kind| {
                let checked = chosen.as_ref() == Some(&kind.key);
                let key = kind.key.clone();
                let pending = self.pending;
                div()
                    .id(SharedString::from(format!("type-{}", kind.key)))
                    .flex()
                    .items_start()
                    .gap(rems(0.75))
                    .p(rems(0.75))
                    .border_1()
                    .border_color(if checked {
                        c.border_focus
                    } else {
                        c.border_standard
                    })
                    .rounded(radius::CONTROL)
                    .when(!pending, |this| this.cursor_pointer())
                    .child(radio_dot(checked, &c))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap(rems(0.125))
                            .child(div().text_label_sm().child(kind.kind.title()))
                            .children(kind.kind.description().map(|description| {
                                div()
                                    .text_body_sm()
                                    .text_color(c.text_subtle)
                                    .child(description)
                            })),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.pending {
                            this.kind = Some(key.clone());
                            cx.notify();
                        }
                    }))
                    .into_any_element()
            })
            .collect();
        side_body()
            .key_context("Agents2NewAgent")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !this.pending {
                    cx.stop_propagation();
                    cx.emit(NewAgentEvent::Close);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(rems(0.5))
                    .child(div().min_w_0().text_heading().child("New agent"))
                    .child(
                        IconButton::new("new-agent-close", IconName::X)
                            .size(IconButtonSize::Sm)
                            .tooltip("Close")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewAgentEvent::Close))),
                    ),
            )
            .child(field(
                "Name",
                None,
                TextField::new(&self.name)
                    .surface(Surface::Floating)
                    .disabled(self.pending),
                &c,
            ))
            .when(hosts.len() > 1, |this| {
                this.child(self.host_field(&hosts, window, cx))
            })
            .child(field(
                "Type",
                None,
                div().flex().flex_col().gap(rems(0.75)).children(cards),
                &c,
            ))
            .child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("It gets its own key on this device and runs while the app is open."),
            )
            .children(problem(self.error.as_ref(), &c))
            .child(
                Button::new("create-agent", "Create and set up")
                    .prominent()
                    .full_width()
                    .loading(self.pending)
                    .disabled(name_empty || chosen.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
            )
    }
}

pub(super) fn radio_dot(checked: bool, c: &Colors) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .mt(rems(0.125))
        .size(rems(1.25))
        .rounded_full()
        .border_1()
        .border_color(if checked {
            c.affordance_prominent
        } else {
            c.border_prominent
        })
        .when(checked, |this| {
            this.child(
                div()
                    .size(rems(0.5))
                    .rounded_full()
                    .bg(c.affordance_prominent),
            )
        })
}

// ----- Build -----

pub struct Build {
    pubkey: String,
    name: Entity<InputState>,
    /// The name the field last showed, to follow outside renames.
    shown: String,
    /// Escape blurs too, and blur saves; this skips that one save.
    cancelled: bool,
    name_error: Option<SharedString>,
    /// A picture is saving.
    picture_busy: bool,
    picture_error: Option<SharedString>,
    /// The avatar editor, while open.
    avatar: Option<(Entity<AvatarEditor>, [Subscription; 2])>,
    copied: usize,
    copied_shown: bool,
    tabs: Entity<AgentTabs>,
    delete: Entity<AgentDelete>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Removed> for Build {}

impl Build {
    fn new(pubkey: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = Agents2::global(cx)
            .and_then(|service| {
                service
                    .read(cx)
                    .find(&pubkey)
                    .map(|agent| agent.name.clone())
            })
            .unwrap_or_default();
        let name = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(current.clone(), window, cx);
            state
        });
        let tabs = cx.new(|cx| AgentTabs::new(pubkey.clone(), window, cx));
        let delete = cx.new(|cx| AgentDelete::new(pubkey.clone(), cx));
        let mut subscriptions = vec![
            cx.subscribe_in(
                &name,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => window.blur(cx),
                    InputEvent::Blur => this.commit(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe(&delete, |_, _, _: &Removed, cx| cx.emit(Removed)),
        ];
        if let Some(service) = Agents2::global(cx) {
            subscriptions.push(cx.observe_in(&service, window, |this, _, window, cx| {
                this.follow(window, cx)
            }));
        }
        Self {
            pubkey,
            name,
            shown: current,
            cancelled: false,
            name_error: None,
            picture_busy: false,
            picture_error: None,
            avatar: None,
            copied: 0,
            copied_shown: false,
            tabs,
            delete,
            _subscriptions: subscriptions,
        }
    }

    fn current(&self, cx: &App) -> Option<Agent> {
        Agents2::global(cx)?.read(cx).find(&self.pubkey).cloned()
    }

    /// Shows a rename made elsewhere unless the field is being edited.
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = self.current(cx) else {
            return;
        };
        if agent.name != self.shown && !self.name.read(cx).focus_handle(cx).is_focused(window) {
            self.shown = agent.name.clone();
            self.name
                .update(cx, |state, cx| state.set_value(agent.name, window, cx));
        }
        cx.notify();
    }

    /// Saves on Enter or blur; an empty field puts the name back.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.cancelled) {
            return;
        }
        let Some(agent) = self.current(cx) else {
            return;
        };
        let next = self.name.read(cx).value().trim().to_string();
        if next.is_empty() {
            self.name.update(cx, |state, cx| {
                state.set_value(agent.name.clone(), window, cx)
            });
            return;
        }
        if next == agent.name {
            return;
        }
        self.name_error = None;
        let Some(service) = Agents2::global(cx) else {
            return;
        };
        let task = service.update(cx, |service, cx| {
            service.save(
                &agent.pubkey,
                AgentChange {
                    name: Some(next),
                    ..Default::default()
                },
                cx,
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.name_error = Some(error.into());
                    this.name
                        .update(cx, |state, cx| state.set_value(agent.name, window, cx));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Opens the avatar editor on the agent's picture; Done saves it, which
    /// its host then publishes in its profile (`AvatarEditor`).
    fn open_avatar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = self.current(cx) else {
            return;
        };
        if self.picture_busy || self.avatar.is_some() {
            return;
        }
        if agent.picture.is_none() {
            self.picture_error = Some(crate::agents2::PICTURES_UNSUPPORTED.into());
            return cx.notify();
        }
        let community = agent_community(&agent, cx);
        let picture = agent.picture.clone().unwrap_or_default();
        let editor = cx.new(|cx| AvatarEditor::new(picture, community, window, cx));
        let subscriptions = [
            cx.subscribe(&editor, |this, _, AvatarDone(picture), cx| {
                this.avatar = None;
                this.change_picture(picture.clone(), cx);
            }),
            cx.subscribe(&editor, |this, _, _: &AvatarDismissed, cx| {
                this.avatar = None;
                cx.notify();
            }),
        ];
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        self.avatar = Some((editor, subscriptions));
        self.picture_error = None;
        cx.notify();
    }

    /// Saves `picture`, an uploaded image's URL; empty removes it.
    fn change_picture(&mut self, picture: String, cx: &mut Context<Self>) {
        self.picture_busy = true;
        self.picture_error = None;
        let task = self.save_picture(picture, cx);
        cx.spawn(async move |this, cx| {
            let saved = task.await;
            let _ = this.update(cx, |this, cx| {
                this.picture_busy = false;
                this.picture_error = saved.err().map(Into::into);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save_picture(
        &mut self,
        picture: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(service) = Agents2::global(cx) else {
            return Task::ready(Err("Agents is not running".into()));
        };
        let pubkey = self.pubkey.clone();
        service.update(cx, |service, cx| {
            service.save(
                &pubkey,
                AgentChange {
                    picture: Some(picture),
                    ..Default::default()
                },
                cx,
            )
        })
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = self.current(cx) else {
            return;
        };
        self.cancelled = true;
        self.name
            .update(cx, |state, cx| state.set_value(agent.name, window, cx));
        window.blur(cx);
        // Escape may not blur a field that is not focused; nothing to skip.
        if !self.name.read(cx).focus_handle(cx).is_focused(window) {
            cx.defer_in(window, |this, _, _| this.cancelled = false);
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.pubkey.clone()));
        self.copied += 1;
        self.copied_shown = true;
        let generation = self.copied;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1500))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.copied == generation {
                    this.copied_shown = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for Build {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let Some(agent) = self.current(cx) else {
            return div();
        };
        let kind = type_of(&agent.agent_type, cx);
        let narrow =
            f32::from(window.viewport_size().width) < NARROW_REM * f32::from(window.rem_size());
        let facts = div()
            .flex()
            .flex_col()
            .gap(rems(0.25))
            .text_body_sm()
            .child(div().text_color(c.text_subtle).child("Type"))
            .child(
                div().min_w_0().child(
                    kind.as_ref()
                        .map(|kind| kind.kind.title().to_string())
                        .unwrap_or_else(|| "Type unavailable".into()),
                ),
            )
            .child(div().mt(rems(0.5)).text_color(c.text_subtle).child("Key"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.25))
                    .min_w_0()
                    .child(
                        div()
                            .font_family("JetBrains Mono")
                            .truncate()
                            .child(short_key(&agent.pubkey, 8)),
                    )
                    .child(
                        IconButton::new("copy-key", IconName::Copy)
                            .size(IconButtonSize::Sm)
                            .icon_size(14.)
                            .tooltip(if self.copied_shown {
                                "Copied"
                            } else {
                                "Copy public key"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                    ),
            );
        let name_field = div()
            .flex()
            .flex_col()
            .gap(rems(0.25))
            .when(narrow, |this| this.flex_1().min_w(rems(12.)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.cancel(window, cx);
                }
            }))
            .child(TextField::new(&self.name).invalid(self.name_error.is_some()))
            .children(problem(self.name_error.as_ref(), &c));
        // The compact avatar editor: the picture, or the draft while the
        // editor is open, with the edit badge over its lower right.
        let draft = self
            .avatar
            .as_ref()
            .map(|(editor, _)| editor.read(cx).preview(cx));
        let artwork = match draft {
            Some(Preview::Emoji { art, color }) => div()
                .size_full()
                .rounded(radius::CONTAINER)
                .overflow_hidden()
                .bg(color)
                .children(art.map(|art| img(art).size_full()))
                .into_any_element(),
            draft => {
                let shown = match draft {
                    Some(Preview::Picture(picture)) => agent_community(&agent, cx)
                        .filter(|_| !picture.is_empty())
                        .map(|community| {
                            crate::ui::media::avatar(&picture, community.read(cx).session.media())
                        }),
                    _ => agent_picture(&agent, cx),
                };
                Avatar::new(agent.name.clone())
                    .size(AvatarSize::Px(96.))
                    .squircle()
                    .when_some(shown, |avatar, picture| avatar.image(picture))
                    .into_any_element()
            }
        };
        let page = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| {
                this.avatar = None;
                cx.notify();
            });
        });
        let popover = self
            .avatar
            .as_ref()
            .map(|(editor, _)| avatar_popover(window, editor.clone(), dismiss));
        let has_picture = agent.picture.as_ref().is_some_and(|p| !p.is_empty());
        let badge = div()
            .absolute()
            .left(px(96. * 0.859375 - 16.))
            .top(px(96. * 0.859375 - 16.))
            .child(
                IconButton::new(
                    "agent-picture",
                    if has_picture {
                        IconName::PencilSimple
                    } else {
                        IconName::Plus
                    },
                )
                .size(IconButtonSize::Sm)
                .variant(buzz_ui::ButtonVariant::Prominent)
                .shape(buzz_ui::IconButtonShape::Round)
                .disabled(self.picture_busy)
                .tooltip("Edit avatar")
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.avatar.take().is_some() {
                        cx.notify();
                    } else {
                        this.open_avatar(window, cx);
                    }
                })),
            )
            .children(popover);
        let picture = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(rems(0.5))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .size(rems(6.))
                    .child(artwork)
                    .child(badge),
            )
            .children(problem(self.picture_error.as_ref(), &c));
        let rail = div()
            .id("agents2-rail")
            .flex()
            .gap(rems(1.))
            .py(rems(1.5))
            .px(rems(1.25))
            .border_color(c.border_standard)
            .overflow_y_scroll()
            .map(|this| {
                if narrow {
                    this.flex_row().flex_wrap().items_center().border_b_1()
                } else {
                    this.flex_col()
                        .w(rems(15.))
                        .flex_none()
                        .h_full()
                        .border_r_1()
                }
            })
            .child(picture)
            .child(name_field)
            .child(facts)
            .child(
                div()
                    .flex()
                    .when(!narrow, |this| this.mt_auto())
                    .child(self.delete.clone()),
            );
        let work = div()
            .id("agents2-work")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(rems(44.))
                    .pt(rems(1.))
                    .px(rems(1.5))
                    .pb(rems(2.))
                    .when(kind.is_none(), |this| {
                        this.child(div().mb(rems(1.)).text_body_sm().text_color(c.text_subtle).child(
                            "Enable the plugin that provides this agent's type to change its \
                             settings. Its attention can still be edited.",
                        ))
                    })
                    .child(self.tabs.clone()),
            );
        div()
            .flex()
            .size_full()
            .min_h_0()
            .map(|this| {
                if narrow {
                    this.flex_col()
                } else {
                    this.flex_row()
                }
            })
            .child(rail)
            .child(work)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn arrows_move_by_column_and_stop_at_the_edges() {
        // Seven tiles in rows of three.
        assert_eq!(step("right", None, 7, 3), Some(0));
        assert_eq!(step("right", Some(0), 7, 3), Some(1));
        assert_eq!(step("left", Some(0), 7, 3), Some(0));
        assert_eq!(step("right", Some(6), 7, 3), Some(6));
        assert_eq!(step("down", Some(1), 7, 3), Some(4));
        assert_eq!(step("down", Some(4), 7, 3), Some(4), "no tile below");
        assert_eq!(step("down", Some(3), 7, 3), Some(6));
        assert_eq!(step("up", Some(1), 7, 3), Some(1));
        assert_eq!(step("up", Some(5), 7, 3), Some(2));
        assert_eq!(step("home", Some(5), 7, 3), Some(0));
        assert_eq!(step("end", Some(0), 7, 3), Some(6));
        assert_eq!(step("x", Some(0), 7, 3), None);
        assert_eq!(step("down", None, 0, 3), None);
    }

    #[test]
    fn keys_show_as_short_npubs() {
        let key = buzz_identity::Identity::ephemeral().pubkey_hex();
        let short = short_key(&key, 8);
        assert!(short.starts_with("npub…"));
        assert_eq!(short.chars().count(), 13);
        assert_eq!(short_key("nothex", 8), "");
    }
}
