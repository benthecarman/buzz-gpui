//! A message a plugin view shows as Buzz draws it in a conversation
//! (`conversation.ui.Message`): byline, text, attachments and reactions,
//! for display.

use crate::community::Community;
use crate::runtime::Runtime;
use crate::ui::markdown::Directory;
use crate::ui::message::{self, Item, Rows};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::session::ThreadView;
use buzz_relay_client::{EventId, ThreadStatus};
use buzz_ui::{ActiveTheme, TypeStyled};
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

pub struct MessageView {
    community: Entity<Community>,
    channel: String,
    row: Option<Arc<MessageRow>>,
    /// The message could not be read.
    missing: bool,
    _thread: ThreadView,
    _follow: Task<()>,
}

impl MessageView {
    /// Shows `message` in `channel`, following edits and reactions.
    pub fn new(
        community: Entity<Community>,
        channel: String,
        message: EventId,
        cx: &mut Context<Self>,
    ) -> Self {
        let thread = community
            .read(cx)
            .session
            .open_thread(&channel, message, true);
        let mut receiver = thread.snapshot.clone();
        let (sender, mut snapshots) = futures::channel::mpsc::unbounded();
        let _ = sender.unbounded_send(receiver.borrow_and_update().clone());
        Runtime::handle(cx).spawn(async move {
            while receiver.changed().await.is_ok() {
                let snapshot = receiver.borrow_and_update().clone();
                if sender.unbounded_send(snapshot).is_err() {
                    return;
                }
            }
        });
        let follow = cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(snapshot) = snapshots.next().await {
                let row = snapshot
                    .root
                    .iter()
                    .chain(snapshot.replies.iter())
                    .find(|row| row.id == message)
                    .cloned();
                let missing = row.is_none() && snapshot.status == ThreadStatus::Error;
                if this
                    .update(cx, |this, cx| {
                        this.row = row;
                        this.missing = missing;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            community,
            channel,
            row: None,
            missing: false,
            _thread: thread,
            _follow: follow,
        }
    }
}

impl Render for MessageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let Some(row) = self.row.clone() else {
            let text = if self.missing {
                "This message is not available."
            } else {
                "Loading message…"
            };
            return div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(text)
                .into_any_element();
        };
        let community = self.community.read(cx);
        let members = community
            .snapshot
            .channels
            .iter()
            .find(|channel| channel.id == self.channel)
            .map(|channel| channel.members.clone());
        let rows = Rc::new(Rows::new(
            community.session.viewer(),
            Directory::of(community, members.as_deref(), cx),
        ));
        let item = Item {
            row,
            head: true,
            day: None,
            group: None,
        };
        message::render(&item, &rows, cx)
    }
}
