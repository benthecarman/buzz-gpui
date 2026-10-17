//! A plugin's panel or page: the element tree it sent, drawn with the
//! app's components. Fields keep their text here, so typing does not wait
//! on the plugin; each change and Enter is sent to it as an event.

use super::protocol::{ButtonStyle, Gap, Justify, Node, TextStyle, Tone};
use super::{ExternalPlugins, Status};
use crate::community::{AppState, Community};
use crate::ui::channel_panels::Opening;
use crate::ui::markdown;
use crate::ui::navigation;
use buzz_ui::{
    ActiveTheme, Badge, BadgeTone, Button, ButtonVariant, Colors, Divider, Icon, IconName, Spinner,
    Switch, TextField, TypeStyled, tokens::space,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

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
        Self::open(plugin, contribution, Some(opening), window, cx)
    }

    pub fn page(plugin: &str, contribution: &str, window: &mut Window, cx: &mut App) -> AnyView {
        Self::open(plugin, contribution, None, window, cx)
    }

    fn open(
        plugin: &str,
        contribution: &str,
        opening: Option<Opening>,
        _window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        let id = ExternalPlugins::next_view_id(cx);
        let view = cx.new(|cx| Self {
            plugin: plugin.to_owned(),
            contribution: contribution.to_owned(),
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
            "kind": if opening.is_some() { "panel" } else { "page" },
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

    fn node(
        &mut self,
        node: &Node,
        path: &str,
        seen: &mut HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let element_id = |kind: &str| SharedString::from(format!("plugin-{kind}-{path}"));
        match node {
            Node::Column { children, gap } => gap_of(div().flex().flex_col().min_w_0(), *gap)
                .children(self.children(children, path, seen, window, cx))
                .into_any_element(),
            Node::Row {
                children,
                gap,
                justify,
                wrap,
            } => {
                let row = gap_of(div().flex().flex_row().items_center().min_w_0(), *gap);
                let row = match justify {
                    Justify::Start => row,
                    Justify::Center => row.justify_center(),
                    Justify::End => row.justify_end(),
                    Justify::Between => row.justify_between(),
                };
                let row = if *wrap { row.flex_wrap() } else { row };
                row.children(self.children(children, path, seen, window, cx))
                    .into_any_element()
            }
            Node::Card { title, children } => div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .p(space::S4)
                .border_1()
                .border_color(c.border_standard)
                .rounded(rems(0.75))
                .children(
                    title
                        .as_ref()
                        .map(|title| div().text_label().child(title.clone())),
                )
                .children(self.children(children, path, seen, window, cx))
                .into_any_element(),
            Node::Text { text, style, tone } => {
                let text = div().child(text.clone());
                let text = match style {
                    TextStyle::Body => text.text_body(),
                    TextStyle::Small => text.text_body_sm(),
                    TextStyle::Label => text.text_label(),
                    TextStyle::Title => text.text_title(),
                    TextStyle::Heading => text.text_heading(),
                    TextStyle::Code => text.text_mono(),
                };
                match tone_color(*tone, &c) {
                    Some(color) => text.text_color(color),
                    None => text,
                }
                .into_any_element()
            }
            Node::Markdown { text } => match self.community(cx) {
                Some(community) => markdown::text(text, &self.contribution, community.read(cx), cx),
                None => div().text_body().child(text.clone()).into_any_element(),
            },
            Node::Button {
                id,
                label,
                variant,
                icon,
                disabled,
                loading,
            } => {
                let (variant, destructive) = match variant {
                    ButtonStyle::Prominent => (ButtonVariant::Prominent, false),
                    ButtonStyle::Subtle => (ButtonVariant::Subtle, false),
                    ButtonStyle::Ghost => (ButtonVariant::Ghost, false),
                    ButtonStyle::Outline => (ButtonVariant::Outline, false),
                    ButtonStyle::Destructive => (ButtonVariant::Subtle, true),
                };
                let mut button = Button::new(element_id("button"), label.clone())
                    .variant(variant)
                    .disabled(*disabled)
                    .loading(*loading);
                if destructive {
                    button = button.destructive();
                }
                if let Some(icon) = icon.as_deref().map(|name| super::icon(Some(name))) {
                    button = button.icon(icon);
                }
                let weak = cx.entity().downgrade();
                let id = id.clone();
                // Sized to its label, even in a column.
                div()
                    .flex()
                    .child(button.on_click(move |_, _, cx| {
                        if let Some(this) = weak.upgrade() {
                            this.read(cx).event(&id, "click", None, cx);
                        }
                    }))
                    .into_any_element()
            }
            Node::Link { label, url } => {
                let url = url.clone();
                div()
                    .id(element_id("link"))
                    .text_body()
                    .text_color(c.text_accent)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child(label.clone())
                    .on_click(move |event, window, cx| {
                        if url.starts_with("buzz://") {
                            navigation::open_url(&url, window, cx);
                        } else if url.starts_with("https://") || url.starts_with("http://") {
                            navigation::open_web_link(&url, event.modifiers(), window, cx);
                        }
                    })
                    .into_any_element()
            }
            Node::Input {
                id,
                value,
                placeholder,
                version,
                clear_on_submit,
            } => {
                seen.insert(id.clone());
                let (field, created) = self.field(id, placeholder.as_ref(), window, cx);
                field.clear_on_submit = *clear_on_submit;
                if created || field.version != *version {
                    field.version = *version;
                    let text = value.clone().unwrap_or_default();
                    field
                        .state
                        .update(cx, |state, cx| state.set_value(text, window, cx));
                }
                TextField::new(&field.state).into_any_element()
            }
            Node::Switch { id, checked, label } => {
                let weak = cx.entity().downgrade();
                let id = id.clone();
                let mut switch = Switch::new(element_id("switch"), *checked);
                if let Some(label) = label {
                    switch = switch.label(label.clone());
                }
                switch
                    .on_change(move |on, _, cx| {
                        if let Some(this) = weak.upgrade() {
                            this.read(cx).event(&id, "toggle", Some(json!(on)), cx);
                        }
                    })
                    .into_any_element()
            }
            Node::Badge { text, tone } => Badge::new(text.clone())
                .tone(match tone {
                    Tone::Default | Tone::Subtle => BadgeTone::Neutral,
                    Tone::Accent => BadgeTone::Accent,
                    Tone::Success => BadgeTone::Success,
                    Tone::Warning => BadgeTone::Warning,
                    Tone::Danger => BadgeTone::Danger,
                })
                .into_any_element(),
            Node::Icon { name } => Icon::new(super::icon(Some(name)))
                .size(16.)
                .into_any_element(),
            Node::Divider => Divider::horizontal().into_any_element(),
            Node::Spinner => Spinner::new().size(16.).into_any_element(),
            Node::Unknown => div().into_any_element(),
        }
    }

    fn children(
        &mut self,
        children: &[Node],
        path: &str,
        seen: &mut HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        children
            .iter()
            .enumerate()
            .map(|(index, child)| self.node(child, &format!("{path}-{index}"), seen, window, cx))
            .collect()
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

fn gap_of(div: Div, gap: Gap) -> Div {
    match gap {
        Gap::None => div,
        Gap::Sm => div.gap(space::S2),
        Gap::Md => div.gap(space::S4),
        Gap::Lg => div.gap(space::S6),
    }
}

fn tone_color(tone: Tone, c: &Colors) -> Option<Hsla> {
    match tone {
        Tone::Default => None,
        Tone::Subtle => Some(c.text_subtle),
        Tone::Accent => Some(c.text_accent),
        Tone::Success => Some(c.text_success),
        Tone::Warning => Some(c.text_warning),
        Tone::Danger => Some(c.text_danger),
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
            body = body.child(self.node(&tree, "0", &mut seen, window, cx));
            // Fields the tree no longer has start empty if they return.
            self.fields.retain(|id, _| seen.contains(id));
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
