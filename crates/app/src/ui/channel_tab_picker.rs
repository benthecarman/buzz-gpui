//! The New tab picker (`ChannelTabPicker.tsx`): choose another channel,
//! direct message or channel tool to open as a tab.

use super::message::channel_icon;
use super::sidebar::channel_label;
use crate::community::Community;
use buzz_relay_client::search::match_name;
use buzz_relay_client::{ChannelSummary, ChannelType};
use buzz_ui::{
    ActiveTheme, IconName, NavigationItem, SearchField, Tab, TabVariant, Tabs, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Channels,
    Dms,
    Tools,
}

const CATEGORIES: [(Category, &str); 3] = [
    (Category::Channels, "Channels"),
    (Category::Dms, "DMs"),
    (Category::Tools, "Tools"),
];

/// A channel tool the picker can open.
#[derive(Clone)]
pub struct Tool {
    pub key: &'static str,
    pub title: &'static str,
    pub icon: IconName,
}

pub type ChooseChannel = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type ChooseTool = Rc<dyn Fn(&'static str, &mut Window, &mut App)>;

pub struct ChannelTabPicker {
    community: Entity<Community>,
    /// The conversation the tabs belong to; it is not offered.
    current: String,
    tools: Vec<Tool>,
    category: Category,
    input: Entity<InputState>,
    /// The highlighted choice's key; nothing until the user types.
    highlighted: Option<String>,
    on_choose: ChooseChannel,
    on_tool: ChooseTool,
    _subscription: Subscription,
}

/// One row: its key, label, icon and match rank.
struct Choice {
    key: String,
    label: SharedString,
    icon: IconName,
    rank: u8,
}

impl ChannelTabPicker {
    pub fn new(
        community: Entity<Community>,
        current: String,
        tools: Vec<Tool>,
        on_choose: ChooseChannel,
        on_tool: ChooseTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search channels…"));
        input.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.highlighted = this.choices(cx).first().map(|c| c.key.clone());
                if this.needle(cx).is_empty() {
                    this.highlighted = None;
                }
                cx.notify();
            }
        });
        Self {
            community,
            current,
            tools,
            category: Category::Channels,
            input,
            highlighted: None,
            on_choose,
            on_tool,
            _subscription: subscription,
        }
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let input = this.read(cx).input.clone();
        input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn needle(&self, cx: &App) -> String {
        let value = self.input.read(cx).value().trim().to_lowercase();
        value.trim_start_matches('#').to_string()
    }

    fn set_category(&mut self, category: Category, window: &mut Window, cx: &mut Context<Self>) {
        self.category = category;
        let placeholder = match category {
            Category::Tools => "Search tools…",
            Category::Dms => "Search direct messages…",
            Category::Channels => "Search channels…",
        };
        self.input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.set_placeholder(placeholder, window, cx);
            state.focus(window, cx);
        });
        self.highlighted = None;
        cx.notify();
    }

    /// Conversations a tab can show: joined, writable, not sessions, not
    /// the one already open.
    fn destinations(&self, cx: &App) -> Vec<ChannelSummary> {
        let snapshot = &self.community.read(cx).snapshot;
        snapshot
            .channels
            .iter()
            .filter(|channel| {
                channel.id != self.current
                    && !channel.read_only
                    && !channel.cached
                    && !channel.archived
                    && channel.channel_type != Some(ChannelType::Session)
                    && !snapshot.hidden_dms.contains(&channel.id)
            })
            .cloned()
            .collect()
    }

    fn choices(&self, cx: &App) -> Vec<Choice> {
        let needle = self.needle(cx);
        let profiles = &self.community.read(cx).snapshot.profiles;
        let all: Vec<(String, SharedString, IconName)> = match self.category {
            Category::Tools => self
                .tools
                .iter()
                .map(|tool| (format!("tool:{}", tool.key), tool.title.into(), tool.icon))
                .collect(),
            category => self
                .destinations(cx)
                .into_iter()
                .filter(|channel| {
                    (channel.channel_type == Some(ChannelType::Dm)) == (category == Category::Dms)
                })
                .map(|channel| {
                    (
                        format!("channel:{}", channel.id),
                        channel_label(&channel, profiles),
                        channel_icon(&channel),
                    )
                })
                .collect(),
        };
        let mut matched: Vec<Choice> = all
            .into_iter()
            .filter_map(|(key, label, icon)| {
                let rank = match needle.is_empty() {
                    true => 0,
                    false => match_name(&label, &needle)?,
                };
                Some(Choice {
                    key,
                    label,
                    icon,
                    rank,
                })
            })
            .collect();
        // Stable within a rank, as the list arrived.
        matched.sort_by_key(|choice| choice.rank);
        matched
    }

    fn run(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(channel) = key.strip_prefix("channel:") {
            (self.on_choose)(channel.to_string(), window, cx);
        } else if let Some(tool) = key.strip_prefix("tool:")
            && let Some(tool) = self.tools.iter().find(|t| t.key == tool)
        {
            (self.on_tool)(tool.key, window, cx);
        }
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let keys: Vec<String> = self.choices(cx).into_iter().map(|c| c.key).collect();
        if keys.is_empty() {
            return;
        }
        let index = self
            .highlighted
            .as_ref()
            .and_then(|key| keys.iter().position(|k| k == key));
        // Clamped at both ends; the first move from nothing lands on an end.
        let next = match index {
            None if delta > 0 => 0,
            None => keys.len() - 1,
            Some(index) => (index as isize + delta).clamp(0, keys.len() as isize - 1) as usize,
        };
        self.highlighted = Some(keys[next].clone());
        cx.notify();
    }

    fn empty_text(&self, cx: &App) -> &'static str {
        if !self.needle(cx).is_empty() {
            return "No matches. Try another name.";
        }
        match self.category {
            Category::Tools => {
                "No channel tools are available. Enable tools in Settings → Plugins. Terminal requires Buzz Desktop."
            }
            Category::Dms => "No direct messages available yet.",
            Category::Channels => "No other channels available yet.",
        }
    }
}

impl Render for ChannelTabPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let choices = self.choices(cx);
        if let Some(key) = &self.highlighted
            && !choices.iter().any(|choice| &choice.key == key)
        {
            // The highlighted row left: fall back to the first.
            self.highlighted = choices.first().map(|choice| choice.key.clone());
        }
        let category = self.category;
        let tabs = CATEGORIES
            .iter()
            .fold(Tabs::new(TabVariant::Panel), |tabs, (id, label)| {
                let id = *id;
                tabs.tab(
                    Tab::new(SharedString::from(format!("picker-{label}")), *label)
                        .selected(category == id)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_category(id, window, cx)
                        })),
                )
            });
        let empty = choices.is_empty().then(|| {
            div()
                .p(rems(0.5))
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(self.empty_text(cx))
        });
        let rows = choices.into_iter().map(|choice| {
            let key = choice.key.clone();
            NavigationItem::new(SharedString::from(choice.key.clone()), choice.label)
                .icon(choice.icon)
                .selected(self.highlighted.as_ref() == Some(&choice.key))
                .on_click(cx.listener(move |this, _, window, cx| this.run(&key, window, cx)))
        });
        div()
            .id("channel-tab-picker")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .px(rems(1.5))
            .py(rems(1.))
            .gap(rems(1.))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "down" => this.move_highlight(1, cx),
                    "up" => this.move_highlight(-1, cx),
                    "enter" => {
                        if let Some(key) = this.highlighted.clone() {
                            this.run(&key, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(tabs)
            .child(
                div()
                    .id("channel-tab-choices")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(SearchField::new(&self.input))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.125))
                            .mt(rems(1.))
                            .children(rows)
                            .children(empty),
                    ),
            )
    }
}
