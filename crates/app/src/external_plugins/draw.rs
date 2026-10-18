//! Draws a plugin's element tree with the app's components, for its
//! views and for what it renders inside messages. Clicks and toggles go
//! to `send`; fields come ready-made from the caller, which keeps their
//! text.

use super::protocol::{AvatarSize, ButtonStyle, Gap, Justify, Node, TextStyle, Tone};
use crate::community::Community;
use crate::ui::markdown;
use crate::ui::navigation;
use buzz_ui::{
    ActiveTheme, Avatar, Badge, BadgeTone, Button, ButtonVariant, Checkbox, Colors, Divider, Icon,
    Radio, Spinner, Switch, TextField, TypeStyled, tokens::space,
};
use gpui_kit::component::input::InputState;
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::rc::Rc;

/// Sends an element's event to the plugin: its id, the event and a value.
pub type Send = Rc<dyn Fn(&str, &str, Option<Value>, &mut App)>;

pub struct Draw<'a> {
    pub send: Send,
    /// The text fields by id; an input without one draws nothing.
    pub fields: &'a HashMap<String, Entity<InputState>>,
    /// Markdown resolves mentions and channels in this community.
    pub community: Option<Entity<Community>>,
    /// Names where Markdown shows, and keeps element ids apart.
    pub place: &'a str,
    /// Buzz's own views the tree embeds, by element id; an embed without
    /// one draws nothing.
    pub embeds: &'a HashMap<String, AnyView>,
}

/// A composer or thread in the tree, for the caller to make its view.
pub enum EmbedNode<'a> {
    Composer {
        id: &'a str,
        channel: &'a str,
        thread: Option<&'a str>,
        parent: Option<&'a str>,
        placeholder: Option<&'a str>,
        /// The plugin takes the draft (`submit: "plugin"`).
        plugin_submits: bool,
        disabled: bool,
        auto_focus: bool,
        draft: Option<&'a str>,
    },
    Thread {
        id: &'a str,
        channel: &'a str,
        root: &'a str,
    },
    Message {
        id: &'a str,
        channel: &'a str,
        message: &'a str,
    },
}

/// Every embedded composer and thread in `node`.
pub fn embeds(node: &Node) -> Vec<EmbedNode<'_>> {
    let mut found = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            Node::Column { children, .. }
            | Node::Row { children, .. }
            | Node::Card { children, .. } => stack.extend(children.iter().rev()),
            Node::Composer {
                id,
                channel,
                thread,
                parent,
                placeholder,
                submit,
                disabled,
                auto_focus,
                draft,
            } => found.push(EmbedNode::Composer {
                id,
                channel,
                thread: thread.as_deref(),
                parent: parent.as_deref(),
                placeholder: placeholder.as_deref(),
                plugin_submits: submit.as_deref() == Some("plugin"),
                disabled: *disabled,
                auto_focus: *auto_focus,
                draft: draft.as_deref(),
            }),
            Node::Thread {
                id, channel, root, ..
            } => found.push(EmbedNode::Thread { id, channel, root }),
            Node::Message {
                id,
                channel,
                message,
            } => found.push(EmbedNode::Message {
                id,
                channel,
                message,
            }),
            _ => {}
        }
    }
    found
}

/// An input in the tree, for the caller to make its field.
pub struct InputNode<'a> {
    pub id: &'a str,
    pub value: Option<&'a String>,
    pub placeholder: Option<&'a String>,
    pub version: u64,
    pub clear_on_submit: bool,
}

/// Every input in `node`.
pub fn inputs(node: &Node) -> Vec<InputNode<'_>> {
    let mut found = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            Node::Column { children, .. }
            | Node::Row { children, .. }
            | Node::Card { children, .. } => stack.extend(children.iter().rev()),
            Node::Input {
                id,
                value,
                placeholder,
                version,
                clear_on_submit,
            } => found.push(InputNode {
                id,
                value: value.as_ref(),
                placeholder: placeholder.as_ref(),
                version: *version,
                clear_on_submit: *clear_on_submit,
            }),
            _ => {}
        }
    }
    found
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

fn children(nodes: &[Node], path: &str, draw: &Draw, cx: &App) -> Vec<AnyElement> {
    nodes
        .iter()
        .enumerate()
        .map(|(index, child)| node(child, &format!("{path}-{index}"), draw, cx))
        .collect()
}

/// Draws `node`; `path` names its place in the tree.
pub fn node(node: &Node, path: &str, draw: &Draw, cx: &App) -> AnyElement {
    let c = *cx.colors();
    let element_id =
        |kind: &str| SharedString::from(format!("plugin-{}-{kind}-{path}", draw.place));
    match node {
        Node::Column {
            children: nodes,
            gap,
        } => gap_of(div().flex().flex_col().min_w_0(), *gap)
            .children(children(nodes, path, draw, cx))
            .into_any_element(),
        Node::Row {
            children: nodes,
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
            row.children(children(nodes, path, draw, cx))
                .into_any_element()
        }
        Node::Card {
            title,
            children: nodes,
        } => div()
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
            .children(children(nodes, path, draw, cx))
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
        Node::Markdown { text } => match &draw.community {
            Some(community) => markdown::text(text, draw.place, community.read(cx), cx),
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
            let send = draw.send.clone();
            let id = id.clone();
            // Sized to its label, even in a column.
            div()
                .flex()
                .child(button.on_click(move |_, _, cx| send(&id, "click", None, cx)))
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
        Node::Input { id, .. } => match draw.fields.get(id) {
            Some(state) => TextField::new(state).into_any_element(),
            None => div().into_any_element(),
        },
        Node::Checkbox { id, checked, label } => {
            let send = draw.send.clone();
            let id = id.clone();
            let mut checkbox = Checkbox::new(element_id("checkbox"), *checked);
            if let Some(label) = label {
                checkbox = checkbox.label(label.clone());
            }
            checkbox
                .on_change(move |on, _, cx| send(&id, "toggle", Some(json!(on)), cx))
                .into_any_element()
        }
        Node::Choice { id, value, options } => div()
            .flex()
            .flex_col()
            .gap(space::S2)
            .children(options.iter().take(50).enumerate().map(|(index, option)| {
                let send = draw.send.clone();
                let (id, chosen) = (id.clone(), option.value.clone());
                Radio::new(
                    element_id(&format!("choice-{index}")),
                    option.label.clone(),
                    value.as_ref() == Some(&option.value),
                )
                .on_select(move |_, cx| send(&id, "change", Some(json!(chosen)), cx))
                .into_any_element()
            }))
            .into_any_element(),
        Node::Progress { value } => div()
            .w_full()
            .h(px(6.))
            .rounded(px(3.))
            .bg(c.border_standard)
            .child(
                div()
                    .h_full()
                    .rounded(px(3.))
                    .bg(c.text_accent)
                    .w(relative(value.clamp(0., 1.))),
            )
            .into_any_element(),
        Node::Image {
            url,
            width,
            height,
            alt,
        } => {
            let safe = url.starts_with("https://")
                || (url.starts_with("http://") && url.contains("://localhost"));
            let source = match &draw.community {
                Some(community) if safe => Some(crate::ui::media::image(
                    url,
                    &community.read(cx).session.media().clone(),
                )),
                _ if safe => Some(ImageSource::from(url.clone())),
                _ => None,
            };
            let width = width.map(|width| width.min(1200) as f32);
            let height = height.map(|height| height.min(1200) as f32);
            match source {
                Some(source) => {
                    let mut image = img(source).object_fit(ObjectFit::Contain).max_w_full();
                    if let Some(width) = width {
                        image = image.w(px(width));
                    }
                    if let Some(height) = height {
                        image = image.h(px(height));
                    }
                    image.into_any_element()
                }
                None => div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(alt.clone().unwrap_or_else(|| "Image".to_owned()))
                    .into_any_element(),
            }
        }
        Node::Message { id, .. } => match draw.embeds.get(id) {
            Some(view) => div().w_full().child(view.clone()).into_any_element(),
            None => div().into_any_element(),
        },
        Node::Composer { id, .. } => match draw.embeds.get(id) {
            Some(view) => div().w_full().child(view.clone()).into_any_element(),
            None => div().into_any_element(),
        },
        Node::Thread { id, height, .. } => match draw.embeds.get(id) {
            Some(view) => div()
                .w_full()
                .h(px(height.unwrap_or(480).clamp(120, 1200) as f32))
                .border_1()
                .border_color(c.border_standard)
                .rounded(rems(0.75))
                .overflow_hidden()
                .child(view.clone())
                .into_any_element(),
            None => div().into_any_element(),
        },
        Node::Avatar { pubkey, size } => {
            let profile = draw.community.as_ref().and_then(|community| {
                community
                    .read(cx)
                    .snapshot
                    .profiles
                    .get(&pubkey.to_ascii_lowercase())
                    .cloned()
            });
            let name = profile
                .as_ref()
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| pubkey.chars().take(2).collect());
            let mut avatar = Avatar::new(name).size(match size {
                AvatarSize::Small => buzz_ui::AvatarSize::Small,
                AvatarSize::Default => buzz_ui::AvatarSize::Default,
                AvatarSize::Large => buzz_ui::AvatarSize::Large,
            });
            if let Some(picture) = profile.and_then(|profile| profile.picture.clone())
                && picture.starts_with("https://")
            {
                avatar = avatar.image(crate::ui::media::any_avatar(&picture));
            }
            avatar.into_any_element()
        }
        Node::Switch { id, checked, label } => {
            let send = draw.send.clone();
            let id = id.clone();
            let mut switch = Switch::new(element_id("switch"), *checked);
            if let Some(label) = label {
                switch = switch.label(label.clone());
            }
            switch
                .on_change(move |on, _, cx| send(&id, "toggle", Some(json!(on)), cx))
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn inputs_are_found_in_order() {
        let tree = Node::parse(json!({
            "type": "column",
            "children": [
                { "type": "input", "id": "a" },
                { "type": "card", "children": [{ "type": "input", "id": "b", "version": 3 }] },
                { "type": "text", "text": "x" },
            ],
        }))
        .unwrap();
        let found: Vec<_> = inputs(&tree)
            .iter()
            .map(|input| (input.id.to_owned(), input.version))
            .collect();
        assert_eq!(found, [("a".to_owned(), 0), ("b".to_owned(), 3)]);
    }
}
