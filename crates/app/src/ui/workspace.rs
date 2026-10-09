//! The window: header, community rail and the joined panel holding the
//! sidebar and the current page (ui.md §2).

use super::agents::AgentsPage;
use super::channel::ChannelPage;
use super::onboarding::{Onboarding, Step};
use super::sidebar::{self, Page, SidebarProps};
use super::thread::{self, ThreadPanel};
use crate::community::{AppState, Community, IdentityState};
use buzz_relay_client::ChannelSummary;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, ButtonVariant, IconButton, IconButtonShape, IconButtonSize,
    IconName, MaterialStyled, ShellBackground, Surface, Tab, TabVariant, Tabs, TypeStyled,
};
use gpui_kit::*;
use nostr::prelude::EventId;
use std::sync::Arc;

pub struct Workspace {
    page: Page,
    sidebar_open: bool,
    community: Option<Entity<Community>>,
    channel: Option<Entity<ChannelPage>>,
    thread: Option<Entity<ThreadPanel>>,
    agents: Option<Entity<AgentsPage>>,
    onboarding: Option<(Step, Entity<Onboarding>)>,
    _community_observer: Option<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_global_in::<AppState>(window, |this, window, cx| this.sync(window, cx))
            .detach();
        let mut workspace = Self {
            page: Page::Messages,
            sidebar_open: true,
            community: None,
            channel: None,
            thread: None,
            agents: None,
            onboarding: None,
            _community_observer: None,
        };
        workspace.sync(window, cx);
        workspace
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
        }
        let current = AppState::current(cx);
        if current.as_ref().map(Entity::entity_id) != self.community.as_ref().map(Entity::entity_id)
        {
            self.channel = None;
            self.thread = None;
            self._community_observer = current.as_ref().map(|community| {
                cx.observe_in(community, window, |this, _, window, cx| {
                    this.restore_selection(window, cx);
                    cx.notify();
                })
            });
            self.community = current;
        }
        cx.notify();
    }

    /// Reopens the channel last selected in this community once it is
    /// known to the session.
    fn restore_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.channel.is_some() || self.page != Page::Messages {
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

    fn select_channel(
        &mut self,
        channel: &ChannelSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.page = Page::Messages;
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
        self.thread = None;
        let weak = cx.entity().downgrade();
        let open_thread: super::channel::OpenThread = Arc::new(move |row, window, cx| {
            let _ = weak.update(cx, |this, cx| this.open_thread(row.id, window, cx));
        });
        let page =
            cx.new(|cx| ChannelPage::new(community, channel.clone(), open_thread, window, cx));
        let composer = page.read(cx).composer();
        composer.update(cx, |state, cx| state.focus(window, cx));
        self.channel = Some(page);
        if let Some(root) = thread::dev_open_thread() {
            self.open_thread(root, window, cx);
        }
        cx.notify();
    }

    fn open_thread(&mut self, root: EventId, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(community), Some(channel)) = (self.community.clone(), &self.channel) else {
            return;
        };
        if let Some(thread) = &self.thread
            && thread.read(cx).root_id() == root
        {
            ThreadPanel::focus(thread, window, cx);
            return;
        }
        let channel = channel.read(cx).channel_id().to_string();
        let weak = cx.entity().downgrade();
        let on_close: thread::OnClose = Arc::new(move |window, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.thread = None;
                if let Some(channel) = &this.channel {
                    let composer = channel.read(cx).composer();
                    composer.update(cx, |state, cx| state.focus(window, cx));
                }
                cx.notify();
            });
        });
        let panel = cx.new(|cx| ThreadPanel::new(community, channel, root, on_close, window, cx));
        ThreadPanel::focus(&panel, window, cx);
        self.thread = Some(panel);
        cx.notify();
    }

    fn select_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        if page == Page::Agents && self.agents.is_none() {
            self.agents = Some(cx.new(AgentsPage::new));
        }
        if page != Page::Messages
            && let Some(community) = &self.community
        {
            community.read(cx).session.select(None);
        }
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let header_button = |id: &'static str, icon: IconName| {
            IconButton::new(id, icon)
                .size(IconButtonSize::Sm)
                .surface(Surface::Header)
        };
        let viewer = match &cx.global::<AppState>().identity {
            IdentityState::Ready(identity) => identity.pubkey_hex(),
            _ => String::new(),
        };
        let me = self
            .community
            .as_ref()
            .and_then(|community| community.read(cx).snapshot.profiles.get(&viewer).cloned());
        let mut avatar = Avatar::new(me.as_ref().map_or("?".to_string(), |p| p.name.clone()))
            .size(AvatarSize::Px(32.))
            .profile_disc();
        if let Some(picture) = me.and_then(|p| p.picture.clone()) {
            avatar = avatar.image(picture);
        }
        div()
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .h(rems(3.))
            .pl(rems(1.))
            .pr(rems(1.))
            .child(
                div()
                    .flex()
                    .pl(rems(1.))
                    .child(header_button("toggle-sidebar", IconName::Sidebar).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.sidebar_open = !this.sidebar_open;
                            cx.notify();
                        }),
                    ))
                    .child(header_button("back", IconName::ArrowLeft).disabled(true))
                    .child(header_button("forward", IconName::ArrowRight).disabled(true)),
            )
            .child(
                div()
                    .absolute()
                    .top(rems(0.25))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        Tabs::new(TabVariant::Chrome)
                            .tab(Tab::new("me", "Me"))
                            .tab(Tab::new("messages", "Messages").selected(true)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(rems(0.5))
                    .child(header_button("search", IconName::MagnifyingGlass))
                    .child(
                        IconButton::new("me", IconName::User)
                            .size(IconButtonSize::Sm)
                            .variant(ButtonVariant::Avatar)
                            .shape(IconButtonShape::Round)
                            .child(avatar),
                    ),
            )
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
            IconButton::new("add-community", IconName::Plus).on_click(cx.listener(
                |this, _, window, cx| {
                    this.onboarding = Some((
                        Step::Community,
                        cx.new(|cx| Onboarding::new(Step::Community, window, cx)),
                    ));
                    cx.notify();
                },
            )),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let main: AnyElement = if let Some((_, onboarding)) = &self.onboarding {
            onboarding.clone().into_any_element()
        } else {
            match self.page {
                Page::Agents => self
                    .agents
                    .clone()
                    .map(IntoElement::into_any_element)
                    .unwrap_or_else(|| div().into_any_element()),
                Page::Messages => match &self.channel {
                    Some(channel) => {
                        thread::board(channel.clone().into_any_element(), self.thread.clone(), cx)
                    }
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
            }
        };
        let sidebar = (self.sidebar_open && self.onboarding.is_none())
            .then(|| self.community.clone())
            .flatten()
            .map(|community| {
                let snapshot = community.read(cx).snapshot.clone();
                let weak = cx.entity().downgrade();
                let weak_page = weak.clone();
                sidebar::render(
                    &snapshot,
                    SidebarProps {
                        page: self.page,
                        selected: self
                            .channel
                            .as_ref()
                            .map(|c| c.read(cx).channel_id().to_string()),
                        on_select_channel: Arc::new(move |channel, window, cx| {
                            let _ = weak
                                .update(cx, |this, cx| this.select_channel(channel, window, cx));
                        }),
                        on_select_page: Arc::new(move |page, _, cx| {
                            let _ = weak_page.update(cx, |this, cx| this.select_page(page, cx));
                        }),
                    },
                    cx,
                )
            });
        ShellBackground::new().child(
            div()
                .flex()
                .flex_col()
                .size_full()
                .font_family("Inter")
                .text_color(c.text_standard)
                .child(self.header(cx))
                .child(
                    div().flex().flex_1().min_h_0().child(self.rail(cx)).child(
                        div().flex_1().min_w_0().pr(rems(1.)).pb(rems(1.)).child(
                            div()
                                .flex()
                                .size_full()
                                .overflow_hidden()
                                .panel_surface(&c)
                                .children(sidebar)
                                .child(main),
                        ),
                    ),
                ),
        )
    }
}
