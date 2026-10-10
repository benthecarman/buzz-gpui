//! The community catalog of one session (`catalog.ts`
//! `createCommunityCatalog`): every shared NIP-AP head readable on the
//! relay, plus the viewer's own share changes through the session's
//! durable outbox.

use super::{Message, Session, SessionSnapshot, SessionStatus};
use crate::catalog::{
    CatalogKind, CatalogModel, CatalogStatus, CatalogView, Head, Publication, READ_FAILED,
    TeamPublication, catalog_draft, read_all, read_head,
};
use crate::outbox::{Delivery, Outbox, SendOptions};
use crate::reader::Reader;
use nostr::prelude::EventId;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

const CLOSED: &str = "The session has closed";
const NOT_WRITABLE: &str = "This community cannot update catalog sharing.";

/// A cheap handle; clones share one catalog. Keep one per session: each
/// [`Session::community_catalog`] call starts a separate one.
#[derive(Clone)]
pub struct CommunityCatalog {
    inner: Arc<Inner>,
}

struct Inner {
    runtime: tokio::runtime::Handle,
    reader: Reader,
    viewer: String,
    state: Mutex<State>,
    view: watch::Sender<Arc<CatalogView>>,
}

struct State {
    model: CatalogModel,
    outbox: Option<Outbox>,
    loading: bool,
    /// Own changes whose head read is in flight.
    verifying: HashSet<String>,
}

impl Session {
    /// Starts this session's community catalog. Nothing is read until
    /// [`CommunityCatalog::retain`] or [`CommunityCatalog::refresh`].
    pub fn community_catalog(&self) -> CommunityCatalog {
        let inner = Arc::new(Inner {
            runtime: self.runtime.clone(),
            reader: self.direct.reader.clone(),
            viewer: self.viewer.clone(),
            state: Mutex::new(State {
                model: CatalogModel::new(self.viewer.clone()),
                outbox: None,
                loading: false,
                verifying: HashSet::new(),
            }),
            view: watch::Sender::new(Arc::default()),
        });
        let weak = Arc::downgrade(&inner);
        let (commands, snapshot) = (self.commands.clone(), self.snapshot.clone());
        self.runtime.spawn(follow_outbox(weak, commands, snapshot));
        CommunityCatalog { inner }
    }
}

/// Fetches the session's outbox each time the session becomes ready (a
/// reconnect replaces it) and rebuilds on every outbox change.
async fn follow_outbox(
    catalog: Weak<Inner>,
    commands: mpsc::UnboundedSender<Message>,
    mut session: watch::Receiver<Arc<SessionSnapshot>>,
) {
    let mut ready = false;
    let mut changes: Option<watch::Receiver<u64>> = None;
    loop {
        let now_ready = session.borrow_and_update().status == SessionStatus::Ready;
        if now_ready && !ready {
            let (reply, answer) = oneshot::channel();
            let sent = commands.send(Message::With(Box::new(move |actor| {
                let _ = reply.send(actor.outbox.clone());
            })));
            let outbox = match sent {
                Ok(()) => answer.await.ok().flatten(),
                Err(_) => return,
            };
            let Some(inner) = catalog.upgrade() else {
                return;
            };
            changes = outbox.as_ref().map(Outbox::changes);
            inner.lock().outbox = outbox;
            inner.rebuild();
            let reconnected = inner.lock().model.status == CatalogStatus::Error;
            if reconnected {
                inner.refresh();
            }
            inner.verify_accepted();
        }
        ready = now_ready;
        let outbox_changed = async {
            match &mut changes {
                Some(changes) => changes.changed().await.is_ok(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            changed = session.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = outbox_changed => {
                if !changed {
                    changes = None;
                }
                let Some(inner) = catalog.upgrade() else {
                    return;
                };
                inner.rebuild();
                inner.verify_accepted();
            }
        }
        if catalog.strong_count() == 0 {
            return;
        }
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn rebuild(&self) {
        let view = {
            let mut state = self.lock();
            let ops = state
                .outbox
                .as_ref()
                .map(|outbox| outbox.visible())
                .unwrap_or_else(|| Arc::new([]));
            let writable = state.outbox.is_some();
            state.model.build(&ops, writable)
        };
        self.view.send_replace(Arc::new(view));
    }

    fn refresh(self: &Arc<Self>) {
        {
            let mut state = self.lock();
            if state.loading {
                return;
            }
            state.loading = true;
            state.model.status = CatalogStatus::Loading;
        }
        self.rebuild();
        let inner = self.clone();
        self.runtime.spawn(async move {
            let result = read_all(&inner.reader).await;
            {
                let mut state = inner.lock();
                state.loading = false;
                match result {
                    Ok(events) => state.model.loaded(events),
                    Err(_) => state.model.failed(),
                }
            }
            inner.rebuild();
            inner.verify_accepted();
        });
    }

    /// Promotes accepted own changes only once the relay's head is theirs.
    fn verify_accepted(self: &Arc<Self>) {
        let pending = {
            let mut state = self.lock();
            let Some(outbox) = &state.outbox else {
                return;
            };
            let ops = outbox.visible();
            let heads: Vec<Head> = state
                .model
                .to_verify(&ops)
                .into_iter()
                .filter(|head| !state.verifying.contains(&head.id))
                .collect();
            for head in &heads {
                state.verifying.insert(head.id.clone());
            }
            heads
        };
        for change in pending {
            let inner = self.clone();
            self.runtime.spawn(async move {
                let kind = CatalogKind::from_kind(change.kind).expect("catalog heads");
                let head = read_head(&inner.reader, kind, &inner.viewer, &change.d)
                    .await
                    .ok()
                    .flatten();
                {
                    let mut state = inner.lock();
                    state.verifying.remove(&change.id);
                    state.model.confirmed_read(&change, head);
                }
                inner.rebuild();
            });
        }
    }
}

impl CommunityCatalog {
    pub fn view(&self) -> watch::Receiver<Arc<CatalogView>> {
        self.inner.view.subscribe()
    }

    pub fn snapshot(&self) -> Arc<CatalogView> {
        self.inner.view.borrow().clone()
    }

    /// Reads the whole catalog again; joins a read already in flight.
    pub fn refresh(&self) {
        self.inner.refresh();
    }

    /// Called whenever a catalog surface opens: reads the catalog unless
    /// a read is in flight.
    pub fn retain(&self) {
        self.inner.refresh();
    }

    pub fn viewer(&self) -> &str {
        &self.inner.viewer
    }

    async fn run<R: Send + 'static>(
        &self,
        work: impl Future<Output = Result<R, String>> + Send + 'static,
    ) -> Result<R, String> {
        self.inner
            .runtime
            .spawn(work)
            .await
            .unwrap_or_else(|_| Err(CLOSED.to_owned()))
    }

    /// Publishes a share or unshare head for one of the viewer's own
    /// coordinates. Unsharing is a newer head without the `shared` tag,
    /// never a deletion. Without `content` the newest known content is
    /// reused.
    pub async fn publish(
        &self,
        kind: CatalogKind,
        d: &str,
        shared: bool,
        content: Option<String>,
    ) -> Result<EventId, String> {
        let inner = self.inner.clone();
        let d = d.to_owned();
        self.run(async move {
            let ready = {
                let state = inner.lock();
                if state.outbox.is_none() {
                    return Err(NOT_WRITABLE.to_owned());
                }
                state.model.status == CatalogStatus::Ready
            };
            if !ready {
                return Err("The community catalog is still loading. Try again.".into());
            }
            // The replacement must outrank the relay's current head, also
            // one another device published after the catalog read.
            let current = read_head(&inner.reader, kind, &inner.viewer, &d)
                .await
                .map_err(|_| READ_FAILED.to_owned())?;
            let (outbox, ops) = {
                let mut state = inner.lock();
                if let Some(head) = current {
                    state.model.retain(head);
                }
                let outbox = state.outbox.clone().ok_or(NOT_WRITABLE)?;
                let ops = outbox.visible();
                (outbox, ops)
            };
            inner.rebuild();
            let (body, (head_at, refused)) = {
                let mut state = inner.lock();
                (
                    content.or_else(|| state.model.content(&ops, kind, &d)),
                    state.model.replacing(&ops, kind, &d),
                )
            };
            let body = body.ok_or("Nothing has been shared from this coordinate.")?;
            // Replaceable heads have second precision: wait out the head's
            // second rather than publish one that cannot outrank it.
            if let Some(at) = head_at {
                for _ in 0..30 {
                    if now() > at {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if now() <= at {
                    return Err(
                        "Please wait a second before changing catalog sharing again.".into(),
                    );
                }
            }
            let id = outbox
                .send(
                    catalog_draft(kind, &d, body, shared),
                    SendOptions::default(),
                )
                .map_err(|error| error.to_string())?;
            // The new head is newer, so a refused older change is clutter.
            if let Some(refused) = refused {
                let _ = outbox.dismiss(&refused).await;
            }
            Ok(id)
        })
        .await
    }

    /// Resumes a change. An accepted one is re-read, never resent.
    pub fn retry(&self, operation: EventId) {
        let inner = self.inner.clone();
        // The outbox schedules its attempts on the session's runtime.
        let _runtime = inner.runtime.enter();
        let accepted = {
            let mut state = inner.lock();
            let Some(outbox) = state.outbox.clone() else {
                return;
            };
            let accepted = outbox
                .get(&operation)
                .is_some_and(|op| matches!(op.delivery, Delivery::Accepted | Delivery::Seen));
            if accepted {
                state.model.unconfirmed.remove(&operation.to_hex());
            } else if let Err(error) = outbox.retry(&operation, None) {
                tracing::warn!(%error, "catalog retry failed");
            }
            accepted
        };
        if accepted {
            inner.rebuild();
            inner.verify_accepted();
        }
    }

    /// Removes a change's notice. The relay keeps whatever it accepted.
    pub async fn dismiss(&self, operation: EventId) -> Result<(), String> {
        let outbox = self.inner.lock().outbox.clone();
        self.run(async move {
            match outbox {
                Some(outbox) => outbox.dismiss(&operation).await.map_err(|e| e.to_string()),
                None => Ok(()),
            }
        })
        .await
    }

    /// Re-reads a listed team's coordinate so adoption uses exactly the
    /// head that was previewed, and only while it is still shared.
    pub async fn current_team(&self, listed: &TeamPublication) -> Result<TeamPublication, String> {
        let inner = self.inner.clone();
        let listed = listed.clone();
        self.run(async move {
            let gone = || "This team is no longer available in the catalog.".to_owned();
            let head = read_head(&inner.reader, CatalogKind::Team, &listed.owner, &listed.d)
                .await
                .map_err(|_| READ_FAILED.to_owned())?
                .ok_or_else(gone)?;
            if head.id != listed.event_id {
                return Err(
                    "This team has changed since it was listed. Refresh and try again.".into(),
                );
            }
            if !head.shared {
                return Err("This team is no longer shared to the community.".into());
            }
            match crate::catalog::parse_head(&head) {
                Some(Publication::Team(team)) => Ok(team),
                _ => Err(gone()),
            }
        })
        .await
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
