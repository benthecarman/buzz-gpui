//! "Share conversation" (`sessions/SessionShare.tsx`,
//! `sessions/share.ts`): moves a Me conversation to Messages, or links it
//! into another channel, granting access to the chosen people.

use super::dialogs::DialogEvent;
use crate::community::Community;
use buzz_relay_client::ChannelSummary;
use buzz_ui::{Button, Dialog, Surface};
use gpui_kit::*;

pub struct SessionShare {
    _community: Entity<Community>,
    _channel: ChannelSummary,
    busy: bool,
}

impl EventEmitter<DialogEvent> for SessionShare {}

impl SessionShare {
    pub fn new(
        community: Entity<Community>,
        channel: ChannelSummary,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Self {
        Self {
            _community: community,
            _channel: channel,
            busy: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }
}

impl Render for SessionShare {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Dialog::new("me-share", "Share conversation")
            .on_close(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close)))
            .footer(
                Button::new("me-share-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close))),
            )
    }
}
