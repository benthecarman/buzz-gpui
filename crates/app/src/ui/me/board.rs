//! A conversation open in Me (`SessionsPage.tsx` `SessionWork`): its
//! heading with Share, the session actions and the tab pane toggle, the
//! conversation itself, and the tab pane beside it, as on the Messages
//! board but with Me's own tabs.

use super::MeModel;
use super::dialogs::{self, DialogHost};
use crate::community::Community;
use crate::ui::channel::{ChannelPage, OpenThread};
use crate::ui::channel_panels::{self, ChannelPanel};
use crate::ui::channel_tab_picker::{ChannelTabPicker, ChooseChannel, ChooseTool, Tool};
use crate::ui::channel_tabs::{self, PanelTab, TabKind, TabState, ThreadTarget};
use crate::ui::message::channel_icon;
use crate::ui::overlay::{self, Align, Dismiss};
use crate::ui::panels::{self, Close, Dock, TabItem};
use crate::ui::thread::{self, ThreadPanel};
use crate::ui::workspace::layout::Split;
use buzz_relay_client::{ChannelSummary, ChannelType};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, IconButton, IconButtonSize, IconName, Menu, MenuItem,
    PanelHeader, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

enum TabView {
    Picker(Entity<ChannelTabPicker>),
    Conversation(Entity<ChannelPage>),
    Thread(Entity<ThreadPanel>),
    Panel(AnyView),
}

/// A channel panel opened from its header launcher.
struct Drawer {
    key: &'static str,
    placement: channel_panels::Placement,
    view: AnyView,
}

pub struct SessionWork {
    community: Entity<Community>,
    model: Entity<MeModel>,
    channel: ChannelSummary,
    page: Entity<ChannelPage>,
    thread: Option<Entity<ThreadPanel>>,
    tab_views: HashMap<String, TabView>,
    tab_strip: ScrollHandle,
    revealed_tab: Option<String>,
    drawer: Option<Drawer>,
    menu_open: bool,
    dialog: Option<dialogs::Dialog>,
    width: Option<f32>,
    opening: u64,
    _subscriptions: Vec<Subscription>,
}

impl SessionWork {
    /// `focus`: the composer takes the focus.
    pub fn new(
        community: Entity<Community>,
        channel: ChannelSummary,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = MeModel::global(cx);
        let weak = cx.entity().downgrade();
        let open_thread: OpenThread = Arc::new(move |row, _, window, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.open_thread(
                    ThreadTarget {
                        root: row.id,
                        exact: false,
                    },
                    window,
                    cx,
                )
            });
        });
        let page = cx.new(|cx| {
            let mut page =
                ChannelPage::new(community.clone(), channel.clone(), open_thread, window, cx);
            page.secondary = true;
            page.set_personal(window, cx);
            page
        });
        page.read(cx)
            .header_actions()
            .update(cx, |actions, _| actions.members_only = true);
        let subscriptions = vec![
            cx.observe(&community, |this, community, cx| {
                if let Some(current) = community
                    .read(cx)
                    .snapshot
                    .channels
                    .iter()
                    .find(|c| c.id == this.channel.id)
                {
                    this.channel = current.clone();
                }
                cx.notify();
            }),
            cx.observe(&model, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            community,
            model,
            channel,
            page,
            thread: None,
            tab_views: HashMap::new(),
            tab_strip: ScrollHandle::new(),
            revealed_tab: None,
            drawer: None,
            menu_open: false,
            dialog: None,
            width: None,
            opening: 0,
            _subscriptions: subscriptions,
        };
        // The conversation gets back the thread it had.
        if let Some(thread) = this.tabs(cx).thread {
            this.make_thread(thread, window, cx);
        }
        if focus {
            this.page.update(cx, |page, cx| {
                let composer = page.composer();
                composer.update(cx, |state, cx| state.focus(window, cx));
            });
        }
        this
    }

    fn tabs(&self, cx: &App) -> TabState {
        self.model.read(cx).tab_state(&self.channel.id, cx)
    }

    fn update_tabs<R>(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TabState) -> R) -> R {
        let id = self.channel.id.clone();
        let (result, opened, ids) = self.model.update(cx, |model, cx| {
            model.update_tab_state(&id, cx, |state| {
                let was = state.showing();
                let result = f(state);
                (result, !was && state.showing(), state.root_ids())
            })
        });
        if opened {
            self.opening += 1;
        }
        self.tab_views.retain(|id, _| ids.contains(id));
        cx.notify();
        result
    }

    fn new_tab_id() -> String {
        format!("new:{}", uuid::Uuid::new_v4())
    }

    fn add_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = Self::new_tab_id();
        self.update_tabs(cx, |state| state.add_new(id.clone()));
        self.focus_tab(&id, window, cx);
    }

    /// The heading toggle: shows the pane, adding a New tab when there is
    /// nothing in it.
    fn toggle_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.update_tabs(cx, |state| {
            state.toggle_pane(Self::new_tab_id);
            state.showing()
        });
        match self.tabs(cx).selected_tab() {
            Some(id) if showing => self.focus_tab(&id, window, cx),
            _ => self.focus_composer(window, cx),
        }
    }

    fn select_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.update_tabs(cx, |state| state.select(id));
        self.focus_tab(id, window, cx);
    }

    fn close_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_owned();
        let remaining = self.update_tabs(cx, |state| {
            if state.panels.iter().any(|panel| panel.id() == id) {
                state.close_panel(&id);
            } else {
                state.close_tab(&id);
            }
            state.selected_tab()
        });
        match remaining {
            Some(next) => self.focus_tab(&next, window, cx),
            None => self.focus_composer(window, cx),
        }
    }

    fn close_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread = None;
        let remaining = self.update_tabs(cx, |state| {
            state.close_thread();
            state.selected_tab()
        });
        match remaining {
            Some(next) => self.focus_tab(&next, window, cx),
            None => self.focus_composer(window, cx),
        }
    }

    fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        let composer = self.page.read(cx).composer();
        composer.update(cx, |state, cx| state.focus(window, cx));
    }

    fn focus_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tab_views(window, cx);
        if id == channel_tabs::THREAD {
            if let Some(thread) = &self.thread {
                ThreadPanel::focus(thread, window, cx);
            }
            return;
        }
        match self.tab_views.get(id) {
            Some(TabView::Picker(picker)) => ChannelTabPicker::focus(&picker.clone(), window, cx),
            Some(TabView::Conversation(page)) => {
                let composer = page.read(cx).composer();
                composer.update(cx, |state, cx| state.focus(window, cx));
            }
            Some(TabView::Thread(panel)) => ThreadPanel::focus(panel, window, cx),
            _ => {}
        }
    }

    fn make_thread(
        &mut self,
        target: ThreadTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ThreadPanel> {
        let weak = cx.entity().downgrade();
        let on_close: thread::OnClose = Arc::new(move |window, cx| {
            let _ = weak.update(cx, |this, cx| this.close_thread(window, cx));
        });
        let (community, channel) = (self.community.clone(), self.channel.id.clone());
        let panel = cx.new(|cx| match target.exact {
            true => ThreadPanel::exact(community, channel, target.root, on_close, window, cx),
            false => ThreadPanel::new(community, channel, target.root, on_close, window, cx),
        });
        self.thread = Some(panel.clone());
        panel
    }

    fn open_thread(&mut self, target: ThreadTarget, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.make_thread(target, window, cx);
        self.update_tabs(cx, |state| state.open_thread(target));
        ThreadPanel::focus(&panel, window, cx);
    }

    fn tools(&self, cx: &App) -> Vec<Tool> {
        channel_panels::tools(&self.channel, cx)
            .into_iter()
            .map(|panel| Tool {
                key: panel.key,
                title: panel.title,
                icon: panel.icon,
            })
            .collect()
    }

    fn panel_view(
        &self,
        panel: &ChannelPanel,
        target: String,
        close: channel_panels::Close,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyView {
        (panel.build)(
            channel_panels::Opening {
                community: self.community.clone(),
                channel: self.channel.id.clone(),
                target,
                close,
                thread: self.tabs(cx).thread.map(|thread| thread.root.to_hex()),
            },
            window,
            cx,
        )
    }

    fn toggle_launcher(
        &mut self,
        panel: ChannelPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = PanelTab::tool(panel.key, &self.channel.id);
        let state = self.tabs(cx);
        if state.panels.contains(&tab) {
            if state.showing() && state.selected_tab() == Some(tab.id()) {
                self.update_tabs(cx, |state| state.pane_open = false);
            } else {
                self.select_tab(&tab.id(), window, cx);
            }
            return;
        }
        if self.drawer.as_ref().is_some_and(|d| d.key == panel.key) {
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
                if this.drawer.as_ref().is_some_and(|d| d.key == key) {
                    this.drawer = None;
                    this.focus_composer(window, cx);
                    cx.notify();
                }
            });
        });
        let view = self.panel_view(&panel, self.channel.id.clone(), close, window, cx);
        self.update_tabs(cx, |state| state.pane_open = true);
        self.drawer = Some(Drawer {
            key,
            placement: launcher.placement,
            view,
        });
        cx.notify();
    }

    fn ensure_tab_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.tabs(cx);
        let community = self.community.clone();
        for tab in &state.tabs {
            if self.tab_views.contains_key(&tab.id) {
                continue;
            }
            let view = match &tab.kind {
                TabKind::New => {
                    let picker = tab.id.clone();
                    let weak = cx.entity().downgrade();
                    let choose: ChooseChannel = Rc::new(move |channel, window, cx| {
                        let picker = picker.clone();
                        let _ = weak.update(cx, |this, cx| {
                            this.choose_conversation(&picker, &channel, window, cx)
                        });
                    });
                    let picker = tab.id.clone();
                    let weak = cx.entity().downgrade();
                    let choose_tool: ChooseTool = Rc::new(move |key, _, cx| {
                        let picker = picker.clone();
                        let _ = weak.update(cx, |this, cx| {
                            let channel = this.channel.id.clone();
                            this.tab_views.remove(&picker);
                            this.drawer = None;
                            this.update_tabs(cx, |state| {
                                state.choose_tool(&picker, PanelTab::tool(key, &channel))
                            });
                        });
                    });
                    let tools = self.tools(cx);
                    let (community, current) = (community.clone(), self.channel.id.clone());
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
                    let id = channel.clone();
                    let open_thread: OpenThread = Arc::new(move |row, _, window, cx| {
                        let channel = id.clone();
                        let _ = weak.update(cx, |this, cx| {
                            this.open_conversation_thread(channel, row.id, window, cx)
                        });
                    });
                    let community = community.clone();
                    TabView::Conversation(cx.new(|cx| {
                        let mut page =
                            ChannelPage::new(community, summary, open_thread, window, cx);
                        page.secondary = true;
                        page
                    }))
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
            let view = self.panel_view(&panel, opening.target.clone(), close, window, cx);
            self.tab_views.insert(id, TabView::Panel(view));
        }
    }

    fn choose_conversation(
        &mut self,
        picker: &str,
        channel: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if usable(&self.community, channel, cx).is_none() || channel == self.channel.id {
            return;
        }
        self.tab_views.remove(picker);
        self.update_tabs(cx, |state| state.choose_conversation(picker, channel));
        self.focus_tab(&format!("conversation:{channel}"), window, cx);
    }

    fn open_conversation_thread(
        &mut self,
        channel: String,
        message: nostr::prelude::EventId,
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

    fn tab_items(
        &mut self,
        state: &TabState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TabItem> {
        self.ensure_tab_views(window, cx);
        let c = *cx.colors();
        let weak = cx.entity().downgrade();
        let closer = |id: String| -> Close {
            let weak = weak.clone();
            Rc::new(move |window, cx| {
                let id = id.clone();
                let _ = weak.update(cx, |this, cx| this.close_tab(&id, window, cx));
            })
        };
        let mut items = Vec::new();
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
                    let _ = weak.update(cx, |this, cx| this.close_thread(window, cx));
                }),
            });
        }
        for tab in &state.tabs {
            let (label, icon): (SharedString, IconName) = match &tab.kind {
                TabKind::New => ("New tab".into(), IconName::Browser),
                TabKind::Conversation { channel } => match usable(&self.community, channel, cx) {
                    Some(summary) => (summary.name.clone().into(), tab_icon(&summary)),
                    None => ("Unavailable conversation".into(), IconName::Browser),
                },
                TabKind::Thread { channel, .. } => match usable(&self.community, channel, cx) {
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
                None => div()
                    .px(rems(1.))
                    .py(rems(2.))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("This conversation is no longer available.")
                    .into_any_element(),
            };
            items.push(TabItem {
                id: tab.id.clone().into(),
                label,
                icon,
                content,
                close: closer(tab.id.clone()),
            });
        }
        for opening in &state.panels {
            let id = opening.id();
            let Some(panel) = channel_panels::get(opening.panel, cx) else {
                continue;
            };
            let content = match self.tab_views.get(&id) {
                Some(TabView::Panel(view)) => view.clone().into_any_element(),
                _ => div().into_any_element(),
            };
            items.push(TabItem {
                id: id.clone().into(),
                label: panel.title.into(),
                icon: panel.icon,
                content,
                close: closer(id),
            });
        }
        items
    }

    /// `SessionHeading` with Me's actions.
    fn heading(
        &mut self,
        showing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let channel = self.channel.clone();
        let mut header = PanelHeader::new()
            .joined(true)
            .label(Some(channel_icon(&channel)), channel.name.clone());
        if channel.archived {
            header = header.action(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("Archived"),
            );
        } else {
            let launchers: Vec<AnyElement> = if channel.read_only {
                Vec::new()
            } else {
                channel_panels::launchers(cx)
                    .into_iter()
                    .map(|(panel, launcher)| {
                        let pressed = self.drawer.as_ref().is_some_and(|d| d.key == panel.key);
                        IconButton::new(
                            SharedString::from(format!("me-launch-{}", panel.key)),
                            launcher.icon,
                        )
                        .size(IconButtonSize::Sm)
                        .tooltip(launcher.label)
                        .selected(pressed)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_launcher(panel.clone(), window, cx)
                        }))
                        .into_any_element()
                    })
                    .collect()
            };
            let share_channel = channel.clone();
            header = header
                .action(self.page.read(cx).header_actions())
                .action(div().flex().gap(rems(0.375)).children(launchers))
                .action(
                    Button::new("me-share", "Share")
                        .size(ButtonSize::Sm)
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let dialog = dialogs::Dialog::share(
                                this.community.clone(),
                                share_channel.clone(),
                                window,
                                cx,
                            );
                            this.dialog = Some(dialog);
                            cx.notify();
                        })),
                );
        }
        let menu_open = self.menu_open;
        let menu = menu_open.then(|| {
            let weak = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.menu_open = false;
                    cx.notify();
                });
            });
            let (id, name) = (channel.id.clone(), channel.name.clone());
            let (settings_id, settings_name) = (id.clone(), name.clone());
            let content = Menu::new()
                .child(
                    MenuItem::new("me-session-rename", "Rename")
                        .icon(IconName::PencilSimple)
                        .icon_size(16.)
                        .disabled(channel.read_only)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_open = false;
                            let dialog = dialogs::Dialog::rename(
                                this.community.clone(),
                                id.clone(),
                                name.clone(),
                                window,
                                cx,
                            );
                            this.dialog = Some(dialog);
                            cx.notify();
                        })),
                )
                .child(
                    MenuItem::new("me-session-settings", "Session settings…")
                        .icon(IconName::Gear)
                        .icon_size(16.)
                        .disabled(channel.read_only)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_open = false;
                            let dialog = dialogs::Dialog::session_settings(
                                this.community.clone(),
                                settings_id.clone(),
                                settings_name.clone(),
                                window,
                                cx,
                            );
                            this.dialog = Some(dialog);
                            cx.notify();
                        })),
                );
            overlay::dropdown(window, Align::End, content, dismiss)
        });
        header
            .action(
                div()
                    .relative()
                    .child(
                        IconButton::new("me-session-actions", IconName::DotsThree)
                            .size(IconButtonSize::Sm)
                            .selected(menu_open)
                            .when(!menu_open, |b| b.tooltip("Session actions"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.menu_open = true;
                                cx.notify();
                            })),
                    )
                    .children(menu),
            )
            .action(
                IconButton::new("me-toggle-tab-pane", IconName::SidebarRight)
                    .size(IconButtonSize::Sm)
                    .tooltip(if showing {
                        "Close tab pane"
                    } else {
                        "Open tab pane"
                    })
                    .selected(showing)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_pane(window, cx))),
            )
            .into_any_element()
    }

    /// Placement notices under the heading.
    fn placement(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let model = self.model.read(cx);
        let session = self.community.read(cx).session.clone();
        if let Some((text, retry)) =
            super::placement_retry("me-work-retry", &model.kit, session.me_kit())
        {
            return Some(notice(text, cx).child(retry).into_any_element());
        }
        if !model.placed(&self.channel.id) {
            let id = self.channel.id.clone();
            return Some(
                notice("This conversation is in Messages.", cx)
                    .child(Button::new("me-work-open", "Open in Messages").on_click(
                        move |_, window, cx| {
                            crate::ui::navigation::open_conversation(id.clone(), window, cx)
                        },
                    ))
                    .text_color(c.text_standard)
                    .into_any_element(),
            );
        }
        None
    }
}

fn notice(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .flex()
        .items_center()
        .gap(rems(0.75))
        .px(rems(1.5))
        .py(rems(0.5))
        .text_body_sm()
        .text_color(cx.colors().text_subtle)
        .child(text.into())
}

/// A conversation a tab may show: joined, writable, not archived, and
/// for sessions only when the viewer is a member.
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

fn tab_icon(channel: &ChannelSummary) -> IconName {
    match channel.channel_type {
        Some(ChannelType::Dm) => IconName::ChatCircle,
        _ => channel_icon(channel),
    }
}

impl DialogHost for SessionWork {
    fn dialog(&mut self) -> &mut Option<dialogs::Dialog> {
        &mut self.dialog
    }
}

impl Render for SessionWork {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.tabs(cx);
        let showing = state.showing();
        let heading = self.heading(showing, window, cx);
        let placement = self.placement(cx);
        let side = self
            .drawer
            .as_ref()
            .filter(|d| d.placement == channel_panels::Placement::Side)
            .map(|d| d.view.clone());
        let bottom = self
            .drawer
            .as_ref()
            .filter(|d| d.placement == channel_panels::Placement::Bottom)
            .map(|d| d.view.clone());
        let pane = showing.then(|| {
            let items = self.tab_items(&state, window, cx);
            let selected = state.selected_tab().unwrap_or_default();
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
        let side = side
            .filter(|_| state.pane_open)
            .map(IntoElement::into_any_element);
        let rows: Vec<AnyElement> = [pane, side].into_iter().flatten().collect();
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
        let conversation = div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(heading)
            .children(placement)
            .child(div().flex().flex_1().min_h_0().child(self.page.clone()))
            .when_some(bottom, |this, view| {
                this.child(
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
            })
            .into_any_element();
        let weak = cx.entity().downgrade();
        let available = f32::from(window.viewport_size().width);
        let board = Dock {
            id: "me-board".into(),
            split: Split::BOARD,
            preferred: self.width,
            window: available,
            available: available * 0.75,
            opening: self.opening,
            on_resize: Rc::new(move |width, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.width = width;
                    cx.notify();
                });
            }),
            on_commit: Rc::new(|_, _| {}),
        }
        .render(conversation, dock, cx);
        div()
            .id("me-session")
            .size_full()
            .flex()
            .child(board)
            .children(
                self.dialog
                    .as_ref()
                    .map(|dialog| dialog.overlay(window, cx)),
            )
    }
}
