//! A plugin's panel or page: the element tree it sent, drawn with the
//! app's components. Fields keep their text here, so typing does not wait
//! on the plugin; each change and Enter is sent to it as an event.

use super::draw::{self, Draw};
use super::protocol::Node;
use super::{ExternalPlugins, Status};
use crate::community::{AppState, Community};
use crate::ui::channel_panels::Opening;
use buzz_ui::{ActiveTheme, Button, IconName, Spinner, TypeStyled, tokens::space};
use gpui_kit::component::input::{InputEvent, InputState};
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
    tree: Option<Node>,
    /// The last tree the plugin sent could not be drawn.
    error: Option<String>,
    fields: HashMap<String, Field>,
    _release: Subscription,
}

impl ExternalView {
    pub fn panel(
        plugin: &str,
        contribution: &str,
        opening: Opening,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::open(plugin, contribution, "panel", Some(opening), window, cx)
    }

    pub fn page(plugin: &str, contribution: &str, window: &mut Window, cx: &mut App) -> AnyView {
        Self::open(plugin, contribution, "page", None, window, cx)
    }

    /// A Settings section the plugin added.
    pub fn settings(
        plugin: &str,
        contribution: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        Self::open(plugin, contribution, "settings", None, window, cx)
    }

    fn open(
        plugin: &str,
        contribution: &str,
        kind: &'static str,
        opening: Option<Opening>,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        let id = ExternalPlugins::next_view_id(cx);
        let view = cx.new(|cx| Self {
            plugin: plugin.to_owned(),
            contribution: contribution.to_owned(),
            kind,
            id: id.clone(),
            opening,
            tree: None,
            error: None,
            fields: HashMap::new(),
            _release: cx.on_release(|this: &mut Self, cx| {
                ExternalPlugins::close_view(&this.plugin, &this.id, cx);
            }),
        });
        let params = view.read(cx).open_params();
        ExternalPlugins::open_view(plugin, &id, view.downgrade(), params, cx);
        view.into()
    }

    /// `view/open`'s params.
    pub(super) fn open_params(&self) -> Value {
        let opening = self.opening.as_ref();
        json!({
            "view": self.id,
            "contribution": self.contribution,
            "kind": self.kind,
            "target": opening.map(|opening| opening.target.clone()),
            "channel": opening.map(|opening| opening.channel.clone()),
            "thread": opening.and_then(|opening| opening.thread.clone()),
        })
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
        let mut body = div().flex().flex_col().gap(space::S4).p(space::S6);
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
            .size_full()
            .min_h_0()
            .overflow_y_scroll()
            .bg(c.surface_panel)
            .text_color(c.text_standard)
            .child(body)
    }
}
