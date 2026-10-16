//! An Agents2 agent's sections as tabs, wherever they are shown (its Build
//! view and its profile panel): the type's own tabs, then the app's
//! Attention and Memory tabs, which every type shares: the app is what
//! wakes the agent, and every type keeps its memory the same way
//! (buzz-app's `tabs.tsx` and `AgentTabs.tsx`).

use super::AttentionPanel;
use super::memory::MemoryPanel;
use crate::agents2::{AgentConversation, AgentView, Agents2, type_of};
use buzz_ui::{Tab, TabVariant, Tabs};
use gpui_kit::*;
use std::collections::HashMap;

const ATTENTION: &str = "agent:attention";
const MEMORY: &str = "agent:memory";

pub struct AgentTabs {
    pubkey: String,
    /// The chosen section; the first when unset.
    selected: Option<SharedString>,
    attention: Entity<AttentionPanel>,
    /// Built when first shown, so a closed tab reads no memory.
    memory: Option<Entity<MemoryPanel>>,
    /// Type tab views, built when first shown.
    views: HashMap<SharedString, AnyView>,
    /// The conversation the agent was opened from, which type tabs set
    /// apart.
    conversation: Option<AgentConversation>,
    _observe: Option<Subscription>,
}

impl AgentTabs {
    pub fn new(pubkey: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let attention = cx.new(|cx| AttentionPanel::new(pubkey.clone(), window, cx));
        let observe =
            Agents2::global(cx).map(|service| cx.observe(&service, |_, _, cx| cx.notify()));
        Self {
            pubkey,
            selected: None,
            attention,
            memory: None,
            views: HashMap::new(),
            conversation: None,
            _observe: observe,
        }
    }

    /// Opened from another conversation, the type tabs start over with it,
    /// closing a transcript they showed.
    pub fn set_conversation(
        &mut self,
        conversation: Option<AgentConversation>,
        cx: &mut Context<Self>,
    ) {
        if conversation != self.conversation {
            self.conversation = conversation;
            self.views.clear();
            cx.notify();
        }
    }
}

impl Render for AgentTabs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(agent) =
            Agents2::global(cx).and_then(|service| service.read(cx).find(&self.pubkey).cloned())
        else {
            return div();
        };
        let kind = type_of(&agent.agent_type, cx);
        let type_tabs = kind
            .as_ref()
            .map(|kind| kind.kind.tabs())
            .unwrap_or_default();
        let mut sections: Vec<(SharedString, SharedString)> = type_tabs
            .iter()
            .map(|tab| (format!("agent:type:{}", tab.id).into(), tab.title.clone()))
            .collect();
        sections.push((ATTENTION.into(), "Attention".into()));
        sections.push((MEMORY.into(), "Memory".into()));
        // A tab that went away (its type was replaced) falls back to the last.
        let selected = self
            .selected
            .clone()
            .filter(|value| sections.iter().any(|(v, _)| v == value))
            .unwrap_or_else(|| {
                if self.selected.is_some() {
                    sections.last().expect("Memory is always there").0.clone()
                } else {
                    sections[0].0.clone()
                }
            });
        let mut tabs = Tabs::new(TabVariant::Panel);
        for (value, label) in &sections {
            let value = value.clone();
            tabs = tabs.tab(
                Tab::new(
                    SharedString::from(format!("{}-{value}", self.pubkey)),
                    label.clone(),
                )
                .selected(value == selected)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = Some(value.clone());
                    cx.notify();
                })),
            );
        }
        let panel: AnyElement = if selected.as_ref() == ATTENTION {
            self.attention.clone().into_any_element()
        } else if selected.as_ref() == MEMORY {
            let pubkey = self.pubkey.clone();
            self.memory
                .get_or_insert_with(|| cx.new(|cx| MemoryPanel::new(pubkey, cx)))
                .clone()
                .into_any_element()
        } else {
            let tab = type_tabs
                .iter()
                .find(|tab| format!("agent:type:{}", tab.id) == selected.as_ref());
            match tab {
                Some(tab) => {
                    let view = match self.views.get(&selected) {
                        Some(view) => view.clone(),
                        None => {
                            let view = (tab.build)(
                                AgentView {
                                    agent: agent.clone(),
                                    conversation: self.conversation.clone(),
                                },
                                window,
                                cx,
                            );
                            self.views.insert(selected.clone(), view.clone());
                            view
                        }
                    };
                    view.into_any_element()
                }
                None => div().into_any_element(),
            }
        };
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(tabs)
            .child(div().pt(rems(1.)).child(panel))
    }
}
