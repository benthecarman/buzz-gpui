//! A plugin's panel or page: the element tree it sent, drawn with the
//! app's components. Fields keep their text here, so typing does not wait
//! on the plugin; each change and Enter is sent to it as an event.

use super::draw::{self, Draw};
use super::protocol::Node;
use super::{ExternalPlugins, Status};
use crate::agents2::registry::AgentView;
use crate::community::{AppState, Community};
use crate::ui::account_actions::Close;
use crate::ui::channel_panels::Opening;
use buzz_ui::{ActiveTheme, Button, IconName, Spinner, TypeStyled, tokens::space};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

struct Field {
    state: Entity<InputState>,
    /// The tree's `version` when its value was last put in.
    version: u64,
    clear_on_submit: bool,
    _subscription: Subscription,
}

pub struct ExternalView {
    plugin: String,
    contribution: String,
    /// `panel`, `page` or `settings`.
    kind: &'static str,
    id: String,
    /// A panel's opening; `None` for a page.
    opening: Option<Opening>,
    /// An agent tab's agent, and its type's id.
    agent: Option<(AgentView, &'static str)>,
    /// More `view/open` params, such as an accessory's channel.
    extra: Value,
    /// Closes a dialog (`ui/closeView`).
    close: Option<crate::ui::account_actions::Close>,
    /// Picks a reaction (`reaction/select`).
    select: Option<crate::ui::emoji_picker::Select>,
    /// Sets a new agent's config (`agent/createConfig`).
    change: Option<crate::agents2::registry::CreateChange>,
    /// Ends a composer capture (`composer/accept`).
    accept: Option<Accept>,
    /// A page's route params, from `ui/navigate`.
    route: Option<Value>,
    /// A `workspace` page: no padding around the plugin's tree.
    edge_to_edge: bool,
    tree: Option<Node>,
    /// The last tree the plugin sent could not be drawn.
    error: Option<String>,
    fields: HashMap<String, Field>,
    /// Buzz's composers and threads the tree embeds, by element id, with
    /// what they were made for.
    embeds: HashMap<String, (String, AnyView)>,
    _release: Subscription,
}

impl ExternalView {
    pub fn panel(
        plugin: &str,
        contribution: &str,
        opening: Opening,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::open(
            plugin,
            contribution,
            "panel",
            |view| view.opening = Some(opening),
            cx,
        )
        .into()
    }

    pub fn page(
        plugin: &str,
        contribution: &str,
        edge_to_edge: bool,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::open(
            plugin,
            contribution,
            "page",
            |view| view.edge_to_edge = edge_to_edge,
            cx,
        )
        .into()
    }

    /// A Settings section the plugin added.
    pub fn settings(
        plugin: &str,
        contribution: &str,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::open(plugin, contribution, "settings", |_| {}, cx).into()
    }

    /// A settings tab of one of the plugin's agent types.
    pub fn agent_tab(
        plugin: &str,
        kind: &'static str,
        tab: &str,
        agent: AgentView,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::open(
            plugin,
            tab,
            "agentTab",
            |view| view.agent = Some((agent, kind)),
            cx,
        )
        .into()
    }

    /// An account action's dialog, titled `title`, with the plugin's view
    /// as its body.
    pub fn dialog(
        plugin: &str,
        contribution: &str,
        title: SharedString,
        close: Close,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::framed(plugin, contribution, title, close, Value::Null, cx).0
    }

    /// A dialog titled `title` around a plugin view of kind `dialog`, with
    /// `extra` in its `view/open`; and the view's id.
    pub(super) fn framed(
        plugin: &str,
        contribution: &str,
        title: SharedString,
        close: Close,
        extra: Value,
        cx: &mut App,
    ) -> (AnyView, String) {
        let inner = close.clone();
        let view = Self::open(
            plugin,
            contribution,
            "dialog",
            |view| {
                view.close = Some(inner);
                view.extra = extra;
            },
            cx,
        );
        let id = view.read(cx).id.clone();
        (cx.new(|_| Framed { title, view, close }).into(), id)
    }

    /// A composer accessory, shown above one composer.
    pub fn accessory(
        plugin: &str,
        contribution: &str,
        channel: &str,
        thread: Option<&str>,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        let extra = json!({ "channel": channel, "thread": thread });
        Self::open(
            plugin,
            contribution,
            "accessory",
            |view| view.extra = extra,
            cx,
        )
        .into()
    }

    /// A plugin agent type's part of the New agent dialog.
    pub fn agent_create(
        plugin: &str,
        kind: &'static str,
        config: &Value,
        change: crate::agents2::registry::CreateChange,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        let extra = json!({ "type": kind, "config": config });
        Self::open(
            plugin,
            kind,
            "agentCreate",
            |view| {
                view.extra = extra;
                view.change = Some(change);
            },
            cx,
        )
        .into()
    }

    /// A composer capture: the plugin's view in place of one composer's
    /// editor.
    pub(super) fn capture(
        plugin: &str,
        composer: u64,
        accept: Accept,
        cx: &mut App,
    ) -> (Entity<Self>, String) {
        let extra = json!({ "composer": composer });
        let view = Self::open(
            plugin,
            "capture",
            "capture",
            |view| {
                view.extra = extra;
                view.accept = Some(accept);
            },
            cx,
        );
        let id = view.read(cx).id.clone();
        (view, id)
    }

    /// What `composer/accept` calls: a capture's end.
    pub(super) fn acceptor(&self) -> Option<Accept> {
        self.accept.clone()
    }

    /// What `agent/createConfig` calls: a create form's change.
    pub(super) fn changer(&self) -> Option<crate::agents2::registry::CreateChange> {
        self.change.clone()
    }

    /// A plugin agent type's provider dialog body.
    pub fn agent_provider(
        plugin: &str,
        kind: &'static str,
        close: Close,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        let extra = json!({ "type": kind });
        Self::open(
            plugin,
            kind,
            "agentProvider",
            |view| {
                view.extra = extra;
                view.close = Some(close);
            },
            cx,
        )
        .into()
    }

    /// A reaction picker, standing in for the emoji picker.
    pub fn reaction_picker(
        plugin: &str,
        contribution: &str,
        select: crate::ui::emoji_picker::Select,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::open(
            plugin,
            contribution,
            "reactionPicker",
            |view| view.select = Some(select),
            cx,
        )
        .into()
    }

    /// What `reaction/select` calls: a reaction picker's pick.
    pub(super) fn selector(&self) -> Option<crate::ui::emoji_picker::Select> {
        self.select.clone()
    }

    /// What `ui/closeView` calls: a dialog's close; `None` for other
    /// views.
    pub(super) fn closer(&self) -> Option<Close> {
        self.close
            .clone()
            .or_else(|| self.opening.as_ref().map(|opening| opening.close.clone()))
    }

    /// `agent/save`: replaces this tab's agent's config.
    pub(super) fn save_agent(
        &self,
        config: Value,
        cx: &mut App,
    ) -> Option<Task<Result<(), String>>> {
        let (agent, _) = self.agent.as_ref()?;
        Some(agent.save(config, cx))
    }

    /// A view of `kind`, which `setup` fills in before the plugin hears of
    /// it.
    fn open(
        plugin: &str,
        contribution: &str,
        kind: &'static str,
        setup: impl FnOnce(&mut Self),
        cx: &mut App,
    ) -> Entity<Self> {
        let id = ExternalPlugins::next_view_id(cx);
        let view = cx.new(|cx| {
            let mut view = Self {
                plugin: plugin.to_owned(),
                contribution: contribution.to_owned(),
                kind,
                id: id.clone(),
                opening: None,
                agent: None,
                extra: Value::Null,
                close: None,
                select: None,
                change: None,
                accept: None,
                route: None,
                edge_to_edge: false,
                tree: None,
                error: None,
                fields: HashMap::new(),
                embeds: HashMap::new(),
                _release: cx.on_release(|this: &mut Self, cx| {
                    ExternalPlugins::close_view(&this.plugin, &this.id, cx);
                }),
            };
            setup(&mut view);
            view
        });
        let params = view.read(cx).open_params();
        ExternalPlugins::open_view(plugin, &id, view.downgrade(), params, cx);
        view
    }

    /// `view/open`'s params.
    pub(super) fn open_params(&self) -> Value {
        let opening = self.opening.as_ref();
        let (agent, kind) = match &self.agent {
            Some((view, kind)) => (Some(super::agents::agent_json(&view.agent)), Some(*kind)),
            None => (None, None),
        };
        let mut params = json!({
            "route": self.route,
            "agent": agent,
            "type": kind,
            "view": self.id,
            "contribution": self.contribution,
            "kind": self.kind,
            "target": opening.map(|opening| opening.target.clone()),
            "channel": opening.map(|opening| opening.channel.clone()),
            "thread": opening.and_then(|opening| opening.thread.clone()),
        });
        if let Value::Object(extra) = &self.extra {
            for (key, value) in extra {
                params[key] = value.clone();
            }
        }
        params
    }

    pub(super) fn set_tree(&mut self, tree: Result<Node, String>, cx: &mut Context<Self>) {
        match tree {
            Ok(tree) => {
                self.tree = Some(tree);
                self.error = None;
            }
            Err(reason) => {
                tracing::warn!(plugin = %self.plugin, %reason, "plugin view not drawn");
                self.error = Some(reason);
            }
        }
        cx.notify();
    }

    fn community(&self, cx: &App) -> Option<Entity<Community>> {
        match &self.opening {
            Some(opening) => Some(opening.community.clone()),
            None => AppState::current(cx),
        }
    }

    fn event(&self, id: &str, event: &str, value: Option<Value>, cx: &App) {
        let mut params = json!({ "view": self.id, "id": id, "event": event });
        if let Some(value) = value {
            params["value"] = value;
        }
        ExternalPlugins::send_event(&self.plugin, params, cx);
    }

    /// The field `id`, made on first sight; `true` when just made.
    fn field(
        &mut self,
        id: &str,
        placeholder: Option<&String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (&mut Field, bool) {
        let created = !self.fields.contains_key(id);
        if created {
            let state = cx.new(|cx| {
                let state = InputState::new(window, cx);
                match placeholder {
                    Some(placeholder) => state.placeholder(placeholder.clone()),
                    None => state,
                }
            });
            let field_id = id.to_owned();
            let subscription = cx.subscribe_in(
                &state,
                window,
                move |this, state, event: &InputEvent, window, cx| {
                    let text = state.read(cx).value().to_string();
                    match event {
                        InputEvent::Change => {
                            this.event(&field_id, "change", Some(json!(text)), cx)
                        }
                        InputEvent::PressEnter { .. } => {
                            this.event(&field_id, "submit", Some(json!(text)), cx);
                            if this
                                .fields
                                .get(&field_id)
                                .is_some_and(|field| field.clear_on_submit)
                            {
                                state.update(cx, |state, cx| state.set_value("", window, cx));
                            }
                        }
                        _ => {}
                    }
                },
            );
            self.fields.insert(
                id.to_owned(),
                Field {
                    state,
                    version: 0,
                    clear_on_submit: false,
                    _subscription: subscription,
                },
            );
        }
        (self.fields.get_mut(id).expect("just added"), created)
    }

    /// Makes the composers and threads the tree names, keeps those it
    /// still names for the same channel, and drops the rest.
    fn sync_embeds(&mut self, tree: &Node, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = self.community(cx) else {
            self.embeds.clear();
            return;
        };
        let mut seen = HashSet::new();
        for embed in draw::embeds(tree) {
            let (id, made_for) = match &embed {
                draw::EmbedNode::Composer {
                    id,
                    channel,
                    thread,
                    ..
                } => (
                    *id,
                    format!("composer:{channel}:{}", thread.unwrap_or_default()),
                ),
                draw::EmbedNode::Thread { id, channel, root } => {
                    (*id, format!("thread:{channel}:{root}"))
                }
            };
            seen.insert(id.to_owned());
            if self
                .embeds
                .get(id)
                .is_some_and(|(made, _)| *made == made_for)
            {
                continue;
            }
            // Only channels you can read.
            let readable = |channel: &str| {
                community
                    .read(cx)
                    .snapshot
                    .channels
                    .iter()
                    .any(|known| known.id == channel)
            };
            let view: Option<AnyView> = match embed {
                draw::EmbedNode::Composer {
                    channel,
                    thread,
                    placeholder,
                    ..
                } if readable(channel) => {
                    let root =
                        thread.and_then(|root| buzz_relay_client::EventId::from_hex(root).ok());
                    let placeholder: SharedString =
                        placeholder.unwrap_or("Message").to_owned().into();
                    let (community, channel) = (community.clone(), channel.to_owned());
                    Some(
                        cx.new(|cx| {
                            crate::ui::composer::Composer::new(
                                community,
                                channel,
                                root,
                                placeholder,
                                window,
                                cx,
                            )
                        })
                        .into(),
                    )
                }
                draw::EmbedNode::Thread { id, channel, root } if readable(channel) => {
                    match buzz_relay_client::EventId::from_hex(root) {
                        Ok(root) => {
                            let weak = cx.entity().downgrade();
                            let element = id.to_owned();
                            let close: crate::ui::thread::OnClose =
                                std::sync::Arc::new(move |_, cx| {
                                    if let Some(this) = weak.upgrade() {
                                        this.read(cx).event(&element, "close", None, cx);
                                    }
                                });
                            let (community, channel) = (community.clone(), channel.to_owned());
                            Some(
                                cx.new(|cx| {
                                    crate::ui::thread::ThreadPanel::new(
                                        community, channel, root, close, window, cx,
                                    )
                                })
                                .into(),
                            )
                        }
                        Err(_) => None,
                    }
                }
                _ => None,
            };
            match view {
                Some(view) => {
                    self.embeds.insert(id.to_owned(), (made_for, view));
                }
                None => {
                    self.embeds.remove(id);
                }
            }
        }
        self.embeds.retain(|id, _| seen.contains(id));
    }

    /// What shows before the plugin's tree, or instead of it.
    fn placeholder(&self, status: Status, cx: &App) -> Option<AnyElement> {
        let c = cx.colors();
        let message = |text: String| {
            div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(text)
        };
        match status {
            Status::Off => Some(
                message("This plugin is off. Turn it on in Settings → Plugins.".into())
                    .into_any_element(),
            ),
            Status::Paused => Some(
                message("Plugins you added are paused for this launch (safe mode).".into())
                    .into_any_element(),
            ),
            Status::Failed(reason) => {
                let plugin = self.plugin.clone();
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space::S3)
                        .child(div().text_label().child("This plugin stopped"))
                        .child(
                            div()
                                .text_body_sm()
                                .text_color(c.text_subtle)
                                .font_family(buzz_ui::tokens::font::MONO)
                                .child(reason),
                        )
                        .child(
                            div().flex().child(
                                Button::new("plugin-restart", "Restart")
                                    .icon(IconName::ArrowClockwise)
                                    .on_click(move |_, _, cx| {
                                        ExternalPlugins::restart(&plugin, cx)
                                    }),
                            ),
                        )
                        .into_any_element(),
                )
            }
            Status::Starting | Status::Running if self.tree.is_none() => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S2)
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(Spinner::new().size(14.))
                    .child("Loading…")
                    .into_any_element(),
            ),
            Status::Starting | Status::Running => None,
        }
    }
}

impl Render for ExternalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let status = ExternalPlugins::status(&self.plugin, cx);
        // Panels, pages and Settings sections fill their place; agent tabs,
        // dialogs and accessories sit in the host's own frame.
        let framed = matches!(self.kind, "panel" | "page" | "settings");
        // A reaction picker sits in the picker's own padded surface.
        let mut body = div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .when(framed && !self.edge_to_edge, |body| body.p(space::S6));
        if let Some(placeholder) = self.placeholder(status, cx) {
            body = body.child(placeholder);
        } else if let Some(tree) = self.tree.clone() {
            let mut seen = HashSet::new();
            for input in draw::inputs(&tree) {
                seen.insert(input.id.to_owned());
                let (field, created) = self.field(input.id, input.placeholder, window, cx);
                field.clear_on_submit = input.clear_on_submit;
                if created || field.version != input.version {
                    field.version = input.version;
                    let text = input.value.cloned().unwrap_or_default();
                    field
                        .state
                        .update(cx, |state, cx| state.set_value(text, window, cx));
                }
            }
            // Fields the tree no longer has start empty if they return.
            self.fields.retain(|id, _| seen.contains(id));
            self.sync_embeds(&tree, window, cx);
            let embeds: HashMap<String, AnyView> = self
                .embeds
                .iter()
                .map(|(id, (_, view))| (id.clone(), view.clone()))
                .collect();
            let fields: HashMap<String, Entity<InputState>> = self
                .fields
                .iter()
                .map(|(id, field)| (id.clone(), field.state.clone()))
                .collect();
            let weak = cx.entity().downgrade();
            let draw = Draw {
                send: Rc::new(move |id, event, value, cx| {
                    if let Some(this) = weak.upgrade() {
                        this.read(cx).event(id, event, value, cx);
                    }
                }),
                fields: &fields,
                embeds: &embeds,
                community: self.community(cx),
                place: &self.contribution,
            };
            body = body.child(draw::node(&tree, &self.id, &draw, cx));
        }
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .text_body_sm()
                    .text_color(c.text_danger)
                    .child(error.clone()),
            );
        }
        div()
            .id(SharedString::from(format!("plugin-view-{}", self.id)))
            .w_full()
            .when(framed, |view| {
                view.size_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .bg(c.surface_panel)
            })
            .text_color(c.text_standard)
            .child(body)
    }
}

/// Whether `params` can be a plugin page's route: a JSON object of at
/// most 4 KiB.
pub fn route_valid(params: &Value) -> bool {
    params.is_object() && params.to_string().len() <= 4_096
}

/// Shows a plugin page at `params`, or its start: the page hears
/// `page/route`.
pub fn open_route(view: &AnyView, params: Option<&Value>, _: &mut Window, cx: &mut App) {
    let Ok(view) = view.clone().downcast::<ExternalView>() else {
        return;
    };
    view.update(cx, |view, cx| {
        view.route = params.cloned();
        ExternalPlugins::notify_running(
            &view.plugin,
            "page/route",
            json!({ "view": view.id, "route": view.route }),
            cx,
        );
    });
}

/// Ends a composer capture, attaching a file `(name, mime, bytes)` if
/// the plugin made one.
pub type Accept = Rc<dyn Fn(Option<(String, String, Vec<u8>)>, &mut Window, &mut App)>;

/// An account action's dialog around the plugin's view.
struct Framed {
    title: SharedString,
    view: Entity<ExternalView>,
    close: Close,
}

impl Render for Framed {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let close = self.close.clone();
        buzz_ui::Dialog::new("plugin-dialog", self.title.clone())
            .on_close(move |_, window, cx| close(window, cx))
            .child(self.view.clone())
    }
}
