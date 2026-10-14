//! The channel's secondary tabs (`useChannelTabState.ts`,
//! `useConversationTabs.tsx`): the thread, Channel settings, other
//! conversations and threads, and channel tools, all in one pane beside
//! the conversation. Tabs open from content; there is no blank tab.
//!
//! The state is plain data kept per community and channel, so a channel
//! reopens with the tabs it had; the views are made again on the visit.

use nostr::prelude::EventId;

pub const THREAD: &str = "thread";
pub const SETTINGS: &str = "settings";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabKind {
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

    /// Closes a conversation tab or thread tab.
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

    pub fn open_settings(&mut self) {
        self.settings = true;
        self.select(SETTINGS);
    }

    /// The header toggle: hides the pane, or shows it again with its
    /// tabs as they were; `side` is a launcher's panel in the pane. With
    /// nothing to show the toggle is disabled.
    pub fn toggle_pane(&mut self, side: bool) {
        self.pane_open = !(self.pane_open && (self.has_panel() || side));
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

    fn conversation(id: &str) -> ConversationTab {
        ConversationTab {
            id: format!("conversation:{id}"),
            kind: TabKind::Conversation { channel: id.into() },
        }
    }

    #[test]
    fn settings_and_the_thread_lead_the_tabs() {
        let mut state = TabState::default();
        assert!(!state.has_panel());
        state.tabs.push(conversation("a"));
        state.open_thread(thread());
        state.open_settings();
        state.open_panel(PanelTab::tool("buzz.todos/todos", "c"));
        assert_eq!(
            state.root_ids(),
            ["settings", "thread", "conversation:a", "buzz.todos/todos:c"]
        );
        assert_eq!(state.selected_tab().as_deref(), Some("buzz.todos/todos:c"));
    }

    #[test]
    fn closing_the_shown_tab_shows_its_neighbour() {
        let mut state = TabState::default();
        for id in ["1", "2", "3"] {
            state.tabs.push(conversation(id));
        }
        state.select("conversation:2");
        state.close_tab("conversation:2");
        assert_eq!(state.selected_tab().as_deref(), Some("conversation:3"));
        state.close_tab("conversation:3");
        assert_eq!(state.selected_tab().as_deref(), Some("conversation:1"));
        // Closing a tab that isn't shown keeps the selection.
        state.tabs.push(conversation("4"));
        state.select("conversation:4");
        state.close_tab("conversation:1");
        assert_eq!(state.selected_tab().as_deref(), Some("conversation:4"));
        state.close_tab("conversation:4");
        assert_eq!(state.selected_tab(), None);
        assert!(!state.showing());
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
    fn the_toggle_hides_and_shows_the_tabs_it_has() {
        let mut state = TabState::default();
        // Nothing to show: no blank tab appears.
        state.toggle_pane(false);
        assert!(!state.showing());
        assert!(state.root_ids().is_empty());
        state.pane_open = true;
        state.open_thread(thread());
        state.toggle_pane(false);
        assert!(!state.showing());
        assert_eq!(state.root_ids(), ["thread"]);
        state.toggle_pane(false);
        assert!(state.showing());
        // A launcher's side panel alone is something to hide.
        state.close_thread();
        state.toggle_pane(true);
        assert!(!state.pane_open);
        state.toggle_pane(true);
        assert!(state.pane_open);
    }
}
