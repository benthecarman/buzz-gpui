//! Open thread panels, driven as `crate::threads` describes.

use super::{Actor, Message};
use crate::Events;
use crate::event::Verified;
use crate::fold::LocalDelivery;
use crate::reader::{ReadError, ReadErrorKind};
use crate::threads::{ThreadConfig, ThreadRead, ThreadSnapshot, ThreadState, thread_authority};
use nostr::prelude::{EventId, PublicKey};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// An open thread panel. Dropping it disposes the view.
pub struct ThreadView {
    pub snapshot: watch::Receiver<Arc<ThreadSnapshot>>,
    pub(super) id: u64,
    pub(super) commands: mpsc::UnboundedSender<Message>,
}

impl ThreadView {
    pub fn load_more(&self) {
        let _ = self.commands.send(Message::ThreadMore(self.id));
    }

    pub fn refresh(&self) {
        let _ = self.commands.send(Message::ThreadRefresh(self.id));
    }

    /// Reports what the panel shows, as [`super::ChannelView::viewport`]
    /// does for a channel: the replies fully in view (at most 128) and
    /// whether the panel sits at its newest reply. Visible replies that
    /// stay in view for [`super::READING_DWELL_MS`] are read, and at the
    /// bottom the thread is caught up. Report nothing and `false` while the
    /// window or the panel is not focused.
    pub fn viewport(&self, visible: Vec<EventId>, at_bottom: bool) {
        let id = self.id;
        let _ = self.commands.send(Message::With(Box::new(move |actor| {
            actor.thread_viewport(id, visible, at_bottom)
        })));
    }
}

impl Drop for ThreadView {
    fn drop(&mut self) {
        let _ = self.commands.send(Message::CloseThread(self.id));
    }
}

pub(super) struct OpenThread {
    pub state: ThreadState,
    pub channel: String,
    view: watch::Sender<Arc<ThreadSnapshot>>,
}

impl OpenThread {
    /// What the panel shows now.
    pub fn snapshot(&self) -> Arc<ThreadSnapshot> {
        self.view.borrow().clone()
    }

    fn publish(&mut self) {
        if let Some(snapshot) = self.state.take_snapshot() {
            self.view.send_replace(snapshot);
        }
    }
}

impl Actor {
    pub(super) fn open_thread(
        &mut self,
        id: u64,
        channel: String,
        message: EventId,
        exact: bool,
        view: watch::Sender<Arc<ThreadSnapshot>>,
    ) {
        let (Some(relay), Ok(viewer)) = (
            self.relay_author(),
            PublicKey::from_hex(&self.identity.pubkey_hex()),
        ) else {
            return;
        };
        let readable = self
            .discovery
            .as_ref()
            .is_some_and(|discovery| discovery.can_access(&channel));
        let seed = readable
            .then(|| {
                self.timelines
                    .get(&channel)
                    .and_then(|timeline| timeline.find(&message))
                    .or_else(|| {
                        self.threads
                            .values()
                            .find_map(|thread| thread.state.event(&message).cloned())
                    })
            })
            .flatten();
        let config = ThreadConfig {
            channel: channel.clone(),
            message_id: message,
            exact,
            relay_author: relay,
            signing_authority: Some(relay),
            viewer,
            authority_host: thread_authority(self.origin.websocket()).ok(),
        };
        let mut state = ThreadState::new(config, self.clock.clone(), seed.as_ref());
        let retained: Vec<Verified> = self
            .timelines
            .get(&channel)
            .map(|timeline| timeline.events())
            .unwrap_or_default();
        state.receive(&retained);
        state.set_local(&self.local_operations());
        let first = state.refresh();
        let mut thread = OpenThread {
            state,
            channel,
            view,
        };
        thread.publish();
        self.threads.insert(id, thread);
        if let Some(read) = first {
            self.thread_read(id, read);
        }
    }

    pub(super) fn thread_read(&self, id: u64, read: ThreadRead) {
        let Some(thread) = self.threads.get(&id) else {
            return;
        };
        let channel = thread.channel.clone();
        let reader = self.reader.clone();
        let commands = self.commands.clone();
        self.runtime.spawn(async move {
            let result = reader.read(read.filters, read.options).await;
            let _ = commands.send(Message::ThreadRead {
                id,
                token: read.token,
                channel,
                result,
            });
        });
    }

    pub(super) fn thread_result(
        &mut self,
        id: u64,
        token: u64,
        channel: &str,
        result: Result<Events, ReadError>,
    ) {
        // A read that finished after access was lost must not reveal anything.
        let readable = self
            .discovery
            .as_ref()
            .is_some_and(|discovery| discovery.can_access(channel));
        let result = if readable {
            result
        } else {
            Err(ReadError {
                kind: ReadErrorKind::Unavailable,
                message: "Conversation is unavailable".into(),
                status: None,
                retry_after: None,
            })
        };
        let Some(thread) = self.threads.get_mut(&id) else {
            return;
        };
        let completion = thread.state.complete(token, result);
        thread.publish();
        if completion.denied {
            if let Some(discovery) = &mut self.discovery {
                discovery.deny(channel);
            }
            self.purge_denied();
            self.publish();
        }
        if !completion.admit.is_empty() {
            self.accept(&completion.admit);
        }
        if let Some(next) = completion.next {
            self.thread_read(id, next);
        }
    }

    pub(super) fn thread_more(&mut self, id: u64, refresh: bool) {
        let Some(thread) = self.threads.get_mut(&id) else {
            return;
        };
        let read = if refresh {
            thread.state.refresh()
        } else {
            thread.state.load_more()
        };
        thread.publish();
        if let Some(read) = read {
            self.thread_read(id, read);
        }
    }

    pub(super) fn threads_receive(&mut self, events: &[Verified]) {
        for thread in self.threads.values_mut() {
            if thread.state.receive(events) {
                thread.publish();
            }
        }
    }

    pub(super) fn threads_local(&mut self, local: &[(Verified, LocalDelivery)]) {
        for thread in self.threads.values_mut() {
            thread.state.set_local(local);
            thread.publish();
        }
    }

    /// The channel's live route is back: repair what each view retained.
    pub(super) fn threads_established(&mut self, channels: &[String]) {
        let reads: Vec<(u64, ThreadRead)> = self
            .threads
            .iter_mut()
            .filter(|(_, thread)| channels.contains(&thread.channel))
            .filter_map(|(id, thread)| thread.state.refresh().map(|read| (*id, read)))
            .collect();
        for (id, read) in reads {
            self.thread_read(id, read);
        }
    }

    /// Revokes views of channels the viewer can no longer read.
    pub(super) fn threads_purge(&mut self) {
        let Some(discovery) = &self.discovery else {
            return;
        };
        for thread in self.threads.values_mut() {
            if !discovery.can_access(&thread.channel) {
                thread.state.revoke();
                thread.publish();
            }
        }
    }
}
