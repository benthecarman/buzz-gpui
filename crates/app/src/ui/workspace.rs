//! The window: header, community rail and the joined panel holding the
//! sidebar and the current page (ui.md §2).

mod board;
mod companion;
mod dialogs;
pub mod layout;
mod shell;

use super::account::Account;
use super::channel::{ChannelClosed, ChannelPage, Reveal};
use super::channel_tabs::{TabState, ThreadTarget};
use super::navigation::{History, Place, Request, Target};
use super::onboarding::{Onboarding, Step};
use super::pages::{self, Page};
use super::profile_card::{self, ProfileCard};
use super::settings::{CloseSettings, OpenHostSettings, OpenSettings, Section, SettingsPage};
use super::sidebar::SidebarView;
use super::thread::{self, ThreadPanel};
use crate::community::{AppState, Community, IdentityState};
use crate::deep_links::{self, Link};
use buzz_relay_client::{ChannelSummary, ChannelType, RosterStatus};
use buzz_ui::{ActiveTheme, Avatar, AvatarSize, IconButton, IconName, ShellBackground, TypeStyled};
use gpui_kit::component::window_border;
use gpui_kit::*;
use nostr::prelude::EventId;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub struct Workspace {
    page: Page,
    /// The header toggle's choice on a wide window.
    sidebar_open: bool,
    sidebar_motion: Option<shell::Motion>,
    /// On a narrow window the sidebar opens as a drawer over the page.
    drawer_open: bool,
    /// The current community's saved sidebar view (its width).
    sidebar_view: SidebarView,
    /// The header pills fold into More pages when they don't fit.
    compact_pages: bool,
    header_sizes: Rc<Cell<shell::HeaderSizes>>,
    overflow_open: bool,
    toggle_focus: FocusHandle,
    /// The page region, focused after a page change.
    main_focus: FocusHandle,
    /// Each channel's tab pane, per community, kept across visits.
    tab_states: HashMap<(EntityId, String), TabState>,
    /// The open channel's tab views, made on this visit.
    tab_views: HashMap<String, board::TabView>,
    tab_strip: ScrollHandle,
    /// A channel panel a header launcher opened, for this visit.
    drawer: Option<board::Drawer>,
    /// The tab last scrolled into view.
    revealed_tab: Option<String>,
    /// The tab pane's width once dragged, kept while it is closed.
    board_width: Option<f32>,
    pane_opening: u64,
    companion: Option<companion::Companion>,
    companion_width: Option<f32>,
    companion_opening: u64,
    community: Option<Entity<Community>>,
    channel: Option<Entity<ChannelPage>>,
    thread: Option<Entity<ThreadPanel>>,
    /// Registered pages' views, made on first visit and kept.
    views: HashMap<Page, AnyView>,
    /// Pages' own sidebars, such as Me's.
    own_sidebars: HashMap<Page, AnyView>,
    settings: Option<Entity<SettingsPage>>,
    account: Entity<Account>,
    onboarding: Option<(Step, Entity<Onboarding>)>,
    _community_observer: Option<Subscription>,
    /// Places visited, for Back and Forward.
    history: History,
    /// A place waiting for its community's channel list.
    pending: Option<Target>,
    /// The pending place is a conversation just opened, so it waits for
    /// the channel to be listed even once the roster has loaded.
    pending_listing: bool,
    /// The thread panel reveals a message rather than showing a thread.
    thread_exact: bool,
    /// Waits for the channel's first page to reveal a message in it.
    _revealing: Option<Subscription>,
    /// The person whose card a mention opened, and where.
    profile: Option<(String, Point<Pixels>)>,
    dialog: Option<dialogs::OpenDialog>,
    _page_events: Option<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_global_in::<AppState>(window, |this, window, cx| this.sync(window, cx))
            .detach();
        let mut workspace = Self {
            page: Page::MESSAGES,
            sidebar_open: true,
            sidebar_motion: None,
            drawer_open: false,
            sidebar_view: SidebarView::default(),
            compact_pages: layout::narrow(f32::from(window.viewport_size().width)),
            header_sizes: shell::header_sizes(),
            overflow_open: false,
            toggle_focus: cx.focus_handle(),
            main_focus: cx.focus_handle(),
            tab_states: HashMap::new(),
            tab_views: HashMap::new(),
            tab_strip: ScrollHandle::new(),
            drawer: None,
            revealed_tab: None,
            board_width: None,
            pane_opening: 0,
            companion: None,
            companion_width: None,
            companion_opening: 0,
            community: None,
            channel: None,
            thread: None,
            views: HashMap::new(),
            own_sidebars: HashMap::new(),
            settings: None,
            account: cx.new(Account::new),
            onboarding: None,
            _community_observer: None,
            history: History::default(),
            pending: None,
            pending_listing: false,
            thread_exact: false,
            _revealing: None,
            profile: None,
            dialog: None,
            _page_events: None,
        };
        workspace.handle_settings_actions(window, cx);
        workspace.sync(window, cx);
        workspace
    }

    /// Settings actions come from menus, other pages and Mod+, (bound in
    /// `main`), whatever has focus, so they are handled app-wide. They are
    /// dispatched while the window is borrowed, so the work is deferred.
    fn handle_settings_actions(&self, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let weak = cx.entity().downgrade();
        let run = move |cx: &mut App, f: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let weak = weak.clone();
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = weak.update(cx, |this, cx| f(this, window, cx));
                });
            });
        };
        let (open_host, close) = (run.clone(), run.clone());
        let app: &mut App = cx;
        app.on_action(move |_: &OpenSettings, cx| {
            run(cx, |this, window, cx| this.open_settings(None, window, cx))
        });
        app.on_action(move |_: &OpenHostSettings, cx| {
            open_host(cx, |this, window, cx| {
                this.open_settings(Some(Section::Agents), window, cx)
            })
        });
        // Settings' Back row returns to the previous view.
        app.on_action(move |_: &CloseSettings, cx| {
            close(cx, |this, window, cx| {
                if this.history.can_go_back() {
                    this.go_back(window, cx);
                } else {
                    this.go_home(window, cx);
                }
            })
        });
    }

    fn open_settings(
        &mut self,
        section: Option<Section>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Without a community Settings still opens, as buzz-app's Personal
        // space allows; without an identity it does not.
        if matches!(self.onboarding, Some((Step::Identity, _))) {
            return;
        }
        match &self.settings {
            Some(settings) => {
                if let Some(section) = section {
                    settings.update(cx, |settings, cx| settings.open(section, cx));
                }
            }
            None => self.settings = Some(cx.new(|cx| SettingsPage::new(section, window, cx))),
        }
        if self.page != Page::SETTINGS {
            self.select_page(Page::SETTINGS, window, cx);
        }
    }

    /// History can return to Settings after the page was closed, so it is
    /// made on demand.
    fn settings_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SettingsPage> {
        self.settings
            .get_or_insert_with(|| cx.new(|cx| SettingsPage::new(None, window, cx)))
            .clone()
    }

    /// Follows the selected community and the onboarding state.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.global::<AppState>();
        let step = match &state.identity {
            IdentityState::Ready(_) if state.communities.is_empty() => Some(Step::Community),
            IdentityState::Ready(_) => None,
            _ => Some(Step::Identity),
        };
        if self.onboarding.as_ref().map(|(s, _)| *s) != step {
            self.onboarding =
                step.map(|step| (step, cx.new(|cx| Onboarding::new(step, window, cx))));
            // Signed out: start over on Messages after the next sign-in.
            if step == Some(Step::Identity) {
                self.page = Page::MESSAGES;
                self.settings = None;
            }
        }
        let current = AppState::current(cx);
        if current.as_ref().map(Entity::entity_id) != self.community.as_ref().map(Entity::entity_id)
        {
            self.channel = None;
            self.leave_channel_tabs();
            self.companion = None;
            self._community_observer = current.as_ref().map(|community| {
                cx.observe_in(community, window, |this, _, window, cx| {
                    this.restore_selection(window, cx);
                    cx.notify();
                })
            });
            self.community = current;
            self.load_sidebar_view(cx);
        }
        cx.notify();
    }

    /// Reopens the channel last selected in this community once it is
    /// known to the session.
    fn restore_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.apply_pending(window, cx) {
            return;
        }
        if self.channel.is_some() || self.page != Page::MESSAGES {
            return;
        }
        let Some(community) = &self.community else {
            return;
        };
        let remembered = community.read(cx).remembered_channel(cx);
        let channel = remembered.and_then(|id| {
            community
                .read(cx)
                .snapshot
                .channels
                .iter()
                .find(|channel| channel.id == id)
                .cloned()
        });
        if let Some(channel) = channel {
            self.select_channel(&channel, window, cx);
        }
    }

    /// Opens a channel and records the visit.
    pub fn select_channel(
        &mut self,
        channel: &ChannelSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_channel(channel, window, cx);
        self.record(cx);
    }

    fn show_channel(
        &mut self,
        channel: &ChannelSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.page = Page::MESSAGES;
        if self
            .channel
            .as_ref()
            .is_some_and(|page| page.read(cx).channel_id() == channel.id)
        {
            cx.notify();
            return;
        }
        let Some(community) = self.community.clone() else {
            return;
        };
        community.read(cx).remember_channel(&channel.id, cx);
        self.leave_channel_tabs();
        self._revealing = None;
        let weak = cx.entity().downgrade();
        let open_thread: super::channel::OpenThread = Arc::new(move |row, window, cx| {
            let _ = weak.update(cx, |this, cx| this.open_thread(row.id, window, cx));
        });
        let page =
            cx.new(|cx| ChannelPage::new(community, channel.clone(), open_thread, window, cx));
        let composer = page.read(cx).composer();
        composer.update(cx, |state, cx| state.focus(window, cx));
        self._page_events = Some(cx.subscribe_in(
            &page,
            window,
            |this, _, _: &ChannelClosed, window, cx| this.close_channel(window, cx),
        ));
        self.channel = Some(page);
        self.restore_channel_tabs(window, cx);
        if let Some(root) = thread::dev_open_thread() {
            self.open_thread(root, window, cx);
        }
        cx.notify();
    }

    /// Opens a thread beside the channel and records the visit.
    pub fn open_thread(&mut self, root: EventId, window: &mut Window, cx: &mut Context<Self>) {
        self.show_thread(root, false, window, cx);
        self.record(cx);
    }

    /// With `exact`, `id` is any message, revealed in its thread.
    fn show_thread(
        &mut self,
        id: EventId,
        exact: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.channel.is_none() {
            return;
        }
        if let Some(thread) = &self.thread
            && thread.read(cx).root_id() == id
            && self.thread_exact == exact
        {
            let thread = thread.clone();
            self.update_tabs(cx, |state| state.select(super::channel_tabs::THREAD));
            ThreadPanel::focus(&thread, window, cx);
            return;
        }
        self.open_thread_tab(ThreadTarget { root: id, exact }, window, cx);
        cx.notify();
    }

    fn close_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had_tab = self.tabs(cx).thread.is_some();
        self.update_tabs(cx, |state| state.close_thread());
        if self.thread.take().is_none() && !had_tab {
            return;
        }
        if let Some(channel) = &self.channel {
            let composer = channel.read(cx).composer();
            composer.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    /// Shows a page and records the visit.
    pub fn select_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.show_page(page, window, cx);
        self.record(cx);
    }

    fn show_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        if let Some(spec) = pages::spec(page, cx) {
            if let Some(build) = spec.build
                && !self.views.contains_key(&page)
            {
                let view = build(window, cx);
                self.views.insert(page, view);
            }
            if let pages::Navigation::Own(build) = spec.navigation
                && !self.own_sidebars.contains_key(&page)
            {
                let view = build(window, cx);
                self.own_sidebars.insert(page, view);
            }
        }
        if page != Page::SETTINGS {
            self.settings = None;
        }
        if page != Page::MESSAGES
            && let Some(community) = &self.community
        {
            community.read(cx).session.select(None);
        }
        // Focus moves to the page, as buzz-app's `#main-content` does; the
        // channel page focuses its composer instead.
        if page != Page::MESSAGES || self.channel.is_none() {
            window.focus(&self.main_focus, cx);
        }
        cx.notify();
    }

    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = cx.global::<AppState>();
        let selected = state.selected;
        let mut rail = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(rems(0.5))
            .w(rems(3.))
            .py(rems(0.5));
        for (index, community) in state.communities.iter().enumerate() {
            let name = community.read(cx).name();
            rail = rail.child(
                IconButton::new(
                    SharedString::from(format!("community-{index}")),
                    IconName::Globe,
                )
                .selected(selected == Some(index))
                .child(Avatar::new(name).squircle().size(AvatarSize::Small))
                .on_click(move |_, _, cx| AppState::select(index, cx)),
            );
        }
        rail.child(
            IconButton::new("add-community", IconName::Plus)
                .tooltip("Add a community")
                .on_click(cx.listener(|this, _, window, cx| this.open_add_community(window, cx))),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let dialog = self.dialog(window, cx);
        let main: AnyElement = if let Some((_, onboarding)) = &self.onboarding
            && self.page != Page::SETTINGS
        {
            onboarding.clone().into_any_element()
        } else {
            match self.page {
                Page::SETTINGS => self.settings_page(window, cx).into_any_element(),
                Page::MESSAGES => match &self.channel {
                    Some(_) => self.board(window, cx),
                    None => div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child("Select a channel to read it.")
                        .into_any_element(),
                },
                page => self.page_view(page),
            }
        };
        // Messages stacks the companion under its tab pane; other pages
        // get the shell's dock.
        let main = match self.page == Page::MESSAGES && self.channel.is_some() {
            true => main,
            false => self.frame(main, window, cx),
        };
        let profile = self.profile_card(window, cx);
        let header = self.header(window, cx);
        let body = self.body(main, window, cx);
        let gutter = layout::gutter(f32::from(window.viewport_size().width));
        // No shadow around the frame: that needs a translucent surface,
        // which not every GPU path gives; edges resize from just inside.
        window_border().shadow_size(px(0.)).child(
            ShellBackground::new().child(
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .on_mouse_down(
                        MouseButton::Navigate(NavigationDirection::Back),
                        cx.listener(|this, _, window, cx| this.go_back(window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Navigate(NavigationDirection::Forward),
                        cx.listener(|this, _, window, cx| this.go_forward(window, cx)),
                    )
                    .children(profile)
                    .font_family("Inter")
                    .emoji_fallback()
                    .text_color(c.text_standard)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        let keys = &event.keystroke;
                        if keys.key == "k" && keys.modifiers.secondary() && this.dialog.is_none() {
                            cx.stop_propagation();
                            this.open_search(window, cx);
                        }
                    }))
                    .child(header)
                    .child(
                        div().flex().flex_1().min_h_0().child(self.rail(cx)).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .pr(px(gutter))
                                .pb(px(gutter))
                                .child(body),
                        ),
                    )
                    .children(dialog)
                    .children(super::media::review_overlay(cx)),
            ),
        )
    }
}

// Navigation: history, links and requests from outside the window.
impl Workspace {
    /// Where the window is now, as history records it.
    fn here(&self, cx: &App) -> Option<Target> {
        if self.onboarding.is_some() {
            return None;
        }
        let community = self
            .community
            .as_ref()
            .map(|community| community.read(cx).origin.to_string());
        let place = match (self.page, &self.channel) {
            (Page::MESSAGES, Some(channel)) => {
                let channel = channel.read(cx).channel_id().to_string();
                let thread = self.thread.as_ref().map(|thread| thread.read(cx).root_id());
                Place::Conversation {
                    channel,
                    message: thread,
                    thread: thread.filter(|_| !self.thread_exact),
                }
            }
            (page, _) => Place::Page(page),
        };
        Some(Target { community, place })
    }

    /// A registered page's view. Every bundled page uses buzz-app's
    /// workspace layout and owns its padding; document layout is only for
    /// external plugins, which are skipped.
    fn page_view(&self, page: Page) -> AnyElement {
        self.views
            .get(&page)
            .cloned()
            .map(IntoElement::into_any_element)
            .unwrap_or_else(|| div().into_any_element())
    }

    /// Records the place the user just went to.
    fn record(&mut self, cx: &mut Context<Self>) {
        // Any navigation closes the narrow drawer.
        self.drawer_open = false;
        if let Some(here) = self.here(cx) {
            self.history.push(here);
        }
        cx.notify();
    }

    /// Goes to `target` as a new visit.
    pub fn navigate(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.history.push(target.clone());
        self.apply(target, window, cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.history.back() {
            self.apply(target, window, cx);
        }
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.history.forward() {
            self.apply(target, window, cx);
        }
    }

    /// The Messages page, on the channel last open there.
    pub fn go_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_page(Page::MESSAGES, window, cx);
        if self.channel.is_none() {
            self.restore_selection(window, cx);
        }
        self.record(cx);
    }

    /// Shows `target` without recording it.
    fn apply(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.drawer_open = false;
        self.pending = None;
        self.pending_listing = false;
        self._revealing = None;
        self.profile = None;
        if let Some(origin) = &target.community {
            let state = cx.global::<AppState>();
            let index = state
                .communities
                .iter()
                .position(|community| community.read(cx).origin.to_string() == *origin);
            let Some(index) = index else {
                tracing::warn!(%origin, "that community is no longer saved");
                cx.notify();
                return;
            };
            if state.selected != Some(index) {
                AppState::select(index, cx);
                self.sync(window, cx);
            }
        }
        match target.place {
            Place::Page(page) => {
                self.show_page(page, window, cx);
                if page == Page::MESSAGES && self.channel.is_none() {
                    self.restore_selection(window, cx);
                }
            }
            Place::Conversation {
                ref channel,
                message,
                thread,
            } => {
                let Some(community) = &self.community else {
                    return;
                };
                let snapshot = community.read(cx).snapshot.clone();
                let Some(summary) = snapshot.channels.iter().find(|c| c.id == *channel) else {
                    // A link can arrive before the channel list does.
                    if snapshot.roster == RosterStatus::Ready {
                        tracing::warn!(%channel, "that channel is not available here");
                    } else {
                        self.pending = Some(target.clone());
                    }
                    cx.notify();
                    return;
                };
                self.show_channel(summary, window, cx);
                match (message, thread) {
                    (Some(message), Some(thread)) if message == thread => {
                        self.show_thread(message, false, window, cx)
                    }
                    (Some(message), _) => self.reveal(message, window, cx),
                    (None, _) => self.close_thread(window, cx),
                }
            }
        }
        cx.notify();
    }

    /// Applies a place that was waiting for its channel list. True while
    /// it still waits or once it opened, so the remembered channel does
    /// not replace it.
    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.pending.clone() else {
            return false;
        };
        let Some(community) = &self.community else {
            return false;
        };
        let community = community.read(cx);
        // The user moved on to another community.
        if target.community.as_deref() != Some(&community.origin.to_string()) {
            self.pending = None;
            return false;
        }
        let snapshot = community.snapshot.clone();
        let Place::Conversation { channel, .. } = &target.place else {
            return false;
        };
        let known = snapshot.channels.iter().any(|c| c.id == *channel);
        if !known && (snapshot.roster != RosterStatus::Ready || self.pending_listing) {
            return true;
        }
        // Either opens it or gives up on it.
        self.apply(target, window, cx);
        known
    }

    /// Opens a conversation the session has just made or joined, once the
    /// community's channel list includes it.
    pub fn open_conversation(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = &self.community else {
            return;
        };
        let community = community.read(cx);
        let listed = community.snapshot.channels.iter().any(|c| c.id == id);
        let target = Target::channel(Some(community.origin.to_string()), id);
        if listed {
            self.navigate(target, window, cx);
        } else {
            self.pending = Some(target);
            self.pending_listing = true;
        }
    }

    /// Opens, or reopens, the direct message with one person.
    fn message_person(&mut self, pubkey: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = &self.community else {
            return;
        };
        let session = community.read(cx).session.clone();
        cx.spawn_in(window, async move |this, cx| {
            match session.open_dm(vec![pubkey]).await {
                Ok(id) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.open_conversation(id, window, cx)
                    });
                }
                Err(error) => tracing::warn!(%error, "could not open the direct message"),
            }
        })
        .detach();
    }

    /// Reveals a message in the open channel: in the timeline when it is a
    /// loaded top-level message, else in the thread panel in exact mode
    /// (relay.md §5c.5).
    fn reveal(&mut self, message: EventId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.channel.clone() else {
            return;
        };
        let outcome = page.update(cx, |page, cx| page.reveal(message, cx));
        self.revealed(outcome, message, window, cx);
        if outcome == Reveal::Loading {
            self._revealing = Some(cx.observe_in(&page, window, move |this, page, window, cx| {
                let outcome = page.update(cx, |page, cx| page.reveal(message, cx));
                this.revealed(outcome, message, window, cx);
            }));
        }
    }

    fn revealed(
        &mut self,
        outcome: Reveal,
        message: EventId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Reveal::Loading => return,
            Reveal::Shown => self.close_thread(window, cx),
            Reveal::Absent => self.show_thread(message, true, window, cx),
        }
        self._revealing = None;
    }

    /// A `buzz://` link: conversations bind to the selected community;
    /// invitations add theirs. Web links go to the browser.
    pub fn open_url(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.route_link_to_tab(url, window, cx) {
            return;
        }
        self.open_link(url, window, cx);
    }

    /// A link from outside the window, or one no tab takes.
    fn open_link(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        match deep_links::parse(url) {
            Some(Link::Conversation {
                channel,
                message,
                thread,
            }) => {
                let Some(community) = &self.community else {
                    tracing::warn!("select a community before opening a Buzz link");
                    return;
                };
                let community = Some(community.read(cx).origin.to_string());
                let target = Target {
                    community,
                    place: Place::Conversation {
                        channel,
                        message,
                        thread,
                    },
                };
                self.navigate(target, window, cx);
            }
            Some(Link::Join { .. }) => self.open_invite(url, window, cx),
            None if url.starts_with("https://") || url.starts_with("http://") => cx.open_url(url),
            None => tracing::warn!("ignored a link this app cannot open"),
        }
    }

    /// Something from outside the window, which is already in front.
    pub fn handle(&mut self, request: Request, window: &mut Window, cx: &mut Context<Self>) {
        match request {
            Request::Focus => {}
            Request::Url(url) => self.open_link(&url, window, cx),
            Request::Open(target) => self.navigate(target, window, cx),
        }
    }

    pub fn show_profile(
        &mut self,
        pubkey: String,
        at: Point<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.profile = Some((pubkey, at));
        cx.notify();
    }

    /// The card a mention opened, kept inside the window.
    fn profile_card(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pubkey, at) = self.profile.clone()?;
        let community = self.community.as_ref()?.read(cx);
        let snapshot = community.snapshot.clone();
        let viewer = community.session.viewer().to_string();
        let media = community.session.media().clone();
        let origin = community.origin.to_string();
        // The DM with just this person, or notes to self.
        let dm = snapshot
            .channels
            .iter()
            .find(|channel| {
                channel.channel_type == Some(ChannelType::Dm)
                    && match channel.participants.as_deref() {
                        Some([]) => pubkey == viewer,
                        Some([other]) => *other == pubkey,
                        _ => false,
                    }
            })
            .map(|channel| channel.id.clone())
            // A hidden DM is reopened through the relay, which unhides it
            // (`ProfilePanel.tsx`). Notes to self cannot be reopened so.
            .filter(|dm| pubkey == viewer || !snapshot.hidden_dms.contains(dm));
        let weak = cx.entity().downgrade();
        let on_message: Option<profile_card::OnMessage> = match dm {
            Some(dm) => Some(Rc::new(move |window, cx| {
                let target = Target::channel(Some(origin.clone()), dm.clone());
                let _ = weak.update(cx, |this, cx| this.navigate(target, window, cx));
            })),
            // Notes to self are not opened on request.
            None if pubkey == viewer => None,
            None => {
                let pubkey = pubkey.clone();
                Some(Rc::new(move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.profile = None;
                        this.message_person(pubkey.clone(), window, cx);
                    });
                }))
            }
        };
        let card = ProfileCard {
            pubkey: &pubkey,
            profile: snapshot.profiles.get(&pubkey).map(|p| &**p),
            media: &media,
            on_message,
        }
        .render(cx);
        let viewport = window.viewport_size();
        let x =
            at.x.min(viewport.width - profile_card::WIDTH - px(8.))
                .max(px(8.));
        Some(
            deferred(
                anchored()
                    .position_mode(AnchoredPositionMode::Window)
                    .position(point(x, at.y + px(8.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("profile-card")
                            .occlude()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.profile = None;
                                cx.notify();
                            }))
                            .child(card),
                    ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}
