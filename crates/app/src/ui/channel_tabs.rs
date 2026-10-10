//! The channel's secondary tabs (`useChannelTabState.ts`,
//! `useConversationTabs.tsx`): the thread, Channel settings, other
//! conversations and threads, channel tools, and New tab pickers, all in
//! one pane beside the conversation.
//!
//! The state is plain data kept per community and channel, so a channel
//! reopens with the tabs it had; the views are made again on the visit.

use nostr::prelude::EventId;

pub const THREAD: &str = "thread";
pub const SETTINGS: &str = "settings";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabKind {
    /// A New tab picker.
    New,
    /// Another conversation, readable and writable here.
    Conversation { channel: String },
    /// A thread in another conversation.
    Thread { channel: String, message: EventId },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationTab {
    pub id: String,
    pub kind: TabKind,
}

/// The thread beside the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadTarget {
    pub root: EventId,
    /// `root` is any message, revealed in its thread.
    pub exact: bool,
}

/// A channel tool, channel-menu panel or linked panel open as a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelTab {
    /// The registered panel's key.
    pub panel: &'static str,
    /// The conversation it was opened from.
    pub channel: String,
    /// What it shows: the channel id for tools, else the link.
    pub target: String,
}

impl PanelTab {
    /// For a channel tool, which shows its channel.
    pub fn tool(panel: &'static str, channel: &str) -> Self {
        Self {
            panel,
            channel: channel.to_string(),
            target: channel.to_string(),
        }
    }

    pub fn id(&self) -> String {
        format!("{}:{}", self.panel, self.target)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabState {
    pub tabs: Vec<ConversationTab>,
    pub panels: Vec<PanelTab>,
    pub selected: String,
    /// The pane can be hidden with its tabs kept.
    pub pane_open: bool,
    pub thread: Option<ThreadTarget>,
    pub settings: bool,
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            panels: Vec::new(),
            selected: THREAD.into(),
            pane_open: true,
            thread: None,
            settings: false,
        }
    }
}

impl TabState {
    /// The tabs in order: Channel settings and the thread lead, then the
    /// conversation tabs, then the panels.
    pub fn root_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if self.settings {
            ids.push(SETTINGS.to_string());
        }
        if self.thread.is_some() {
            ids.push(THREAD.to_string());
        }
        ids.extend(self.tabs.iter().map(|tab| tab.id.clone()));
        ids.extend(self.panels.iter().map(PanelTab::id));
        ids
    }

    /// The tab shown: the selected one while it exists, else the first.
    pub fn selected_tab(&self) -> Option<String> {
        let ids = self.root_ids();
        if ids.contains(&self.selected) {
            Some(self.selected.clone())
        } else {
            ids.into_iter().next()
        }
    }

    pub fn has_panel(&self) -> bool {
        self.settings || self.thread.is_some() || !self.tabs.is_empty() || !self.panels.is_empty()
    }

    /// The pane is on screen.
    pub fn showing(&self) -> bool {
        self.pane_open && self.has_panel()
    }

    pub fn select(&mut self, id: &str) {
        self.selected = id.to_string();
        if !id.is_empty() {
            self.pane_open = true;
        }
    }

    /// After `id` closes: when it was the shown tab, the tab now at its
    /// place is shown, or the one before it. `ids` are the tabs before
    /// the close.
    fn after_close(&mut self, id: &str, ids: &[String], shown: Option<&str>) {
        if shown != Some(id) {
            return;
        }
        let index = ids.iter().position(|tab| tab == id).unwrap_or(0);
        let remaining: Vec<&String> = ids.iter().filter(|tab| *tab != id).collect();
        let next = remaining
            .get(index.min(remaining.len().saturating_sub(1)))
            .map(|tab| tab.as_str())
            .unwrap_or_default();
        self.selected = next.to_string();
    }

    /// Closes a conversation tab, picker or thread tab.
    pub fn close_tab(&mut self, id: &str) {
        let ids = self.root_ids();
        let shown = self.selected_tab();
        self.tabs.retain(|tab| tab.id != id);
        self.after_close(id, &ids, shown.as_deref());
    }

    pub fn close_thread(&mut self) {
        let ids = self.root_ids();
        let shown = self.selected_tab();
        self.thread = None;
        self.after_close(THREAD, &ids, shown.as_deref());
    }

    pub fn close_settings(&mut self) {
        let ids = self.root_ids();
        let shown = self.selected_tab();
        self.settings = false;
        self.after_close(SETTINGS, &ids, shown.as_deref());
    }

    pub fn close_panel(&mut self, id: &str) {
        let ids = self.root_ids();
        let shown = self.selected_tab();
        self.panels.retain(|panel| panel.id() != id);
        self.after_close(id, &ids, shown.as_deref());
    }

    /// Adds a New tab picker and shows it.
    pub fn add_new(&mut self, id: String) {
        self.tabs.push(ConversationTab {
            id: id.clone(),
            kind: TabKind::New,
        });
        self.select(&id);
    }

    /// The picker `picker` chose `channel`: an open tab for it is shown
    /// and the picker closes; otherwise the picker becomes that tab.
    pub fn choose_conversation(&mut self, picker: &str, channel: &str) {
        let target = format!("conversation:{channel}");
        let existing = self.tabs.iter().any(|tab| {
            tab.kind
                == TabKind::Conversation {
                    channel: channel.to_string(),
                }
        });
        if existing {
            self.tabs.retain(|tab| tab.id != picker);
        } else if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == picker) {
            *tab = ConversationTab {
                id: target.clone(),
                kind: TabKind::Conversation {
                    channel: channel.to_string(),
                },
            };
        }
        self.select(&target);
    }

    /// The picker `picker` chose a channel tool: the picker closes and the
    /// tool opens as a panel tab, or is shown if already open.
    pub fn choose_tool(&mut self, picker: &str, panel: PanelTab) {
        self.tabs.retain(|tab| tab.id != picker);
        self.open_panel(panel);
    }

    pub fn open_panel(&mut self, panel: PanelTab) {
        let id = panel.id();
        if !self.panels.contains(&panel) {
            self.panels.push(panel);
        }
        self.select(&id);
    }

    /// A thread in a conversation tab opens as its own tab.
    pub fn open_conversation_thread(&mut self, channel: &str, message: EventId, root: EventId) {
        let id = format!("thread:{channel}:{}", root.to_hex());
        let tab = ConversationTab {
            id: id.clone(),
            kind: TabKind::Thread {
                channel: channel.to_string(),
                message,
            },
        };
        match self.tabs.iter_mut().find(|existing| existing.id == id) {
            Some(existing) => *existing = tab,
            None => self.tabs.push(tab),
        }
        self.select(&id);
    }

    /// Opens or replaces the thread beside the conversation. Timeline
    /// actions retire channel-menu panels' transient details, but tools
    /// and conversation tabs stay.
    pub fn open_thread(&mut self, thread: ThreadTarget) {
        self.thread = Some(thread);
        self.select(THREAD);
    }

    /// The header toggle: hides the pane, or shows it, adding a New tab
    /// when there is nothing to show.
    pub fn toggle_pane(&mut self, new_id: impl FnOnce() -> String) {
        if self.showing() {
            self.pane_open = false;
        } else {
            self.pane_open = true;
            if self.root_ids().is_empty() {
                self.add_new(new_id());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(byte: u8) -> EventId {
        EventId::from_byte_array([byte; 32])
    }

    fn thread() -> ThreadTarget {
        ThreadTarget {
            root: event(1),
            exact: false,
        }
    }

    #[test]
    fn settings_and_the_thread_lead_the_tabs() {
        let mut state = TabState::default();
        assert!(!state.has_panel());
        state.add_new("new:1".into());
        state.open_thread(thread());
        state.settings = true;
        state.open_panel(PanelTab::tool("buzz.todos/todos", "c"));
        assert_eq!(
            state.root_ids(),
            ["settings", "thread", "new:1", "buzz.todos/todos:c"]
        );
        assert_eq!(state.selected_tab().as_deref(), Some("buzz.todos/todos:c"));
    }

    #[test]
    fn closing_the_shown_tab_shows_its_neighbour() {
        let mut state = TabState::default();
        state.add_new("new:1".into());
        state.add_new("new:2".into());
        state.add_new("new:3".into());
        state.select("new:2");
        state.close_tab("new:2");
        assert_eq!(state.selected_tab().as_deref(), Some("new:3"));
        state.close_tab("new:3");
        assert_eq!(state.selected_tab().as_deref(), Some("new:1"));
        // Closing a tab that isn't shown keeps the selection.
        state.add_new("new:4".into());
        state.close_tab("new:1");
        assert_eq!(state.selected_tab().as_deref(), Some("new:4"));
        state.close_tab("new:4");
        assert_eq!(state.selected_tab(), None);
        assert!(!state.showing());
    }

    #[test]
    fn a_picker_becomes_the_conversation_or_shows_the_open_one() {
        let mut state = TabState::default();
        state.add_new("new:1".into());
        state.choose_conversation("new:1", "random");
        assert_eq!(
            state.tabs,
            [ConversationTab {
                id: "conversation:random".into(),
                kind: TabKind::Conversation {
                    channel: "random".into()
                },
            }]
        );
        state.add_new("new:2".into());
        state.choose_conversation("new:2", "random");
        assert_eq!(state.tabs.len(), 1);
        assert_eq!(state.selected_tab().as_deref(), Some("conversation:random"));
    }

    #[test]
    fn conversation_threads_get_one_tab_per_root() {
        let mut state = TabState::default();
        state.open_conversation_thread("random", event(2), event(1));
        state.open_conversation_thread("random", event(3), event(1));
        assert_eq!(state.tabs.len(), 1);
        assert_eq!(
            state.tabs[0].kind,
            TabKind::Thread {
                channel: "random".into(),
                message: event(3)
            }
        );
    }

    #[test]
    fn the_toggle_hides_or_adds_a_picker() {
        let mut state = TabState::default();
        state.toggle_pane(|| "new:1".into());
        assert!(state.showing());
        assert_eq!(state.selected_tab().as_deref(), Some("new:1"));
        state.toggle_pane(|| unreachable!());
        assert!(!state.showing());
        assert_eq!(state.tabs.len(), 1);
        state.toggle_pane(|| unreachable!());
        assert!(state.showing());
    }

    #[test]
    fn a_tool_replaces_its_picker() {
        let mut state = TabState::default();
        state.add_new("new:1".into());
        let tool = PanelTab::tool("buzz.terminal/terminal", "c");
        state.choose_tool("new:1", tool.clone());
        state.add_new("new:2".into());
        state.choose_tool("new:2", tool);
        assert!(state.tabs.is_empty());
        assert_eq!(state.panels.len(), 1);
        assert_eq!(
            state.selected_tab().as_deref(),
            Some("buzz.terminal/terminal:c")
        );
    }
}
