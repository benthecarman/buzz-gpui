//! Agent activity capture for the archive (relay.md §3.3 `observer`): the
//! live route for kinds 24200 and 44200 addressed to the viewer, and the
//! channel its envelopes leave the session through.

use super::{Actor, Message, Session};
use crate::event::Verified;
use nostr::prelude::Event;
use std::sync::Arc;
use tokio::sync::mpsc;

/// What the session captures and where it sends it.
pub(super) struct Observer {
    kinds: Vec<u16>,
    sender: mpsc::UnboundedSender<Arc<Event>>,
}

impl Session {
    /// Receives the viewer's agent `kinds` (24200 activity, 44200 metrics)
    /// live from now on, replacing an earlier capture. Empty `kinds`
    /// stops it. Envelopes arrive signature-checked but not decrypted.
    pub fn observe(&self, kinds: Vec<u16>) -> mpsc::UnboundedReceiver<Arc<Event>> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let _ = self.commands.send(Message::With(Box::new(move |actor| {
            actor.observer = (!kinds.is_empty()).then_some(Observer { kinds, sender });
            actor.observe_live();
        })));
        receiver
    }
}

impl Actor {
    /// Applies the capture to the live connection, also after a reconnect.
    pub(super) fn observe_live(&mut self) {
        let kinds = self
            .observer
            .as_ref()
            .map(|observer| observer.kinds.clone())
            .unwrap_or_default();
        if let Some(live) = &mut self.live {
            live.watch_observer(kinds);
        }
    }

    pub(super) fn observed(&mut self, events: Vec<Verified>) {
        let Some(observer) = &self.observer else {
            return;
        };
        for event in events {
            if observer.sender.send(event.event().clone()).is_err() {
                // Nobody is listening any more.
                self.observer = None;
                self.observe_live();
                return;
            }
        }
    }
}
