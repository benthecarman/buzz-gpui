//! The workspace's dialogs (search, create channel, new message, add a
//! community) and opening a channel at a message.

use super::Workspace;
use crate::ui::add_community::AddCommunity;
use crate::ui::modal::{Callback, Dismissed, OpenChannel};
use crate::ui::new_channel::NewChannelDialog;
use crate::ui::new_message::NewMessageDialog;
use crate::ui::overlay::{self, Dismiss};
use crate::ui::search::SearchDialog;
use crate::ui::sidebar::Page;
use buzz_relay_client::{ChannelSummary, EventId};
use gpui_kit::*;
use std::rc::Rc;

/// The dialog open over the workspace.
pub(super) struct OpenDialog {
    view: AnyView,
    /// The search palette draws its own placement over the scrim.
    palette: bool,
    _subscription: Subscription,
}

impl Workspace {
    /// Opens `channel` and, with `message`, scrolls to it, loading older
    /// history as needed; a message not in the channel timeline (a thread
    /// reply) opens in the thread panel instead.
    pub fn open_channel_at(
        &mut self,
        channel: &ChannelSummary,
        message: Option<EventId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_channel(channel, window, cx);
        if let Some(message) = message {
            self.reveal(message, window, cx);
        }
    }

    fn opener(cx: &mut Context<Self>) -> OpenChannel {
        let weak = cx.entity().downgrade();
        Rc::new(move |channel, message, window, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.open_channel_at(channel, message, window, cx)
            });
        })
    }

    fn show<V: Render + EventEmitter<Dismissed>>(
        &mut self,
        view: Entity<V>,
        palette: bool,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe(&view, |this, _, _: &Dismissed, cx| {
            this.dialog = None;
            cx.notify();
        });
        self.dialog = Some(OpenDialog {
            view: view.into(),
            palette,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = self.community.clone() else {
            return;
        };
        let current = self
            .channel
            .as_ref()
            .map(|page| page.read(cx).channel().clone());
        let on_open = Self::opener(cx);
        let weak = cx.entity().downgrade();
        let on_agents: Callback = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| this.select_page(Page::Agents, cx));
        });
        let dialog =
            cx.new(|cx| SearchDialog::new(community, current, on_open, on_agents, window, cx));
        self.show(dialog, true, cx);
    }

    pub(super) fn open_new_channel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = self.community.clone() else {
            return;
        };
        let on_open = Self::opener(cx);
        let dialog = cx.new(|cx| NewChannelDialog::new(community, on_open, window, cx));
        self.show(dialog, false, cx);
    }

    pub(super) fn open_new_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(community) = self.community.clone() else {
            return;
        };
        let on_open = Self::opener(cx);
        let dialog = cx.new(|cx| NewMessageDialog::new(community, on_open, window, cx));
        self.show(dialog, false, cx);
    }

    pub(super) fn open_add_community(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = cx.new(|cx| AddCommunity::new(false, window, cx));
        self.show(dialog, false, cx);
    }

    /// The page closed itself (the viewer left or hid the channel).
    pub(super) fn close_channel(&mut self, cx: &mut Context<Self>) {
        self.channel = None;
        self.thread = None;
        if let Some(community) = &self.community {
            community.read(cx).session.select(None);
        }
        cx.notify();
    }

    pub(super) fn dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.dialog.as_ref()?;
        let weak = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.dialog = None;
                cx.notify();
            });
        });
        let content = if open.palette {
            // The palette fills the scrim and places itself near the top.
            div()
                .w(window.viewport_size().width)
                .h(window.viewport_size().height)
                .child(open.view.clone())
                .into_any_element()
        } else {
            open.view.clone().into_any_element()
        };
        Some(overlay::modal(window, content, Some(dismiss)).into_any_element())
    }
}
