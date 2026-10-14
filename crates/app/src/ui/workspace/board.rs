//! The Messages board (`ChannelsPage.tsx`): the channel's main pane, its
//! conversation followed by threads moved or opened there, and beside it
//! the side pane holding threads, other conversations and their threads,
//! Channel settings and channel tools, with the companion panel stacked
//! under it.

use super::Workspace;
use super::layout::Split;
use crate::community::Community;
use crate::ui::channel::{ChannelPage, Launcher, MainTab, MainTabs, PaneButton, PaneToggle};
use crate::ui::channel_menu::MenuPanel;
use crate::ui::channel_panels::{self, ChannelPanel};
use crate::ui::channel_settings::{self as settings_panel, ChannelRemoved, ChannelSettingsPanel};
use crate::ui::channel_tab_views::{
    self, TabHost, TabView, root_content, tab_icon, thread_channel, thread_label, usable,
};
use crate::ui::channel_tabs::{self, PanelTab, Routed, TabKind, TabPane, TabState};
use crate::ui::pages::Page;
use crate::ui::panels::{self, Close, Dock, TabItem};
use crate::ui::tab_drag::{self, PaneOrder, Rules, TabDrag, TabGeometry};
use crate::ui::thread::ThreadPanel;
use buzz_relay_client::{ChannelSummary, ChannelType};
use buzz_ui::{ActiveTheme, IconName, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nostr::prelude::EventId;
use std::collections::HashMap;
use std::rc::Rc;

/// A channel panel opened from its header launcher.
pub(super) struct Drawer {
    key: &'static str,
    placement: channel_panels::Placement,
    view: AnyView,
}

impl TabHost for Workspace {
    fn community(&self) -> Option<Entity<Community>> {
        self.community.clone()
    }

    fn channel_id(&self, cx: &App) -> Option<String> {
        self.tab_key(cx).map(|(_, channel)| channel)
    }

    fn page(&self) -> Option<Entity<ChannelPage>> {
        self.channel.clone()
    }

    fn tab_views(&mut self) -> &mut HashMap<String, TabView> {
        &mut self.tab_views
    }

    fn tabs(&self, cx: &App) -> TabState {
        Workspace::tabs(self, cx)
    }

    fn update_tabs(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TabState)) {
        Workspace::update_tabs(self, cx, f);
    }

    fn close_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        Workspace::close_tab(self, id, window, cx)
    }

    fn focus_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        Workspace::focus_tab(self, id, window, cx)
    }

    fn thread_pane(&self, cx: &App) -> TabPane {
        Workspace::thread_pane(cx)
    }

    fn panel_view(
        &self,
        panel: &ChannelPanel,
        channel: String,
        target: String,
        close: channel_panels::Close,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyView> {
        Workspace::panel_view(self, panel, channel, target, close, window, cx)
    }
}

impl Workspace {
    /// The current channel's key in [`Workspace::tab_states`].
    fn tab_key(&self, cx: &App) -> Option<(EntityId, String)> {
        let community = self.community.as_ref()?;
        let channel = self.channel.as_ref()?.read(cx).channel_id().to_string();
        Some((community.entity_id(), channel))
    }

    pub(super) fn tabs(&self, cx: &App) -> TabState {
        self.tab_key(cx)
            .and_then(|key| self.tab_states.get(&key).cloned())
            .unwrap_or_default()
    }

    /// Changes the current channel's tabs, then drops the views of tabs
    /// that closed.
    pub(super) fn update_tabs<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut TabState) -> R,
    ) -> Option<R> {
        let key = self.tab_key(cx)?;
        let state = self.tab_states.entry(key).or_default();
        let was_showing = state.showing();
        let result = f(state);
        if !was_showing && state.showing() {
            self.pane_opening += 1;
        }
        let ids = state.all_ids();
        let closed: Vec<EntityId> = self
            .tab_views
            .iter()
            .filter(|(id, _)| !ids.contains(id))
            .filter_map(|(_, view)| view.thread().map(Entity::entity_id))
            .collect();
        // Media hook: a clip started in a closed thread stops with it.
        for thread in closed {
            super::super::media::pause_audio(Some(thread), cx);
        }
        self.tab_views.retain(|id, _| ids.contains(id));
        cx.notify();
        Some(result)
    }

    /// The tabs of the channel being left stay saved; their views go, as
    /// does any launcher panel, which belongs to the visit.
    pub(super) fn leave_channel_tabs(&mut self) {
        self.tab_views.clear();
        self.drawer = None;
        self.routed = None;
        self.place = (None, None);
    }

    /// Where a new thread opens: this device's choice in Settings →
    /// Messages. Already-open threads stay where they were placed.
    pub(super) fn thread_pane(cx: &App) -> TabPane {
        crate::ui::settings::messages::thread_placement(cx)
    }

    /// A header launcher: shows the panel's tab when one is open, else
    /// toggles the panel's drawer or side panel (`useChannelPanels`).
    fn toggle_launcher(
        &mut self,
        panel: ChannelPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(channel) = self.tab_key(cx).map(|(_, channel)| channel) else {
            return;
        };
        let tab = PanelTab::tool(panel.key, &channel);
        let state = self.tabs(cx);
        if state.panels.contains(&tab) {
            if state.showing() && state.selected_tab() == Some(tab.id()) {
                self.update_tabs(cx, |state| state.pane_open = false);
            } else {
                self.select_tab(&tab.id(), window, cx);
            }
            return;
        }
        if self
            .drawer
            .as_ref()
            .is_some_and(|drawer| drawer.key == panel.key)
        {
            self.drawer = None;
            self.focus_composer(window, cx);
            cx.notify();
            return;
        }
        let Some(launcher) = panel.launcher else {
            return;
        };
        let weak = cx.entity().downgrade();
        let key = panel.key;
        let close: channel_panels::Close = Rc::new(move |window, cx| {
            let _ = weak.update(cx, |this, cx| {
                if this.drawer.as_ref().is_some_and(|drawer| drawer.key == key) {
                    this.drawer = None;
                    this.focus_composer(window, cx);
                    cx.notify();
                }
            });
        });
        let Some(view) = self.panel_view(&panel, channel.clone(), channel, close, window, cx)
        else {
            return;
        };
        self.update_tabs(cx, |state| state.pane_open = true);
        self.drawer = Some(Drawer {
            key,
            placement: launcher.placement,
            view,
        });
        cx.notify();
    }

    /// A panel's shortcut (Mod+J for the terminal): toggles it as its
    /// header launcher does, while a writable channel shows.
    pub(crate) fn toggle_channel_panel(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let readable = self
            .current_summary(cx)
            .is_some_and(|channel| !channel.read_only);
        if self.page != Page::MESSAGES || !readable {
            return;
        }
        if let Some(panel) = channel_panels::get(key, cx) {
            self.toggle_launcher(panel, window, cx);
        }
    }

    /// The channel menu's panel entries; each opens its panel as a tab.
    fn menu_panels(&self, cx: &mut Context<Self>) -> Vec<MenuPanel> {
        let Some(channel) = self.current_summary(cx) else {
            return Vec::new();
        };
        channel_panels::menu_entries(&channel, cx)
            .into_iter()
            .map(|(panel, label)| {
                let weak = cx.entity().downgrade();
                let tab = PanelTab::tool(panel.key, &channel.id);
                let open: Close = Rc::new(move |window, cx| {
                    let tab = tab.clone();
                    let _ = weak.update(cx, |this, cx| {
                        let id = tab.id();
                        this.drawer = None;
                        this.update_tabs(cx, |state| state.open_panel(tab));
                        this.focus_tab(&id, window, cx);
                    });
                });
                (SharedString::from(label), open)
            })
            .collect()
    }

    /// The header launchers for the open channel.
    fn launchers(&self, cx: &mut Context<Self>) -> Vec<Launcher> {
        let readable = self
            .current_summary(cx)
            .is_some_and(|channel| !channel.read_only);
        if !readable {
            return Vec::new();
        }
        channel_panels::launchers(cx)
            .into_iter()
            .map(|(panel, launcher)| {
                let weak = cx.entity().downgrade();
                let key = panel.key;
                let label = match crate::shortcuts::panel_chord(key, cx) {
                    Some(chord) => format!("{} ({chord})", panel.title).into(),
                    None => launcher.label.into(),
                };
                Launcher {
                    key,
                    icon: launcher.icon,
                    label,
                    pressed: self.drawer.as_ref().is_some_and(|drawer| drawer.key == key),
                    toggle: Rc::new(move |window, cx| {
                        let panel = panel.clone();
                        let _ = weak.update(cx, |this, cx| this.toggle_launcher(panel, window, cx));
                    }),
                }
            })
            .collect()
    }

    /// The header toggle (`Toggle tab pane`): hides or shows the pane
    /// with what it holds. With nothing to show it is disabled.
    pub(super) fn toggle_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.side_drawer();
        let showing = self
            .update_tabs(cx, |state| {
                state.toggle_pane(side);
                state.showing()
            })
            .unwrap_or_default();
        match self.tabs(cx).selected_tab() {
            Some(id) if showing => self.focus_tab(&id, window, cx),
            _ => self.focus_composer(window, cx),
        }
    }

    pub(super) fn select_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.update_tabs(cx, |state| state.select(id));
        self.focus_tab(id, window, cx);
    }

    /// Shows a main-pane tab: a thread, or with `None` the conversation.
    fn select_main_tab(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.update_tabs(cx, |state| state.select_main(id.clone()));
        match id {
            Some(id) => self.focus_tab(&id, window, cx),
            None => self.focus_composer(window, cx),
        }
    }

    /// Closes the shown tab of the pane that owns Close (Mod+W): the main
    /// pane while focus is in it, else the side pane. False when that
    /// pane shows nothing it can close; the conversation never closes.
    pub(super) fn close_selected_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.tabs(cx);
        if self.main_area.contains_focused(window, cx) {
            return match state.selected_main() {
                Some(id) => {
                    self.close_tab(&id, window, cx);
                    true
                }
                None => false,
            };
        }
        if !state.showing() {
            return false;
        }
        match state.selected_tab() {
            Some(id) => {
                self.close_tab(&id, window, cx);
                true
            }
            None => false,
        }
    }

    /// Mod+W: the shown channel tab closes; with none, the window does.
    /// Not while a dialog is open: the workspace's, a Settings dialog, or
    /// any dialog or media viewer holding focus.
    pub fn close_tab_or_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings_modal = self.page == Page::SETTINGS
            && self
                .settings
                .as_ref()
                .is_some_and(|settings| settings.read(cx).has_modal());
        let focused_dialog = window.context_stack().iter().any(|context| {
            context.contains(buzz_ui::DIALOG_CONTEXT) || context.contains("MediaReview")
        });
        if self.dialog.is_some() || settings_modal || focused_dialog {
            return;
        }
        let on_board = self.page == Page::MESSAGES && self.channel.is_some();
        if !(on_board && self.close_selected_tab(window, cx)) {
            crate::main_window::close(window, cx);
        }
    }

    fn close_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        let state = self.tabs(cx);
        let closing = state.tab(&id).and_then(|tab| tab.thread().cloned());
        let shown = state.is_shown(&id);
        let loaded_root = self
            .tab_views
            .get(&id)
            .and_then(TabView::thread)
            .and_then(|panel| panel.read(cx).loaded_root());
        let remaining = self
            .update_tabs(cx, |state| {
                if id == channel_tabs::SETTINGS {
                    state.close_settings();
                } else if state.panels.iter().any(|panel| panel.id() == id) {
                    state.close_panel(&id);
                } else {
                    state.close_tab(&id);
                }
                state.selected_tab()
            })
            .flatten();
        let Some(thread) = closing else {
            match remaining {
                Some(next) => self.focus_tab(&next, window, cx),
                None => self.focus_composer(window, cx),
            }
            return;
        };
        // Closing the thread the route addresses returns the route to its
        // channel, so only Back or Forward reopens it.
        let addressed = self.routed.as_ref().is_some_and(|routed| {
            routed.channel == thread.channel && routed.message == thread.message
        });
        if addressed {
            self.visit_channel(cx);
        }
        // Focus goes back to the conversation that opened it, and a
        // thread closed in view shows where its root is.
        self.focus_composer(window, cx);
        let root = loaded_root.or(thread.root);
        if shown
            && self.channel_id(cx).as_deref() == Some(thread.channel.as_str())
            && let (Some(page), Some(root)) = (self.channel.clone(), root)
        {
            page.update(cx, |page, cx| page.highlight_return(root, cx));
        }
    }

    /// The open channel again, as a new visit.
    fn visit_channel(&mut self, cx: &mut Context<Self>) {
        self.routed = None;
        self.place = (None, None);
        self.record(cx);
    }

    fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        if let Some(channel) = &self.channel {
            let composer = channel.read(cx).composer();
            composer.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    /// Focus follows the shown tab.
    fn focus_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tab_views(window, cx);
        match self.tab_views.get(id) {
            Some(TabView::Settings { panel, .. }) => {
                let handle = panel.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            Some(TabView::Conversation(page)) => {
                let composer = page.read(cx).composer();
                composer.update(cx, |state, cx| state.focus(window, cx));
            }
            Some(TabView::Thread { panel, .. }) => ThreadPanel::focus(panel, window, cx),
            _ => {}
        }
    }

    /// Opens a thread of the channel from its timeline: an open tab is
    /// selected wherever it was placed, else a new tab opens in the
    /// preferred pane. Every opening keeps the work already open, and the
    /// visit it leaves is kept for Back. Reply puts focus in the thread's
    /// composer.
    pub(super) fn open_thread(
        &mut self,
        message: EventId,
        root: EventId,
        reply: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(channel) = self.channel_id(cx) else {
            return;
        };
        let pane = Self::thread_pane(cx);
        let Some(id) = self.update_tabs(cx, |state| {
            state.settings = false;
            state.leave_visit();
            // Transient details give way; selecting the thread is the
            // last word.
            state.retire_details();
            state.open_thread_tab(&channel, message, root, pane)
        }) else {
            return;
        };
        self.ensure_tab_views(window, cx);
        match self.tab_views.get(&id).and_then(TabView::thread).cloned() {
            Some(panel) if reply => ThreadPanel::focus_composer(&panel, window, cx),
            Some(panel) => ThreadPanel::focus(&panel, window, cx),
            None => {}
        }
        self.routed = Some(Routed {
            channel,
            message,
            root,
        });
        self.place = (Some(message), Some(root));
        self.record(cx);
    }

    /// Applies the current history entry to the channel's tabs once: a
    /// thread visit selects its thread, another restores the tabs it last
    /// had.
    pub(super) fn enter_visit(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::MESSAGES || self.channel.is_none() {
            return;
        }
        let Some(entry) = self.history.current_id() else {
            return;
        };
        let routed = self.routed.clone();
        let kind = if routed.is_some() {
            "thread"
        } else {
            "channel"
        };
        let visit = format!("{entry}:{kind}");
        let pane = Self::thread_pane(cx);
        self.update_tabs(cx, |state| state.enter_visit(&visit, routed.as_ref(), pane));
    }

    /// The tab of the thread the current visit addresses.
    pub(super) fn shown_thread_tab(&self, cx: &App) -> Option<String> {
        let routed = self.routed.as_ref()?;
        self.tabs(cx)
            .tabs
            .iter()
            .find(|tab| {
                tab.thread().is_some_and(|thread| {
                    thread.channel == routed.channel && thread.message == routed.message
                })
            })
            .map(|tab| tab.id.clone())
    }

    /// Focus moves to a routed thread as it opens.
    pub(super) fn focus_routed(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_tab(id, window, cx);
    }

    /// The thread on screen in either pane, with the conversation it is
    /// in.
    pub(super) fn shown_thread(&self, cx: &App) -> Option<(String, EventId)> {
        let state = self.tabs(cx);
        let id = state
            .selected_main()
            .or_else(|| state.showing().then(|| state.selected_tab()).flatten())?;
        let thread = state.tab(&id)?.thread()?;
        let root = self
            .tab_views
            .get(&id)
            .and_then(TabView::thread)
            .and_then(|panel| panel.read(cx).loaded_root())
            .unwrap_or(thread.root_id());
        Some((thread.channel.clone(), root))
    }

    /// A launcher's panel in the pane, which the toggle hides with it.
    fn side_drawer(&self) -> bool {
        self.drawer
            .as_ref()
            .is_some_and(|drawer| drawer.placement == channel_panels::Placement::Side)
    }

    /// The current channel's summary.
    fn current_summary(&self, cx: &App) -> Option<ChannelSummary> {
        Some(self.channel.as_ref()?.read(cx).channel().clone())
    }

    /// Makes a registered panel's view for `opening`, closed through
    /// `close`.
    fn panel_view(
        &self,
        panel: &ChannelPanel,
        channel: String,
        target: String,
        close: channel_panels::Close,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyView> {
        let community = self.community.clone()?;
        let thread = self.routed.as_ref().map(|routed| routed.message.to_hex());
        Some((panel.build)(
            channel_panels::Opening {
                community,
                channel,
                target,
                close,
                thread,
            },
            window,
            cx,
        ))
    }

    /// Makes the views of tabs that have none yet.
    fn ensure_tab_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(community), Some(current)) = (self.community.clone(), self.channel.clone())
        else {
            return;
        };
        let current_id = current.read(cx).channel_id().to_string();
        let state = self.tabs(cx);
        if state.settings && !self.tab_views.contains_key(channel_tabs::SETTINGS) {
            let weak = cx.entity().downgrade();
            let on_close: settings_panel::Callback = Rc::new(move |window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.close_tab(channel_tabs::SETTINGS, window, cx)
                });
            });
            let actions = current.read(cx).header_actions();
            let open_members: settings_panel::Callback = Rc::new(move |window, cx| {
                actions.update(cx, |actions, cx| actions.open_members(window, cx));
            });
            let summary = current.read(cx).channel().clone();
            let community = community.clone();
            let panel = cx.new(|cx| {
                ChannelSettingsPanel::new(
                    community,
                    current_id.clone(),
                    on_close,
                    open_members,
                    window,
                    cx,
                )
                .with_channel(summary, window, cx)
            });
            // Leaving or deleting from the panel closes the page, as the
            // header menu does.
            let subscription =
                cx.subscribe_in(&panel, window, |this, _, _: &ChannelRemoved, window, cx| {
                    this.close_channel(window, cx)
                });
            self.tab_views.insert(
                channel_tabs::SETTINGS.to_string(),
                TabView::Settings {
                    panel,
                    _subscription: subscription,
                },
            );
        }
        channel_tab_views::ensure_views(self, window, cx);
    }

    /// A conversation or thread tab's label and icon.
    fn tab_identity(
        &self,
        tab: &channel_tabs::ConversationTab,
        cx: &App,
    ) -> (SharedString, IconName) {
        let Some(community) = &self.community else {
            return ("Unavailable conversation".into(), IconName::Browser);
        };
        match &tab.kind {
            TabKind::Conversation { channel } => match usable(community, channel, cx) {
                Some(summary) => (summary.name.clone().into(), tab_icon(&summary)),
                None => ("Unavailable conversation".into(), IconName::Browser),
            },
            TabKind::Thread(thread) => {
                let name = thread_channel(self, thread, cx).map(|summary| summary.name);
                let content = root_content(self, thread, cx);
                (
                    thread_label(content.as_deref(), name.as_deref()).into(),
                    IconName::ChatCircle,
                )
            }
        }
    }

    /// "This conversation is no longer available."
    fn unavailable(cx: &App) -> AnyElement {
        div()
            .px(rems(1.))
            .py(rems(2.))
            .text_body_sm()
            .text_color(cx.colors().text_subtle)
            .child("This conversation is no longer available.")
            .into_any_element()
    }

    /// The side pane's tabs as its tab strip shows them.
    fn tab_items(
        &mut self,
        state: &TabState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TabItem> {
        self.ensure_tab_views(window, cx);
        let weak = cx.entity().downgrade();
        let closer = |id: String| -> Close {
            let weak = weak.clone();
            Rc::new(move |window, cx| {
                let id = id.clone();
                let _ = weak.update(cx, |this, cx| this.close_tab(&id, window, cx));
            })
        };
        let mut items = Vec::new();
        if state.settings
            && let Some(TabView::Settings { panel, .. }) =
                self.tab_views.get(channel_tabs::SETTINGS)
        {
            items.push(TabItem {
                id: channel_tabs::SETTINGS.into(),
                label: "Channel settings".into(),
                icon: IconName::Gear,
                content: panel.clone().into_any_element(),
                close: closer(channel_tabs::SETTINGS.to_string()),
                drag: None,
            });
        }
        for tab in state.side_tabs() {
            let (label, icon) = self.tab_identity(tab, cx);
            let content = match self.tab_views.get(&tab.id) {
                Some(TabView::Conversation(page)) => page.clone().into_any_element(),
                Some(TabView::Thread { panel, .. }) => panel.clone().into_any_element(),
                Some(TabView::Panel(view)) => view.clone().into_any_element(),
                Some(TabView::Settings { .. }) | None => Self::unavailable(cx),
            };
            let drag = tab.thread().map(|_| TabDrag {
                id: tab.id.clone(),
                label: label.clone(),
                pane: TabPane::Side,
            });
            items.push(TabItem {
                id: tab.id.clone().into(),
                label,
                icon,
                content,
                close: closer(tab.id.clone()),
                drag,
            });
        }
        for opening in &state.panels {
            let id = opening.id();
            let Some(panel) = channel_panels::get(opening.panel, cx) else {
                continue;
            };
            let view = match self.tab_views.get(&id) {
                Some(TabView::Panel(view)) => Some(view.clone()),
                _ => None,
            };
            let label = view
                .as_ref()
                .and_then(|view| channel_panels::title(panel.key, view, cx))
                .unwrap_or_else(|| panel.title.into());
            let content = match view {
                Some(view) => view.into_any_element(),
                None => div().into_any_element(),
            };
            let close = closer(id.clone());
            items.push(TabItem {
                id: id.into(),
                label,
                icon: panel.icon,
                content,
                close,
                drag: None,
            });
        }
        items
    }

    /// The main pane's tabs after the conversation, and the thread shown
    /// in its place.
    fn main_tabs(&mut self, state: &TabState, cx: &mut Context<Self>) -> MainTabs {
        let weak = cx.entity().downgrade();
        let tabs = state
            .main_tabs()
            .map(|tab| {
                let (label, icon) = self.tab_identity(tab, cx);
                let weak = weak.clone();
                let id = tab.id.clone();
                let drag = TabDrag {
                    id: tab.id.clone(),
                    label: label.clone(),
                    pane: TabPane::Main,
                };
                MainTab {
                    id: tab.id.clone().into(),
                    label,
                    icon,
                    close: Rc::new(move |window, cx| {
                        let _ = weak.update(cx, |this, cx| this.close_tab(&id, window, cx));
                    }),
                    drag: Some(drag),
                }
            })
            .collect();
        let selected = state.selected_main();
        let content = selected.as_ref().map(|id| match self.tab_views.get(id) {
            Some(TabView::Thread { panel, .. }) => panel.clone().into(),
            _ => AnyView::from(cx.new(|_| Unavailable)),
        });
        MainTabs {
            tabs,
            selected: selected.map(SharedString::from),
            content,
            select: Rc::new(move |id, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.select_main_tab(id.map(|id| id.to_string()), window, cx)
                });
            }),
            geometry: self.tab_geometry.clone(),
            strip: self.main_strip.clone(),
        }
    }

    /// The Messages page with a channel open.
    pub(super) fn board(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(page) = self.channel.clone() else {
            return div().into_any_element();
        };
        let laid_out = TabGeometry::clear(&self.tab_geometry);
        if !cx.has_active_drag() {
            // A drag released outside the board leaves nothing behind.
            self.drag_target = None;
            self.drag_cancelled = false;
            TabGeometry::finish(&self.tab_geometry);
        }
        let state = self.tabs(cx);
        self.ensure_tab_views(window, cx);
        self.reveal_main(&state, &laid_out, window);
        let side = self
            .drawer
            .as_ref()
            .filter(|drawer| drawer.placement == channel_panels::Placement::Side)
            .map(|drawer| drawer.view.clone());
        let bottom = self
            .drawer
            .as_ref()
            .filter(|drawer| drawer.placement == channel_panels::Placement::Bottom)
            .map(|drawer| drawer.view.clone());
        let showing = state.showing();
        let has_content = state.has_panel() || side.is_some();
        let weak = cx.entity().downgrade();
        let toggle: PaneToggle = Rc::new(move |window, cx| {
            let _ = weak.update(cx, |this, cx| this.toggle_pane(window, cx));
        });
        let launchers = self.launchers(cx);
        let menu_panels = self.menu_panels(cx);
        let main_tabs = self.main_tabs(&state, cx);
        page.update(cx, |page, _| {
            page.pane = Some(PaneButton {
                open: state.pane_open && has_content,
                enabled: has_content,
                toggle,
            });
            page.launchers = launchers;
            page.menu_panels = menu_panels;
            page.main_tabs = Some(main_tabs);
        });
        let pane = showing.then(|| {
            let items = self.tab_items(&state, window, cx);
            let selected = state.selected_tab().unwrap_or_default();
            // A newly selected tab scrolls into view; the user may scroll
            // away from it after.
            if self.revealed_tab.as_deref() != Some(selected.as_str()) {
                let bounds = laid_out.get(&selected).copied();
                match panels::reveal_tab(&self.tab_strip, bounds) {
                    false => window.request_animation_frame(),
                    true => self.revealed_tab = Some(selected.clone()),
                }
            }
            let weak = cx.entity().downgrade();
            panels::tabs(
                items,
                &selected,
                &self.tab_strip,
                Rc::new(move |id, window, cx| {
                    let _ = weak.update(cx, |this, cx| this.select_tab(&id, window, cx));
                }),
                Some(&self.tab_geometry),
                cx,
            )
        });
        let companion = self.companion_panel(window, cx);
        // The pane, a launcher's side panel and the companion stack in
        // equal rows, divided by hairlines.
        let side = side
            .filter(|_| state.pane_open)
            .map(IntoElement::into_any_element);
        let rows: Vec<AnyElement> = [pane, side, companion].into_iter().flatten().collect();
        let border = cx.colors().border_standard;
        let side_area = self.side_area.clone();
        let dock = (!rows.is_empty()).then(|| {
            div()
                .track_focus(&side_area)
                .flex()
                .flex_col()
                .size_full()
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .when(index > 0, |this| this.border_t_1().border_color(border))
                        .child(row)
                }))
                .into_any_element()
        });
        // A bottom launcher panel takes 42% under the conversation.
        let conversation = match bottom {
            Some(view) => div()
                .flex()
                .flex_col()
                .size_full()
                .child(div().flex().flex_1().min_h_0().child(page))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .h(relative(0.42))
                        .min_h(px(180.))
                        .max_h(relative(0.65))
                        .border_t_1()
                        .border_color(border)
                        .overflow_hidden()
                        .child(view),
                )
                .into_any_element(),
            None => page.into_any_element(),
        };
        // The main pane owns Close while focus is in it.
        let conversation = div()
            .track_focus(&self.main_area)
            .relative()
            .flex()
            .size_full()
            .min_w_0()
            .child(conversation)
            .child(tab_drag::record(&self.tab_geometry, |geometry, bounds| {
                geometry.main = Some(bounds);
            }))
            .into_any_element();
        let weak = cx.entity().downgrade();
        let commit = weak.clone();
        let board = Dock {
            id: "board".into(),
            split: Split::BOARD,
            preferred: self.board_width,
            window: f32::from(window.viewport_size().width),
            available: self.main_width(window, cx),
            opening: self.pane_opening + self.companion_opening,
            on_resize: Rc::new(move |width, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.board_width = width;
                    cx.notify();
                });
            }),
            on_commit: Rc::new(move |_, cx| {
                let _ = commit.update(cx, |_, _| {});
            }),
        }
        .render(conversation, dock, cx);
        let (moved, dropped, escape) = (
            cx.entity().downgrade(),
            cx.entity().downgrade(),
            cx.entity().downgrade(),
        );
        let feedback = cx
            .has_active_drag()
            .then(|| tab_drag::feedback(self.drag_target, cx))
            .flatten();
        div()
            .flex()
            .size_full()
            .min_w_0()
            .on_drag_move::<TabDrag>(move |event, window, cx| {
                let drag = event.drag(cx).clone();
                let position = event.event.position;
                let _ = moved.update(cx, |this, cx| this.drag_moved(&drag, position, window, cx));
            })
            .on_drop::<TabDrag>(move |drag, window, cx| {
                let drag = drag.clone();
                let _ = dropped.update(cx, |this, cx| this.drag_dropped(&drag, window, cx));
            })
            .capture_key_down(move |event, _, cx| {
                if event.keystroke.key == "escape" && cx.has_active_drag() {
                    cx.stop_propagation();
                    let _ = escape.update(cx, |this, cx| this.cancel_drag(cx));
                }
            })
            .child(board)
            .children(feedback)
            .into_any_element()
    }

    /// Scrolls the main strip to a newly selected tab.
    fn reveal_main(
        &mut self,
        state: &TabState,
        laid_out: &HashMap<String, Bounds<Pixels>>,
        window: &mut Window,
    ) {
        let selected = state.selected_main().unwrap_or_default();
        if self.revealed_main.as_deref() == Some(selected.as_str()) {
            return;
        }
        let bounds = laid_out.get(&selected).copied();
        if selected.is_empty() || panels::reveal_tab(&self.main_strip, bounds) {
            self.revealed_main = Some(selected);
        } else {
            window.request_animation_frame();
        }
    }

    /// Where a dragged tab could go: either pane for a thread of this
    /// channel, only the side pane for another conversation's.
    fn drag_rules<'a>(
        &self,
        state: &'a TabState,
        side_strip: &'a [String],
        can_move: &'a dyn Fn(TabPane) -> bool,
    ) -> Rules<'a> {
        let side_tabs = state.side_tabs().count();
        let side_drawer = self.side_drawer();
        Rules {
            order: vec![
                PaneOrder {
                    pane: TabPane::Main,
                    tabs: state.main_tabs().map(|tab| tab.id.as_str()).collect(),
                    strip: std::iter::once(tab_drag::CONVERSATION)
                        .chain(state.main_tabs().map(|tab| tab.id.as_str()))
                        .collect(),
                },
                PaneOrder {
                    pane: TabPane::Side,
                    tabs: state.side_tabs().map(|tab| tab.id.as_str()).collect(),
                    strip: side_strip.iter().map(String::as_str).collect(),
                },
            ],
            can_move,
            side_visible: state.pane_open && (state.has_panel() || side_drawer),
            closed_side_index: (!state.pane_open || (state.root_ids().is_empty() && !side_drawer))
                .then_some(side_tabs),
        }
    }

    /// A dragged tab moved: its drop target follows the pointer, and a
    /// long strip scrolls while the pointer is held near its ends.
    fn drag_moved(
        &mut self,
        drag: &TabDrag,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drag_cancelled {
            return;
        }
        let Some(start) = self.tab_geometry.borrow().start else {
            return;
        };
        // A resized window is a new layout; the gesture ends.
        if start.viewport != window.viewport_size() {
            self.cancel_drag(cx);
            return;
        }
        let state = self.tabs(cx);
        let current = self.channel_id(cx);
        let Some(thread) = state.tab(&drag.id).and_then(|tab| tab.thread()).cloned() else {
            self.cancel_drag(cx);
            return;
        };
        let can_move =
            |pane: TabPane| pane == TabPane::Side || current.as_deref() == Some(&thread.channel);
        let side_strip = state.root_ids();
        let rules = self.drag_rules(&state, &side_strip, &can_move);
        let target = tab_drag::target(
            &self.tab_geometry.borrow(),
            drag,
            start.at,
            position,
            &rules,
            self.drag_target,
        );
        for (pane, strip) in [
            (tab_drag::PaneKey::Side, &self.tab_strip),
            (tab_drag::PaneKey::Main, &self.main_strip),
        ] {
            let header = self.tab_geometry.borrow().headers.get(&pane).copied();
            if header.is_some_and(|header| header.contains(&position)) {
                tab_drag::scroll_near_ends(strip, position);
            }
        }
        if target != self.drag_target {
            self.drag_target = target;
            cx.notify();
        }
    }

    /// The pointer let go: the tab moves to the drop target, and takes
    /// the focus where it landed.
    fn drag_dropped(&mut self, drag: &TabDrag, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.drag_target.take();
        let cancelled = std::mem::take(&mut self.drag_cancelled);
        TabGeometry::finish(&self.tab_geometry);
        cx.notify();
        let Some(target) = target.filter(|_| !cancelled) else {
            return;
        };
        let state = self.tabs(cx);
        let current = self.channel_id(cx);
        let Some(moving) = state.tab(&drag.id) else {
            return;
        };
        let Some(thread) = moving.thread() else {
            return;
        };
        if target.pane == TabPane::Main && current.as_deref() != Some(&thread.channel) {
            return;
        }
        let order: Vec<&str> = state
            .tabs
            .iter()
            .filter(|tab| tab.pane() == target.pane)
            .map(|tab| tab.id.as_str())
            .collect();
        if moving.pane() == target.pane
            && order.iter().position(|id| *id == drag.id) == Some(target.index)
        {
            return;
        }
        let id = drag.id.clone();
        self.update_tabs(cx, |state| state.move_tab(&id, target.pane, target.index));
        self.focus_tab(&id, window, cx);
    }

    /// Escape or a resize ends the drag without a move.
    fn cancel_drag(&mut self, cx: &mut Context<Self>) {
        self.drag_target = None;
        self.drag_cancelled = true;
        cx.notify();
    }

    /// "View channel details": the Channel settings tab, focused, as only
    /// an explicit open moves focus there.
    pub(super) fn open_channel_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drawer = None;
        self.update_tabs(cx, |state| state.open_settings());
        self.focus_tab(channel_tabs::SETTINGS, window, cx);
    }

    /// A link clicked in the open channel: a registered panel's link
    /// opens as a tab beside it, and a link to a joined session (with no
    /// message) as a conversation tab. False when neither applies.
    pub(super) fn route_link_to_tab(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.page != Page::MESSAGES {
            return false;
        }
        let Some((_, current)) = self.tab_key(cx) else {
            return false;
        };
        if let Some(panel) = channel_panels::resolve(url, cx) {
            let tab = PanelTab {
                panel: panel.key,
                channel: current,
                target: url.to_string(),
            };
            let id = tab.id();
            self.update_tabs(cx, |state| state.open_panel(tab));
            self.focus_tab(&id, window, cx);
            return true;
        }
        let Some(crate::deep_links::Link::Conversation {
            channel,
            message: None,
            ..
        }) = crate::deep_links::parse(url)
        else {
            return false;
        };
        let Some(community) = self.community.clone() else {
            return false;
        };
        let session = usable(&community, &channel, cx)
            .is_some_and(|summary| summary.channel_type == Some(ChannelType::Session));
        if !session || channel == current {
            return false;
        }
        let id = format!("conversation:{channel}");
        self.update_tabs(cx, |state| {
            if !state.tabs.iter().any(|tab| tab.id == id) {
                state.tabs.push(channel_tabs::ConversationTab {
                    id: id.clone(),
                    kind: TabKind::Conversation { channel },
                });
            }
            state.select(&id);
        });
        self.focus_tab(&id, window, cx);
        true
    }
}

/// A main-pane tab whose thread can no longer be read here.
struct Unavailable;

impl Render for Unavailable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Workspace::unavailable(cx)
    }
}
