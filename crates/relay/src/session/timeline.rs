//! One channel's retained history plus its live traffic, folded into rows.

use crate::event::{Verified, kinds};
use crate::fold::{FoldOptions, LocalDelivery, MessageClock, MessageProjection, MessageRow};
use crate::window::Cursor;
use nostr::prelude::{EventId, PublicKey};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::Instant;

/// Rows retained per channel before older pages are refused.
pub const MAX_ROWS: usize = 2400;
pub const MAX_BYTES: usize = 8 << 20;
/// A head newer than this is reused without a read.
pub const FRESH_FOR: std::time::Duration = std::time::Duration::from_secs(60);

/// Kinds a channel window takes from live traffic.
const WINDOW_KINDS: &[u16] = &[
    kinds::CHAT,
    kinds::AGENT_MESSAGE,
    kinds::DIFF,
    kinds::SYSTEM_MESSAGE,
    kinds::EDIT,
    kinds::DELETION,
    kinds::MODERATOR_DELETE,
    kinds::REACTION,
    kinds::THREAD_SUMMARY,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineStatus {
    Loading,
    Ready,
    Error(String),
}

/// What a channel view renders.
#[derive(Clone, Debug)]
pub struct TimelineSnapshot {
    pub channel: String,
    pub status: TimelineStatus,
    pub rows: Arc<[Arc<MessageRow>]>,
    pub has_more: bool,
    pub loading_older: bool,
    /// Older history exists but is over the retention budget.
    pub history_limited: bool,
    /// Shown from before a disconnect; a refresh is on its way.
    pub stale: bool,
    pub error: Option<String>,
}

impl TimelineSnapshot {
    pub fn loading(channel: &str) -> Self {
        Self {
            channel: channel.to_string(),
            status: TimelineStatus::Loading,
            rows: Arc::new([]),
            has_more: false,
            loading_older: false,
            history_limited: false,
            stale: false,
            error: None,
        }
    }
}

pub(super) struct Timeline {
    pub id: String,
    pub viewers: usize,
    /// Bumped when the timeline is reset, fencing reads started before.
    pub generation: u64,
    pages: HashMap<EventId, Verified>,
    traffic: HashMap<EventId, Verified>,
    /// Our own outbox operations for this channel, signed, with their state.
    local: Vec<(Verified, LocalDelivery)>,
    projection: MessageProjection,
    pub cursor: Option<Cursor>,
    pub has_more: bool,
    /// Only the head is loaded, so a catch-up may move the cursor.
    pub at_head: bool,
    pub loaded_at: Option<Instant>,
    pub loading_head: bool,
    pub loading_older: bool,
    pub history_limited: bool,
    pub stale: bool,
    pub error: Option<String>,
    current: Arc<TimelineSnapshot>,
    /// One per open view; closed ones are dropped on the next publish.
    views: Vec<watch::Sender<Arc<TimelineSnapshot>>>,
}

impl Timeline {
    pub fn new(id: String, relay_author: PublicKey, clock: Arc<MessageClock>) -> Self {
        let projection = MessageProjection::new(
            id.clone(),
            relay_author,
            FoldOptions {
                signing_authority: Some(relay_author),
                ..Default::default()
            },
        )
        .with_clock(clock);
        let snapshot = Arc::new(TimelineSnapshot::loading(&id));
        Self {
            id,
            viewers: 0,
            generation: 0,
            pages: HashMap::new(),
            traffic: HashMap::new(),
            local: Vec::new(),
            projection,
            cursor: None,
            has_more: false,
            at_head: true,
            loaded_at: None,
            loading_head: false,
            loading_older: false,
            history_limited: false,
            stale: false,
            error: None,
            current: snapshot,
            views: Vec::new(),
        }
    }

    /// Starts feeding a view, beginning with the current snapshot.
    pub fn attach(&mut self, view: watch::Sender<Arc<TimelineSnapshot>>) {
        view.send_replace(self.current.clone());
        self.views.push(view);
    }

    pub fn needs_head(&self) -> bool {
        !self.loading_head
            && (self.stale || self.loaded_at.is_none_or(|at| at.elapsed() > FRESH_FOR))
    }

    /// A fresh head replaces retained pages; older pages are added to them.
    pub fn apply_page(
        &mut self,
        events: Vec<Verified>,
        cursor: Option<Cursor>,
        has_more: bool,
        head: bool,
    ) {
        if head {
            self.pages.clear();
            self.at_head = true;
            self.loaded_at = Some(Instant::now());
            self.stale = false;
        } else {
            self.at_head = false;
        }
        for event in events {
            self.pages.insert(event.id, event);
        }
        let rows = self.row_count();
        let bytes: usize = self
            .pages
            .values()
            .chain(self.traffic.values())
            .map(Verified::byte_size)
            .sum();
        if !head && (rows > MAX_ROWS || bytes > MAX_BYTES) {
            // Over budget: keep what was shown and stop paging back.
            self.history_limited = true;
            self.has_more = false;
        } else {
            self.cursor = cursor;
            self.has_more = has_more && self.cursor.is_some();
        }
        self.error = None;
    }

    /// Merges a catch-up head without dropping older pages. The cursor moves
    /// only while nothing older than the head was loaded.
    pub fn merge_head(&mut self, events: Vec<Verified>, cursor: Option<Cursor>, has_more: bool) {
        for event in events {
            self.pages.insert(event.id, event);
        }
        if self.at_head {
            self.cursor = cursor;
            self.has_more = has_more && self.cursor.is_some();
        }
        self.loaded_at = Some(Instant::now());
        self.stale = false;
        self.error = None;
    }

    /// Takes live events that belong here: rows and overlays tagged with
    /// this channel, and anything referring to an event already here
    /// (transitively, so a deletion of a reaction follows the reaction).
    pub fn accept(&mut self, events: &[Verified]) -> bool {
        let mut known: HashSet<EventId> = self
            .pages
            .keys()
            .chain(self.traffic.keys())
            .copied()
            .collect();
        let mut pending: Vec<&Verified> = events
            .iter()
            .filter(|event| WINDOW_KINDS.contains(&event.kind_u16()) && !known.contains(&event.id))
            .collect();
        let mut taken = false;
        loop {
            let before = pending.len();
            pending.retain(|event| {
                let belongs = event.channel() == Some(self.id.as_str())
                    || event.tags_named("e").any(|tag| {
                        tag.get(1)
                            .and_then(|id| EventId::from_hex(id).ok())
                            .is_some_and(|id| known.contains(&id))
                    });
                if belongs {
                    known.insert(event.id);
                    self.traffic.insert(event.id, (*event).clone());
                    taken = true;
                }
                !belongs
            });
            if pending.len() == before {
                return taken;
            }
        }
    }

    /// Replaces the local operations shown here. Content stays visible when
    /// it failed so it can be retried; a failed edit or reaction does not.
    pub fn set_local(&mut self, operations: &[(Verified, LocalDelivery)]) {
        let known: HashSet<EventId> = self
            .pages
            .keys()
            .chain(self.traffic.keys())
            .copied()
            .collect();
        let mut local: Vec<(Verified, LocalDelivery)> = operations
            .iter()
            .filter(|(event, delivery)| {
                let content = kinds::CHANNEL_ROW_KINDS.contains(&event.kind_u16());
                let here = event.channel() == Some(self.id.as_str())
                    || event.tags_named("e").any(|tag| {
                        tag.get(1)
                            .and_then(|id| EventId::from_hex(id).ok())
                            .is_some_and(|id| known.contains(&id))
                    });
                here && (content || delivery.status != crate::fold::Delivery::Failed)
            })
            .cloned()
            .collect();
        local.sort_by_key(|(event, _)| event.id);
        self.local = local;
    }

    /// An event shown in this channel, for edits, reactions and removals.
    pub fn find(&self, id: &EventId) -> Option<Verified> {
        self.pages
            .get(id)
            .or_else(|| self.traffic.get(id))
            .or_else(|| {
                self.local
                    .iter()
                    .find(|(event, _)| event.id == *id)
                    .map(|(event, _)| event)
            })
            .cloned()
    }

    /// The newest verified top-level message: the bottom of the channel, as
    /// a reading catch-up sees it. Unsent local rows do not count.
    pub fn bottom(&self) -> Option<String> {
        self.pages
            .values()
            .chain(self.traffic.values())
            .filter(|event| {
                crate::unread::CONTENT_KINDS.contains(&event.kind_u16())
                    && crate::unread::thread_reference(event).is_none()
            })
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| b.id.cmp(&a.id))
            })
            .map(Verified::id_hex)
    }

    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
    }

    /// Ids of everyone a row shows, for profile lookups.
    pub fn people(&self) -> Vec<String> {
        let rows = self.projection.snapshot();
        let mut people = Vec::new();
        for row in rows.iter() {
            people.push(row.author.to_hex());
            people.extend(row.participants.iter().map(|p| p.to_hex()));
            people.extend(row.mentions.iter().map(|p| p.to_hex()));
            for group in &row.reactions {
                people.extend(group.events.iter().map(|r| r.author.to_hex()));
            }
        }
        people
    }

    /// Refolds and publishes if anything visible changed.
    pub fn publish(&mut self) {
        let combined = self
            .pages
            .values()
            .chain(self.traffic.values())
            .chain(self.local.iter().map(|(event, _)| event));
        let deliveries = self
            .local
            .iter()
            .map(|(event, delivery)| (event.id, delivery.clone()));
        let (rows, _) = self.projection.reconcile_local(combined, deliveries);
        let status = match (&self.error, self.loaded_at) {
            (Some(error), None) => TimelineStatus::Error(error.clone()),
            (_, None) => TimelineStatus::Loading,
            _ => TimelineStatus::Ready,
        };
        let next = TimelineSnapshot {
            channel: self.id.clone(),
            status,
            rows,
            has_more: self.has_more,
            loading_older: self.loading_older,
            history_limited: self.history_limited,
            stale: self.stale,
            error: self.error.clone(),
        };
        let current = &self.current;
        let same = Arc::ptr_eq(&current.rows, &next.rows)
            && current.status == next.status
            && current.has_more == next.has_more
            && current.loading_older == next.loading_older
            && current.history_limited == next.history_limited
            && current.stale == next.stale
            && current.error == next.error;
        if same {
            return;
        }
        self.current = Arc::new(next);
        self.views.retain(|view| !view.is_closed());
        for view in &self.views {
            view.send_replace(self.current.clone());
        }
    }

    fn row_count(&self) -> usize {
        self.pages
            .values()
            .filter(|event| kinds::CHANNEL_ROW_KINDS.contains(&event.kind_u16()))
            .count()
    }
}
