//! The Messages board (`ChannelsPage.tsx`): the conversation, and beside
//! it the channel's tab pane holding the thread, other conversations and
//! threads, New tab pickers and channel tools, with the companion panel
//! stacked under it.

use super::Workspace;
use super::layout::Split;
use crate::community::Community;
use crate::ui::channel::{ChannelPage, Launcher, OpenThread, PaneToggle};
use crate::ui::channel_menu::MenuPanel;
use crate::ui::channel_panels::{self, ChannelPanel};
use crate::ui::channel_settings::{self as settings_panel, ChannelRemoved, ChannelSettingsPanel};
use crate::ui::channel_tab_picker::{ChannelTabPicker, ChooseChannel, ChooseTool, Tool};
use crate::ui::channel_tabs::{self, PanelTab, TabKind, TabState, ThreadTarget};
use crate::ui::message::channel_icon;
use crate::ui::pages::Page;
use crate::ui::panels::{self, Close, Dock, TabItem};
use crate::ui::thread::{self, ThreadPanel};
use buzz_relay_client::{ChannelSummary, ChannelType};
use buzz_ui::{ActiveTheme, IconName, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nostr::prelude::EventId;
use std::rc::Rc;
use std::sync::Arc;

/// A channel panel opened from its header launcher.
pub(super) struct Drawer {
    key: &'static str,
    placement: channel_panels::Placement,
    view: AnyView,
}

/// A tab's view, made when the tab first shows on this visit.
pub(super) enum TabView {
    Settings {
        panel: Entity<ChannelSettingsPanel>,
        _subscription: Subscription,
    },
    Picker(Entity<ChannelTabPicker>),
    Conversation(Entity<ChannelPage>),
    Thread(Entity<ThreadPanel>),
    Panel(AnyView),
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
        let ids = state.root_ids();
        self.tab_views.retain(|id, _| ids.contains(id));
        cx.notify();
        Some(result)
    }

    /// The tabs of the channel being left stay saved; their views go, as
    /// does any launcher panel, which belongs to the visit.
    pub(super) fn leave_channel_tabs(&mut self) {
        self.tab_views.clear();
        self.thread = None;
        self.drawer = None;
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
                Launcher {
                    key,
                    icon: launcher.icon,
                    label: launcher.label,
                    pressed: self.drawer.as_ref().is_some_and(|drawer| drawer.key == key),
                    toggle: Rc::new(move |window, cx| {
                        let panel = panel.clone();
                        let _ = weak.update(cx, |this, cx| this.toggle_launcher(panel, window, cx));
                    }),
                }
            })
            .collect()
    }

    /// The channel just shown gets back the thread it had.
    pub(super) fn restore_channel_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(thread) = self.tabs(cx).thread {
            self.make_thread(thread, window, cx);
        }
    }

    fn new_tab_id() -> String {
        format!("new:{}", uuid::Uuid::new_v4())
    }

    pub(super) fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = Self::new_tab_id();
        self.update_tabs(cx, |state| state.add_new(id.clone()));
        self.focus_tab(&id, window, cx);
    }

    pub(super) fn toggle_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self
            .update_tabs(cx, |state| {
                state.toggle_pane(Self::new_tab_id);
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

    /// Closes the shown tab (Escape inside it, Mod+W). False when the
    /// pane shows nothing.
    pub(super) fn close_selected_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = self.tabs(cx);
        if !state.showing() {
            return false;
        }
        match state.selected_tab().as_deref() {
            Some(channel_tabs::THREAD) => {
                self.close_thread(window, cx);
                self.record(cx);
            }
            Some(id) => {
                let id = id.to_string();
                self.close_tab(&id, window, cx);
            }
            None => return false,
        }
        true
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
        match remaining {
            Some(next) => self.focus_tab(&next, window, cx),
            None => self.focus_composer(window, cx),
        }
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
        if id == channel_tabs::THREAD {
            if let Some(thread) = &self.thread {
                ThreadPanel::focus(thread, window, cx);
            }
            return;
        }
        match self.tab_views.get(id) {
            Some(TabView::Settings { panel, .. }) => {
                let handle = panel.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }
            Some(TabView::Picker(picker)) => ChannelTabPicker::focus(&picker.clone(), window, cx),
            Some(TabView::Conversation(page)) => {
                let composer = page.read(cx).composer();
                composer.update(cx, |state, cx| state.focus(window, cx));
            }
            Some(TabView::Thread(panel)) => ThreadPanel::focus(panel, window, cx),
            _ => {}
        }
    }

    /// Makes the thread view beside the conversation.
    pub(super) fn make_thread(
        &mut self,
        target: ThreadTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ThreadPanel>> {
        let community = self.community.clone()?;
        let channel = self.channel.as_ref()?.read(cx).channel_id().to_string();
        let weak = cx.entity().downgrade();
        let on_close: thread::OnClose = Arc::new(move |window, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.close_thread(window, cx);
                this.record(cx);
            });
        });
        let panel = cx.new(|cx| match target.exact {
            true => ThreadPanel::exact(community, channel, target.root, on_close, window, cx),
            false => ThreadPanel::new(community, channel, target.root, on_close, window, cx),
        });
        // Media hook: a clip started in the replaced thread stops with it.
        if let Some(old) = &self.thread {
            super::super::media::pause_audio(Some(old.entity_id()), cx);
        }
        self.thread = Some(panel.clone());
        self.thread_exact = target.exact;
        Some(panel)
    }

    /// Opens a thread of another conversation as its own tab.
    fn open_conversation_thread(
        &mut self,
        channel: String,
        message: EventId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = format!("thread:{channel}:{}", message.to_hex());
        self.tab_views.remove(&id);
        self.update_tabs(cx, |state| {
            state.open_conversation_thread(&channel, message, message)
        });
        self.focus_tab(&id, window, cx);
    }

    /// The current channel's summary.
    fn current_summary(&self, cx: &App) -> Option<ChannelSummary> {
        Some(self.channel.as_ref()?.read(cx).channel().clone())
    }

    /// The channel's tools: registered channel panels offered under Tools.
    fn tools(&self, cx: &App) -> Vec<Tool> {
        let Some(channel) = self.current_summary(cx) else {
            return Vec::new();
        };
        channel_panels::tools(&channel, cx)
            .into_iter()
            .map(|panel| Tool {
                key: panel.key,
                title: panel.title,
                icon: panel.icon,
            })
            .collect()
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
        let thread = self
            .tabs(cx)
            .thread
            .filter(|_| self.thread.is_some())
            .map(|thread| thread.root.to_hex());
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
        for tab in &state.tabs {
            if self.tab_views.contains_key(&tab.id) {
                continue;
            }
            let view = match &tab.kind {
                TabKind::New => {
                    let picker_id = tab.id.clone();
                    let weak = cx.entity().downgrade();
                    let choose: ChooseChannel = Rc::new(move |channel, window, cx| {
                        let picker = picker_id.clone();
                        let _ = weak.update(cx, |this, cx| {
                            this.choose_conversation(&picker, &channel, window, cx)
                        });
                    });
                    let picker_id = tab.id.clone();
                    let weak = cx.entity().downgrade();
                    let choose_tool: ChooseTool = Rc::new(move |key, _, cx| {
                        let picker = picker_id.clone();
                        let _ = weak.update(cx, |this, cx| {
                            let channel = this.tab_key(cx).map(|(_, c)| c).unwrap_or_default();
                            this.tab_views.remove(&picker);
                            // A tool has one place on screen at a time.
                            this.drawer = None;
                            this.update_tabs(cx, |state| {
                                state.choose_tool(&picker, PanelTab::tool(key, &channel))
                            });
                        });
                    });
                    let tools = self.tools(cx);
                    let current = current_id.clone();
                    let community = community.clone();
                    TabView::Picker(cx.new(|cx| {
                        ChannelTabPicker::new(
                            community,
                            current,
                            tools,
                            choose,
                            choose_tool,
                            window,
                            cx,
                        )
                    }))
                }
                TabKind::Conversation { channel } => {
                    let Some(summary) = usable(&community, channel, cx) else {
                        continue;
                    };
                    let weak = cx.entity().downgrade();
                    let channel_id = channel.clone();
                    let open_thread: OpenThread = Arc::new(move |row, _, window, cx| {
                        let channel = channel_id.clone();
                        let _ = weak.update(cx, |this, cx| {
                            this.open_conversation_thread(channel, row.id, window, cx)
                        });
                    });
                    let community = community.clone();
                    let page = cx.new(|cx| {
                        let mut page =
                            ChannelPage::new(community, summary, open_thread, window, cx);
                        page.secondary = true;
                        page
                    });
                    TabView::Conversation(page)
                }
                TabKind::Thread { channel, message } => {
                    if usable(&community, channel, cx).is_none() {
                        continue;
                    }
                    let id = tab.id.clone();
                    let weak = cx.entity().downgrade();
                    let on_close: thread::OnClose = Arc::new(move |window, cx| {
                        let id = id.clone();
                        let _ = weak.update(cx, |this, cx| this.close_tab(&id, window, cx));
                    });
                    let (community, channel, message) =
                        (community.clone(), channel.clone(), *message);
                    TabView::Thread(cx.new(|cx| {
                        ThreadPanel::new(community, channel, message, on_close, window, cx)
                    }))
                }
            };
            self.tab_views.insert(tab.id.clone(), view);
        }
        for opening in &state.panels {
            let id = opening.id();
            if self.tab_views.contains_key(&id) {
                continue;
            }
            let Some(panel) = channel_panels::get(opening.panel, cx) else {
                continue;
            };
            let weak = cx.entity().downgrade();
            let close_id = id.clone();
            let close: channel_panels::Close = Rc::new(move |window, cx| {
                let id = close_id.clone();
                let _ = weak.update(cx, |this, cx| this.close_tab(&id, window, cx));
            });
            if let Some(view) = self.panel_view(
                &panel,
                opening.channel.clone(),
                opening.target.clone(),
                close,
                window,
                cx,
            ) {
                self.tab_views.insert(id, TabView::Panel(view));
            }
        }
    }

    fn choose_conversation(
        &mut self,
        picker: &str,
        channel: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Membership is checked again when chosen, not just when listed.
        let Some(community) = self.community.clone() else {
            return;
        };
        if usable(&community, channel, cx).is_none()
            || self.tab_key(cx).is_some_and(|(_, c)| c == channel)
        {
            return;
        }
        self.tab_views.remove(picker);
        self.update_tabs(cx, |state| state.choose_conversation(picker, channel));
        self.focus_tab(&format!("conversation:{channel}"), window, cx);
    }

    /// The pane's tabs as the tab strip shows them.
    fn tab_items(
        &mut self,
        state: &TabState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TabItem> {
        self.ensure_tab_views(window, cx);
        let c = *cx.colors();
        let Some(community) = self.community.clone() else {
            return Vec::new();
        };
        let unavailable = || {
            div()
                .px(rems(1.))
                .py(rems(2.))
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("This conversation is no longer available.")
                .into_any_element()
        };
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
            });
        }
        if state.thread.is_some()
            && let Some(thread) = self.thread.clone()
        {
            thread.update(cx, |thread, _| thread.show_header = false);
            let weak = cx.entity().downgrade();
            items.push(TabItem {
                id: channel_tabs::THREAD.into(),
                label: "Thread".into(),
                icon: IconName::ChatCircle,
                content: thread.into_any_element(),
                close: Rc::new(move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.close_thread(window, cx);
                        this.record(cx);
                    });
                }),
            });
        }
        for tab in &state.tabs {
            let close = closer(tab.id.clone());
            let (label, icon): (SharedString, IconName) = match &tab.kind {
                TabKind::New => ("New tab".into(), IconName::Browser),
                TabKind::Conversation { channel } => match usable(&community, channel, cx) {
                    Some(summary) => (summary.name.clone().into(), tab_icon(&summary)),
                    None => ("Unavailable conversation".into(), IconName::Browser),
                },
                TabKind::Thread { channel, .. } => match usable(&community, channel, cx) {
                    Some(summary) => (
                        format!("Thread · {}", summary.name).into(),
                        tab_icon(&summary),
                    ),
                    None => ("Thread · Unavailable".into(), IconName::Browser),
                },
            };
            let content = match self.tab_views.get(&tab.id) {
                Some(TabView::Picker(picker)) => picker.clone().into_any_element(),
                Some(TabView::Conversation(page)) => page.clone().into_any_element(),
                Some(TabView::Thread(panel)) => {
                    panel.update(cx, |panel, _| panel.show_header = false);
                    panel.clone().into_any_element()
                }
                Some(TabView::Panel(view)) => view.clone().into_any_element(),
                Some(TabView::Settings { .. }) | None => unavailable(),
            };
            items.push(TabItem {
                id: tab.id.clone().into(),
                label,
                icon,
                content,
                close,
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
            });
        }
        items
    }

    /// The Messages page with a channel open.
    pub(super) fn board(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(page) = self.channel.clone() else {
            return div().into_any_element();
        };
        let state = self.tabs(cx);
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
        let weak = cx.entity().downgrade();
        let toggle: PaneToggle = Rc::new(move |window, cx| {
            let _ = weak.update(cx, |this, cx| this.toggle_pane(window, cx));
        });
        let launchers = self.launchers(cx);
        let menu_panels = self.menu_panels(cx);
        page.update(cx, |page, _| {
            page.pane = Some((showing, toggle));
            page.launchers = launchers;
            page.menu_panels = menu_panels;
        });
        let pane = showing.then(|| {
            let items = self.tab_items(&state, window, cx);
            let selected = state.selected_tab().unwrap_or_default();
            // A newly selected tab scrolls into view; the user may scroll
            // away from it after.
            if self.revealed_tab.as_deref() != Some(selected.as_str()) {
                let index = items.iter().position(|item| item.id.as_ref() == selected);
                match index.map(|index| panels::reveal_tab(&self.tab_strip, index, window)) {
                    Some(false) => window.request_animation_frame(),
                    _ => self.revealed_tab = Some(selected.clone()),
                }
            }
            let weak = cx.entity().downgrade();
            let add = weak.clone();
            panels::tabs(
                items,
                &selected,
                &self.tab_strip,
                Rc::new(move |id, window, cx| {
                    let _ = weak.update(cx, |this, cx| this.select_tab(&id, window, cx));
                }),
                Some(Rc::new(move |window, cx| {
                    let _ = add.update(cx, |this, cx| this.add_tab(window, cx));
                })),
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
        let dock = (!rows.is_empty()).then(|| {
            div()
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
        let weak = cx.entity().downgrade();
        let commit = weak.clone();
        Dock {
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
        .render(conversation, dock, cx)
    }

    /// Opens the thread beside the conversation, in its tab.
    pub(super) fn open_thread_tab(
        &mut self,
        target: ThreadTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.make_thread(target, window, cx) else {
            return;
        };
        self.update_tabs(cx, |state| state.open_thread(target));
        ThreadPanel::focus(&panel, window, cx);
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

/// The conversation `id` when a tab may show it: joined, writable, not
/// archived, and for sessions only when the viewer is a member.
fn usable(community: &Entity<Community>, id: &str, cx: &App) -> Option<ChannelSummary> {
    let community = community.read(cx);
    let viewer = community.session.viewer().to_string();
    community
        .snapshot
        .channels
        .iter()
        .find(|channel| channel.id == id)
        .filter(|channel| {
            !channel.read_only
                && !channel.archived
                && !channel.cached
                && (channel.channel_type != Some(ChannelType::Session)
                    || channel.members.contains(&viewer))
        })
        .cloned()
}

/// A conversation tab's icon: the speech bubble for DMs, else the
/// channel's hash or lock.
fn tab_icon(channel: &ChannelSummary) -> IconName {
    match channel.channel_type {
        Some(ChannelType::Dm) => IconName::ChatCircle,
        _ => channel_icon(channel),
    }
}
