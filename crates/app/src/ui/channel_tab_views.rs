//! The views behind a conversation's tab pane, shared by the Messages
//! board and a conversation open in Me: made when a tab first shows on a
//! visit, and dropped with it.

use crate::community::Community;
use crate::ui::channel::{ChannelPage, OpenThread};
use crate::ui::channel_panels::{self, ChannelPanel};
use crate::ui::channel_settings::ChannelSettingsPanel;
use crate::ui::channel_tab_picker::{ChannelTabPicker, ChooseChannel, ChooseTool, Tool};
use crate::ui::channel_tabs::{PanelTab, TabKind, TabState};
use crate::ui::message::channel_icon;
use crate::ui::thread::{self, ThreadPanel};
use buzz_relay_client::{ChannelSummary, ChannelType};
use buzz_ui::IconName;
use gpui_kit::*;
use nostr::prelude::EventId;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// A tab's view, made when the tab first shows on this visit.
pub enum TabView {
    /// The channel's settings; only the Messages board has this tab.
    Settings {
        panel: Entity<ChannelSettingsPanel>,
        _subscription: Subscription,
    },
    Picker(Entity<ChannelTabPicker>),
    Conversation(Entity<ChannelPage>),
    Thread(Entity<ThreadPanel>),
    Panel(AnyView),
}

/// A view that shows a conversation with its tab pane.
pub trait TabHost: Sized + 'static {
    fn community(&self) -> Option<Entity<Community>>;
    /// The conversation the pane belongs to.
    fn channel_id(&self, cx: &App) -> Option<String>;
    fn tab_views(&mut self) -> &mut HashMap<String, TabView>;
    fn tabs(&self, cx: &App) -> TabState;
    /// Changes the conversation's tabs, then drops the views of tabs that
    /// closed.
    fn update_tabs(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut TabState));
    fn close_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>);
    fn focus_tab(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>);
    /// The channel's tools: registered channel panels offered under Tools.
    fn tools(&self, cx: &App) -> Vec<Tool>;
    /// Makes a registered panel's view, opened from `channel` to show
    /// `target` and closed through `close`.
    fn panel_view(
        &self,
        panel: &ChannelPanel,
        channel: String,
        target: String,
        close: channel_panels::Close,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyView>;
    /// Puts away the panel opened from a header launcher.
    fn close_drawer(&mut self);
}

/// Makes the views of tabs that have none yet.
pub fn ensure_views<T: TabHost>(this: &mut T, window: &mut Window, cx: &mut Context<T>) {
    let (Some(community), Some(current)) = (this.community(), this.channel_id(cx)) else {
        return;
    };
    let state = this.tabs(cx);
    for tab in &state.tabs {
        if this.tab_views().contains_key(&tab.id) {
            continue;
        }
        let view = match &tab.kind {
            TabKind::New => {
                let picker_id = tab.id.clone();
                let weak = cx.entity().downgrade();
                let choose: ChooseChannel = Rc::new(move |channel, window, cx| {
                    let picker = picker_id.clone();
                    let _ = weak.update(cx, |this, cx| {
                        choose_conversation(this, &picker, &channel, window, cx)
                    });
                });
                let picker_id = tab.id.clone();
                let weak = cx.entity().downgrade();
                let choose_tool: ChooseTool = Rc::new(move |key, _, cx| {
                    let picker = picker_id.clone();
                    let _ = weak.update(cx, |this, cx| {
                        let channel = this.channel_id(cx).unwrap_or_default();
                        this.tab_views().remove(&picker);
                        // A tool has one place on screen at a time.
                        this.close_drawer();
                        this.update_tabs(cx, |state| {
                            state.choose_tool(&picker, PanelTab::tool(key, &channel))
                        });
                    });
                });
                let tools = this.tools(cx);
                let (community, current) = (community.clone(), current.clone());
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
                        open_conversation_thread(this, channel, row.id, window, cx)
                    });
                });
                let community = community.clone();
                TabView::Conversation(cx.new(|cx| {
                    let mut page = ChannelPage::new(community, summary, open_thread, window, cx);
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
                let (community, channel, message) = (community.clone(), channel.clone(), *message);
                TabView::Thread(
                    cx.new(|cx| {
                        ThreadPanel::new(community, channel, message, on_close, window, cx)
                    }),
                )
            }
        };
        this.tab_views().insert(tab.id.clone(), view);
    }
    for opening in &state.panels {
        let id = opening.id();
        if this.tab_views().contains_key(&id) {
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
        if let Some(view) = this.panel_view(
            &panel,
            opening.channel.clone(),
            opening.target.clone(),
            close,
            window,
            cx,
        ) {
            this.tab_views().insert(id, TabView::Panel(view));
        }
    }
}

/// Swaps the New tab `picker` for the conversation `channel`.
fn choose_conversation<T: TabHost>(
    this: &mut T,
    picker: &str,
    channel: &str,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    // Membership is checked again when chosen, not just when listed.
    let Some(community) = this.community() else {
        return;
    };
    if usable(&community, channel, cx).is_none() || this.channel_id(cx).as_deref() == Some(channel)
    {
        return;
    }
    this.tab_views().remove(picker);
    this.update_tabs(cx, |state| state.choose_conversation(picker, channel));
    this.focus_tab(&format!("conversation:{channel}"), window, cx);
}

/// Opens a thread of another conversation as its own tab.
fn open_conversation_thread<T: TabHost>(
    this: &mut T,
    channel: String,
    message: EventId,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let id = format!("thread:{channel}:{}", message.to_hex());
    this.tab_views().remove(&id);
    this.update_tabs(cx, |state| {
        state.open_conversation_thread(&channel, message, message)
    });
    this.focus_tab(&id, window, cx);
}

/// The conversation `id` when a tab may show it: joined, writable, not
/// archived, and for sessions only when the viewer is a member.
pub fn usable(community: &Entity<Community>, id: &str, cx: &App) -> Option<ChannelSummary> {
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
pub fn tab_icon(channel: &ChannelSummary) -> IconName {
    match channel.channel_type {
        Some(ChannelType::Dm) => IconName::ChatCircle,
        _ => channel_icon(channel),
    }
}
