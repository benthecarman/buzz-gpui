//! A message a plugin view shows as Buzz draws it in a conversation
//! (`conversation.ui.Message`): byline, text, attachments and reactions.
//! Opening its thread, replying and picking a reaction go to the plugin,
//! which decides what they do (`onOpenThread`, `onReply`); a failed send
//! retries in place.

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

/// Tells the plugin's view what happened on the message.
pub type Send = Rc<dyn Fn(&str, serde_json::Value, &mut App)>;

pub struct MessageView {
    community: Entity<Community>,
    send: Send,
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
        send: Send,
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
            send,
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
        let summary = community
            .snapshot
            .channels
            .iter()
            .find(|channel| channel.id == self.channel)
            .cloned();
        let members = summary.as_ref().map(|channel| channel.members.clone());
        let mut rows = Rows::new(
            community.session.viewer(),
            Directory::of(community, members.as_deref(), cx),
        );
        // What the viewer may do in it, such as react.
        rows.channel = summary;
        let ids = |row: &MessageRow| {
            serde_json::json!({
                "message": row.id.to_hex(),
                "root": row.thread_root_id.map(|root| root.to_hex()),
                "channel": &*row.channel,
            })
        };
        let send = self.send.clone();
        rows.actions.on_open_thread =
            Some(Rc::new(move |row, _, cx| send("openThread", ids(row), cx)));
        let session = community.session.clone();
        rows.actions.on_retry = Some(Rc::new(move |row, _, cx| {
            let (session, id) = (session.clone(), row.id);
            Runtime::handle(cx).spawn(async move {
                let _ = session.retry_send(id).await;
            });
        }));
        let send = self.send.clone();
        rows.actions.on_command = Some(Rc::new(move |row, command, _, cx| match command {
            message::Command::Reply => send("reply", ids(row), cx),
            message::Command::React { content, .. } => {
                let mut value = ids(row);
                value["emoji"] = content.into();
                send("react", value, cx)
            }
            _ => {}
        }));
        // "Add reaction" asks the plugin, which may open its own picker
        // or react with `messages/react`.
        let (send, channel) = (self.send.clone(), self.channel.clone());
        rows.actions.on_popup = Some(Rc::new(move |popup, _, cx| {
            if let Some((message, message::Popup::Picker { .. })) = popup {
                let value = serde_json::json!({ "message": message.to_hex(), "channel": channel });
                send("addReaction", value, cx);
            }
        }));
        let rows = Rc::new(rows);
        let item = Item {
            row,
            head: true,
            day: None,
            group: None,
        };
        message::render(&item, &rows, cx)
    }
}
