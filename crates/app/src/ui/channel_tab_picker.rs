//! The New tab picker (`ChannelTabPicker.tsx`): choose another channel,
//! direct message or channel tool to open as a tab. It matches, ranks and
//! highlights as Command-K does.

use super::message::channel_icon;
use super::search_kit::usage::{self, SearchUsage, picker_text, record_choice, search_order};
use super::search_kit::{Highlight, match_name, matched_label};
use super::sidebar::channel_label;
use crate::community::Community;
use buzz_relay_client::{ChannelSummary, ChannelType};
use buzz_ui::{
    ActiveTheme, IconName, NavigationItem, SearchField, Tab, TabVariant, Tabs, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;
use std::collections::HashSet;
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
    highlight: Highlight,
    /// Read once, so a choice made here cannot reorder the list.
    usage: SearchUsage,
    on_choose: ChooseChannel,
    on_tool: ChooseTool,
    _subscription: Subscription,
}

/// One row, in display order.
struct Choice {
    key: String,
    label: SharedString,
    icon: IconName,
    positions: Vec<usize>,
}

/// A candidate row with its match rank (`None` before anything is typed).
type Candidate = (Choice, Option<u8>);

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
        let subscription = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let usage = usage::usage_of(community.read(cx), cx);
        Self {
            community,
            current,
            tools,
            category: Category::Channels,
            input,
            highlight: Highlight::new(),
            usage,
            on_choose,
            on_tool,
            _subscription: subscription,
        }
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let input = this.read(cx).input.clone();
        input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn needle(&self, cx: &App) -> String {
        let value = self.query(cx).trim().to_lowercase();
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

    /// The rows for the typed text, in the shared search order. Everything
    /// here is on this device, so fuzzy matches are stable: "bgp" finds
    /// buzz-github-prs.
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
        let matched: Vec<Candidate> = all
            .into_iter()
            .filter_map(|(key, label, icon)| {
                let found = match needle.is_empty() {
                    true => None,
                    false => Some(match_name(&label, &needle)?),
                };
                let rank = found.as_ref().map(|found| found.rank);
                let positions = found.map(|found| found.positions).unwrap_or_default();
                let choice = Choice {
                    key,
                    label,
                    icon,
                    positions,
                };
                Some((choice, rank))
            })
            .collect();
        let typed = picker_text("tab", &needle);
        let keys: HashSet<String> = matched
            .iter()
            .map(|(choice, _)| choice.key.clone())
            .collect();
        let picked = self.usage.pick(&typed, &keys);
        let mut ordered: Vec<(f64, Choice)> = matched
            .into_iter()
            .map(|(choice, rank)| {
                let order = rank.map_or(0., |rank| {
                    search_order(&self.usage, picked.as_deref(), &choice.key, rank as f64)
                });
                (order, choice)
            })
            .collect();
        // Stable within an order, as the list arrived.
        ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
        ordered.into_iter().map(|(_, choice)| choice).collect()
    }

    fn run(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let typed = picker_text("tab", &self.needle(cx));
        if !typed.is_empty() {
            record_choice(self.community.read(cx), &typed, key, cx);
        }
        if let Some(channel) = key.strip_prefix("channel:") {
            (self.on_choose)(channel.to_string(), window, cx);
        } else if let Some(tool) = key.strip_prefix("tool:")
            && let Some(tool) = self.tools.iter().find(|t| t.key == tool)
        {
            (self.on_tool)(tool.key, window, cx);
        }
    }

    fn empty_text(&self, cx: &App) -> &'static str {
        if !self.query(cx).trim().is_empty() {
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
        let keys: Vec<String> = choices.iter().map(|choice| choice.key.clone()).collect();
        let query = self.query(cx);
        self.highlight.sync(&query, &keys);
        self.highlight
            .reveal(|key| keys.iter().position(|k| k == key));
        let active = self.highlight.active().map(str::to_owned);
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
            let hover_key = choice.key.clone();
            let row_keys = keys.clone();
            div()
                .id(SharedString::from(format!("row-{}", choice.key)))
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    if this.highlight.hover(&hover_key, event.position, &row_keys) {
                        cx.notify();
                    }
                }))
                .child(
                    NavigationItem::new(
                        SharedString::from(choice.key.clone()),
                        choice.label.clone(),
                    )
                    .styled_label(matched_label(choice.label, &choice.positions))
                    .icon(choice.icon)
                    .selected(active.as_ref() == Some(&choice.key))
                    .on_click(cx.listener(move |this, _, window, cx| this.run(&key, window, cx))),
                )
        });
        let step_keys = keys.clone();
        div()
            .id("channel-tab-picker")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .px(rems(1.5))
            .py(rems(1.))
            .gap(rems(1.))
            .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "down" => {
                        this.highlight.step(true, &step_keys);
                    }
                    "up" => {
                        this.highlight.step(false, &step_keys);
                    }
                    "enter" => {
                        if let Some(key) = this.highlight.chosen(&step_keys) {
                            this.run(&key, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(tabs)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(SearchField::new(&self.input))
                    .child(
                        div()
                            .id("channel-tab-choices")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .mt(rems(1.))
                            .gap(rems(0.125))
                            .overflow_y_scroll()
                            .track_scroll(self.highlight.scroll_handle())
                            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                                this.highlight.track_pointer(event.position)
                            }))
                            .children(rows)
                            .children(empty),
                    ),
            )
    }
}
