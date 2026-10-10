//! Other people's statuses inside the session actor (`useUserStatus`,
//! `user-status.ts`). [`StatusDirectory`] is synchronous; this module runs
//! the reads it asks for. Live statuses arrive with the profiles route
//! and go through [`Actor::accept`].
//!
//! [`StatusDirectory`]: crate::user_status::StatusDirectory

use super::unread::now_ms;
use super::{Actor, Message, Session};
use crate::event::Verified;
use crate::reader::{Events, ReadError, ReadOptions};
use crate::user_status::{StatusRead, batch_filter, watch_keys};
use tokio::sync::mpsc;

/// Keeps people's statuses in [`super::SessionSnapshot::statuses`] while
/// it lives. Watches are counted, so overlapping views each hold one.
pub struct StatusWatch {
    commands: mpsc::UnboundedSender<Message>,
    ids: Vec<String>,
}

impl Drop for StatusWatch {
    fn drop(&mut self) {
        let ids = std::mem::take(&mut self.ids);
        if !ids.is_empty() {
            let _ = self.commands.send(Message::With(Box::new(move |actor| {
                if actor.statuses.unwatch(&ids, now_ms()) {
                    actor.publish();
                }
            })));
        }
    }
}

impl Session {
    /// Follows these people's statuses (lowercase hex keys; others are
    /// ignored) until the returned watch is dropped.
    pub fn watch_statuses(&self, ids: &[String]) -> StatusWatch {
        let ids = watch_keys(ids);
        if !ids.is_empty() {
            let counted = ids.clone();
            let _ = self.commands.send(Message::With(Box::new(move |actor| {
                actor.statuses.watch(&counted);
                actor.statuses_schedule();
            })));
        }
        StatusWatch {
            commands: self.commands.clone(),
            ids,
        }
    }
}

impl Actor {
    /// Starts the next status read if one is due.
    pub(super) fn statuses_schedule(&mut self) {
        if self.relay.is_none() {
            return;
        }
        let Some(read) = self.statuses.next_read(now_ms()) else {
            return;
        };
        let reader = self.reader.clone();
        let commands = self.commands.clone();
        self.runtime.spawn(async move {
            let result = reader
                .read(vec![batch_filter(&read.authors)], ReadOptions::background())
                .await;
            let _ = commands.send(Message::With(Box::new(move |actor| {
                actor.status_read(&read, result);
            })));
        });
    }

    fn status_read(&mut self, read: &StatusRead, result: Result<Events, ReadError>) {
        let now = now_ms();
        let finished = match &result {
            Ok(events) => self.statuses.finished(read, Ok(events), now),
            Err(_) => self.statuses.finished(read, Err(()), now),
        };
        let Some(changed) = finished else {
            return;
        };
        if changed {
            self.publish();
        }
        // A failure waits for its retry.
        if result.is_ok() {
            self.statuses_schedule();
        }
    }

    /// Shows a status the viewer just saved, before live traffic echoes it.
    pub(super) fn status_saved(&mut self, event: Verified) {
        if self.statuses.accept(&[event], now_ms()) {
            self.publish();
        }
    }

    pub(super) fn statuses_deadline(&self) -> Option<u64> {
        self.statuses.deadline()
    }

    pub(super) fn statuses_tick(&mut self) {
        if self.statuses.tick(now_ms()) {
            self.publish();
        }
        self.statuses_schedule();
    }

    /// The live connection came back: statuses changed while away are read
    /// again.
    pub(super) fn statuses_reconnected(&mut self) {
        self.statuses.reconnect();
        self.statuses_schedule();
    }
}
