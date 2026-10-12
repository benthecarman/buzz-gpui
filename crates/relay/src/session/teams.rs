//! The viewer's saved teams in one community (`capability.ts`
//! `createChannelKit`, team operations only): reads, saves through the
//! session's durable outbox, portable payloads, each team's shared
//! instructions in their own text record, and the team heads the agent
//! host reconciles its bindings against.

use super::delivery::{Wait, delivered};
use super::{Message, Session};
use crate::event::Verified;
use crate::kit::payload::{self, TeamPayload};
use crate::kit::{self, TeamManifest, TextRecord};
use crate::messages::Draft;
use crate::outbox::{Delivery, Outbox, SendOptions};
use crate::reader::{ReadOptions, Reader};
use crate::socket::Filter;
use crate::teams::{
    KIT_TAG, MANIFEST_TAG, ME_KIT_TAG, Team, TeamEntry, TeamHead, parse_team, record_tags,
    team_record,
};
use buzz_identity::Identity;
use nostr::prelude::EventId;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

const CLOSED: &str = "The session has closed";
const READ_LIMIT: usize = 500;
/// The outbox gives up on a delivery after this; the save stays queued.
const DELIVERY_WAIT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TeamsStatus {
    #[default]
    Idle,
    Loading,
    Ready,
    Error,
}

/// The viewer's teams as last read.
#[derive(Clone, Debug, Default)]
pub struct TeamsView {
    pub status: TeamsStatus,
    pub error: Option<String>,
    /// Every team head, deleted ones included.
    pub entries: Vec<TeamEntry>,
}

impl TeamsView {
    /// Saved teams, in the order they were read.
    pub fn teams(&self) -> impl Iterator<Item = &TeamEntry> {
        self.entries.iter().filter(|entry| !entry.deleted)
    }
}

/// A cheap handle; clones share one team list. Keep one per session.
#[derive(Clone)]
pub struct TeamKit {
    inner: Arc<Inner>,
}

struct Inner {
    runtime: tokio::runtime::Handle,
    reader: Reader,
    identity: Arc<Identity>,
    viewer: String,
    community: String,
    commands: mpsc::UnboundedSender<Message>,
    state: Mutex<State>,
    view: watch::Sender<Arc<TeamsView>>,
}

#[derive(Default)]
struct State {
    loading: bool,
    saving: bool,
    /// Teams whose text head this session has seen; its later absence is
    /// an error, never permission to fall back to legacy bundle text.
    known_text: HashSet<String>,
}

/// Told the id of the event a write enqueued.
type Enqueued = Arc<dyn Fn(&str) + Send + Sync>;

/// Lets a retry confirm the exact event an earlier attempt enqueued
/// (`Resume`). Clones share the one slot.
#[derive(Clone, Default)]
pub struct Resume {
    id: Arc<Mutex<Option<String>>>,
    enqueued: Option<Enqueued>,
}

impl Resume {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resumes an event enqueued earlier, and reports a new one to
    /// `enqueued` (e.g. to keep it in a receipt).
    pub fn with(id: Option<String>, enqueued: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            id: Arc::new(Mutex::new(id)),
            enqueued: Some(Arc::new(enqueued)),
        }
    }

    /// The event an attempt enqueued, if any.
    pub fn id(&self) -> Option<String> {
        self.id
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn set(&self, id: &str) {
        *self.id.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(id.to_owned());
        if let Some(enqueued) = &self.enqueued {
            enqueued(id);
        }
    }
}

impl std::fmt::Debug for Resume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Resume").field(&self.id()).finish()
    }
}

/// A team's current instructions, as saved (untrimmed), and their head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamText {
    pub text: String,
    pub head: String,
}

/// A team's current text head without its chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextHead {
    pub head: String,
    pub deleted: bool,
}

/// What a replaceable write checks besides its own head.
enum Guard {
    None,
    /// A live text head needs its team's live head, `team`.
    Team {
        id: String,
        head: Option<String>,
    },
}

const TEAM_CHANGED: &str =
    "This team changed or was deleted. Refresh and review it before saving its instructions.";

impl Session {
    /// Starts this session's saved-team list. `community` is the HTTP
    /// origin records are scoped to.
    pub fn team_kit(&self) -> TeamKit {
        TeamKit {
            inner: Arc::new(Inner {
                runtime: self.runtime.clone(),
                reader: self.direct.reader.clone(),
                identity: self.direct.identity.clone(),
                viewer: self.viewer.clone(),
                community: self.origin.http(),
                commands: self.commands.clone(),
                state: Mutex::default(),
                view: watch::Sender::new(Arc::default()),
            }),
        }
    }
}

fn filter(value: Value) -> Filter {
    match value {
        Value::Object(map) => map,
        _ => unreachable!("filters are objects"),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn update(&self, f: impl FnOnce(&mut TeamsView)) {
        self.view.send_modify(|view| {
            let mut next = (**view).clone();
            f(&mut next);
            *view = Arc::new(next);
        });
    }

    async fn fresh(&self, query: Value) -> Result<Vec<Verified>, String> {
        self.reader
            .read(
                vec![filter(query)],
                ReadOptions {
                    fresh: true,
                    ..Default::default()
                },
            )
            .await
            .map(|events| events.to_vec())
            .map_err(|error| error.message)
    }

    async fn outbox(&self) -> Result<Outbox, String> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Message::With(Box::new(move |actor| {
                let _ = reply.send(actor.outbox.clone());
            })))
            .map_err(|_| CLOSED.to_owned())?;
        answer
            .await
            .map_err(|_| CLOSED.to_owned())?
            .ok_or_else(|| "Not connected to the relay yet".to_owned())
    }

    /// The viewer's recipe heads in this community, one per coordinate.
    async fn heads(&self, strong: bool) -> Result<Vec<Verified>, String> {
        let mut query = json!({
            "kinds": [30078],
            "authors": [self.viewer],
            "#t": [KIT_TAG, MANIFEST_TAG, ME_KIT_TAG],
            "limit": READ_LIMIT,
        });
        if strong {
            query["consistency"] = json!("strong");
        }
        let events = self.fresh(query).await?;
        if events.len() >= READ_LIMIT {
            return Err(
                "Recipe catalog reached its read limit; no partial catalog will be used".into(),
            );
        }
        Ok(kit::catalog_heads(&events, &self.community)
            .into_iter()
            .cloned()
            .collect())
    }

    async fn read_entries(&self) -> Result<Vec<TeamEntry>, String> {
        let mut entries = Vec::new();
        for event in self.heads(false).await? {
            let raw = kit::open(&self.identity, &event, &self.community)?;
            if let Some((team, deleted)) = parse_team(&raw) {
                entries.push(TeamEntry {
                    team,
                    deleted,
                    event_id: event.id.to_hex(),
                    created_at: event.created_at_secs(),
                });
            }
        }
        entries.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.team.id.cmp(&b.team.id))
        });
        // A portable record supersedes an ordinary record for the same team.
        let portable: HashSet<String> = entries
            .iter()
            .filter(|entry| entry.team.portable.is_some())
            .map(|entry| entry.team.id.clone())
            .collect();
        entries.retain(|entry| entry.team.portable.is_some() || !portable.contains(&entry.team.id));
        Ok(entries)
    }

    /// Reads the list unless a read is in flight; `false` when it was.
    async fn refresh(self: Arc<Self>) -> bool {
        {
            let mut state = self.lock();
            if state.loading {
                return false;
            }
            state.loading = true;
            // Loading shows before the lock is released, so a waiter that
            // sees `loading` also sees this read in the view.
            self.update(|view| {
                view.status = TeamsStatus::Loading;
                view.error = None;
            });
        }
        let result = self.read_entries().await;
        let mut state = self.lock();
        state.loading = false;
        self.update(|view| match result {
            Ok(entries) => {
                view.status = TeamsStatus::Ready;
                view.error = None;
                view.entries = entries;
            }
            Err(error) => {
                view.status = TeamsStatus::Error;
                view.error = Some(error);
            }
        });
        true
    }

    /// The head of one coordinate, read fresh.
    async fn read_coordinate(&self, d: &str, strong: bool) -> Result<Option<Verified>, String> {
        let mut query = json!({
            "kinds": [30078],
            "authors": [self.viewer],
            "#d": [d],
            "limit": 2,
        });
        if strong {
            query["consistency"] = json!("strong");
        }
        let events = self.fresh(query).await?;
        Ok(kit::selected_head(&events).cloned())
    }

    /// Waits until the relay holds exactly `id`.
    async fn confirm(&self, outbox: &Outbox, id: EventId) -> Result<(), String> {
        let exact = json!({ "ids": [id.to_hex()], "limit": 1, "consistency": "strong" });
        if self.fresh(exact.clone()).await?.iter().any(|e| e.id == id) {
            return Ok(());
        }
        let saved = outbox.get(&id);
        if saved
            .as_ref()
            .is_none_or(|op| now().saturating_sub(op.created_at()) >= 15 * 60)
        {
            return Err("This operation could not be confirmed and is too old to replay. Keep your draft and inspect the current result before making a new save.".into());
        }
        // An uncertain save may still land: the exact read below decides.
        let wait = Wait {
            missing: Some("The save was dismissed."),
            failed: "The relay refused this save.",
            unknown_fails: false,
            closed: CLOSED,
        };
        let delivered = tokio::time::timeout(DELIVERY_WAIT, delivered(outbox, id, &wait))
            .await
            .unwrap_or(Ok(()));
        delivered?;
        if self.fresh(exact).await?.iter().any(|e| e.id == id) {
            Ok(())
        } else {
            Err("Save is awaiting exact relay confirmation; your draft is kept".into())
        }
    }

    fn encrypt(&self, raw: &Value) -> Result<String, String> {
        self.identity
            .encrypt_to_self(&raw.to_string())
            .map_err(|_| "Invalid channel recipe".to_string())
    }

    async fn save(
        &self,
        team: Team,
        expected: Option<String>,
        deleted: bool,
        resume: &Resume,
    ) -> Result<String, String> {
        let raw = team_record(&team, &self.community, deleted);
        self.publish(raw, expected, resume, false, Guard::None)
            .await
    }

    /// One replaceable private-record write (`publish`): a fresh head
    /// check, the unresolved-save guard, enqueue, exact confirmation and
    /// a selected-head readback. With an id in `resume` it confirms that
    /// earlier event instead of enqueueing again.
    async fn publish(
        &self,
        raw: Value,
        expected: Option<String>,
        resume: &Resume,
        strong: bool,
        guard: Guard,
    ) -> Result<String, String> {
        {
            let mut state = self.lock();
            if state.saving {
                return Err("Recipe saving is unavailable or already in progress".into());
            }
            state.saving = true;
        }
        let result = self
            .publish_unguarded(raw, expected, resume, strong, guard)
            .await;
        self.lock().saving = false;
        result
    }

    async fn publish_unguarded(
        &self,
        raw: Value,
        expected: Option<String>,
        resume: &Resume,
        strong: bool,
        guard: Guard,
    ) -> Result<String, String> {
        let tags = record_tags(&raw, &self.community)?;
        let d = tags[0][1].clone();
        let outbox = self.outbox().await?;
        let id = match resume.id() {
            Some(id) => EventId::from_hex(&id).map_err(|error| error.to_string())?,
            None => {
                self.check(&guard).await?;
                let head = self.read_coordinate(&d, strong).await?;
                if head.as_ref().map(|e| e.id.to_hex()) != expected {
                    return Err("This saved recipe changed. Refresh the catalog and review your draft before replacing it.".into());
                }
                let unresolved = outbox.visible().iter().any(|op| {
                    op.kind() == 30078
                        && op.tag("d") == Some(d.as_str())
                        && !matches!(op.delivery, Delivery::Accepted | Delivery::Seen)
                });
                if unresolved {
                    return Err("A save for this recipe is unresolved. Inspect Outbox and refresh before replacing it.".into());
                }
                // Replaceable records use seconds: no ambiguous equal-time head.
                if head
                    .as_ref()
                    .is_some_and(|head| head.created_at_secs() >= now())
                {
                    return Err("Please wait a second before saving this recipe again".into());
                }
                let content = self.encrypt(&raw)?;
                self.check(&guard).await?;
                let id = outbox
                    .send(
                        Draft {
                            kind: 30078,
                            content,
                            tags,
                        },
                        SendOptions::default(),
                    )
                    .map_err(|error| error.to_string())?;
                resume.set(&id.to_hex());
                id
            }
        };
        self.confirm(&outbox, id).await?;
        if self.read_coordinate(&d, strong).await?.map(|e| e.id) != Some(id) {
            return Err("Another recipe save is selected. Your draft is kept; refresh and review before saving again.".into());
        }
        Ok(id.to_hex())
    }

    /// A live text head is written only over its team's live head, as
    /// last listed and still the relay's.
    async fn check(&self, guard: &Guard) -> Result<(), String> {
        let Guard::Team { id, head } = guard else {
            return Ok(());
        };
        let entry = head.as_ref().and_then(|head| {
            self.view
                .borrow()
                .entries
                .iter()
                .find(|entry| entry.event_id == *head)
                .cloned()
        });
        let Some(entry) = entry.filter(|entry| !entry.deleted && entry.team.id == *id) else {
            return Err(TEAM_CHANGED.into());
        };
        let raw = team_record(&entry.team, &self.community, entry.deleted);
        let d = record_tags(&raw, &self.community)?
            .swap_remove(0)
            .swap_remove(1);
        if self
            .read_coordinate(&d, false)
            .await?
            .map(|e| e.id.to_hex())
            != Some(entry.event_id)
        {
            return Err(TEAM_CHANGED.into());
        }
        Ok(())
    }

    /// The current text head of a team: `None` only when a fresh read
    /// proves none exists. A head this session saw that is now missing is
    /// an error.
    async fn text_head(&self, team: &str) -> Result<Option<(TextRecord, String)>, String> {
        let d = kit::text_coordinate(&self.community, &self.viewer, team);
        let Some(event) = self.read_coordinate(&d, true).await? else {
            if self.lock().known_text.contains(team) {
                return Err("This team's saved instructions are unavailable".into());
            }
            return Ok(None);
        };
        // Seen counts before decoding: an invalid head that later
        // disappears stays unreadable rather than unlocking the legacy
        // fallback.
        self.lock().known_text.insert(team.to_owned());
        let raw = kit::open(&self.identity, &event, &self.community)?;
        let record = kit::parse_text_record(&raw, &self.community)?;
        if record.owner != self.viewer || event.tag("d") != Some(d.as_str()) {
            return Err("Team instructions coordinate mismatch".into());
        }
        Ok(Some((record, event.id.to_hex())))
    }

    /// The chunks of one text revision; exact revision reads only.
    async fn read_chunks(
        &self,
        team: &str,
        manifest: &TeamManifest,
    ) -> Result<Vec<TeamPayload>, String> {
        let mut payloads = Vec::with_capacity(manifest.chunks as usize);
        for d in payload::chunk_coordinates(manifest, &self.community, team) {
            let event = self
                .read_coordinate(&d, true)
                .await?
                .ok_or("Portable team payload is unavailable")?;
            let raw = kit::open(&self.identity, &event, &self.community)?;
            let chunk = payload::parse_payload_record(&raw, &self.community)?;
            if chunk.coordinate() != d || event.tag("d") != Some(d.as_str()) {
                return Err("Portable team payload coordinate mismatch".into());
            }
            payloads.push(chunk);
        }
        Ok(payloads)
    }

    /// Publishes immutable chunks, reusing identical ones. Never rewrites
    /// a revision with different bytes.
    async fn write_chunks(&self, payloads: &[TeamPayload]) -> Result<(), String> {
        let outbox = self.outbox().await?;
        for chunk in payloads {
            let record = chunk.record();
            let d = chunk.coordinate();
            if let Some(existing) = self.read_coordinate(&d, true).await? {
                let raw = kit::open(&self.identity, &existing, &self.community)?;
                if payload::parse_payload_record(&raw, &self.community)? != *chunk {
                    return Err("Portable team revision already has different content".into());
                }
                continue;
            }
            let pending = outbox
                .visible()
                .iter()
                .find(|op| op.kind() == 30078 && op.tag("d") == Some(d.as_str()))
                .map(|op| op.id());
            let id = match pending {
                Some(id) => id,
                None => outbox
                    .send(
                        Draft {
                            kind: 30078,
                            content: self.encrypt(&record)?,
                            tags: record_tags(&record, &self.community)?,
                        },
                        SendOptions::default(),
                    )
                    .map_err(|error| error.to_string())?,
            };
            self.confirm(&outbox, id).await?;
        }
        Ok(())
    }

    async fn read_text(&self, team: &str) -> Result<Option<TeamText>, String> {
        let Some((record, head)) = self.text_head(team).await? else {
            return Ok(None);
        };
        let text = match &record.manifest {
            Some(manifest) => payload::decode_team_text(
                manifest,
                &self.read_chunks(team, manifest).await?,
                &self.community,
                &self.viewer,
                team,
            )?,
            None => String::new(),
        };
        Ok(Some(TeamText { text, head }))
    }

    async fn prepare_text(
        &self,
        team: &str,
        text: &str,
        revision: &str,
    ) -> Result<TeamManifest, String> {
        let (manifest, payloads) =
            payload::encode_team_text(text, &self.community, &self.viewer, team, revision)?;
        self.write_chunks(&payloads).await?;
        let read = payload::decode_team_text(
            &manifest,
            &self.read_chunks(team, &manifest).await?,
            &self.community,
            &self.viewer,
            team,
        )?;
        if read != text {
            return Err("Team instructions could not be verified".into());
        }
        Ok(manifest)
    }

    async fn publish_text(
        &self,
        team: &str,
        manifest: Option<TeamManifest>,
        expected: Option<String>,
        team_head: Option<String>,
        resume: &Resume,
    ) -> Result<String, String> {
        let live = manifest.is_some();
        let record = TextRecord::new(&self.community, team, &self.viewer, manifest);
        let raw = kit::parse_text_record(&record.to_json(), &self.community)?.to_json();
        let guard = if live {
            Guard::Team {
                id: team.to_owned(),
                head: team_head,
            }
        } else {
            Guard::None
        };
        // A text head is read from the writer, as in `text_head`, so a
        // lagging replica can't hide another device's first text save.
        let id = self.publish(raw, expected, resume, true, guard).await?;
        self.lock().known_text.insert(team.to_owned());
        Ok(id)
    }

    async fn load(&self, team: &Team) -> Result<Value, String> {
        let manifest = team
            .portable
            .as_ref()
            .ok_or("This team has no portable definition")?;
        if manifest.owner != self.viewer {
            return Err("Portable team belongs to another viewer".into());
        }
        payload::decode_team_payload(
            manifest,
            &self.read_chunks(&team.id, manifest).await?,
            &self.community,
            &self.viewer,
            &team.id,
        )
    }

    async fn save_portable(
        &self,
        team: Team,
        snapshot: Value,
        expected: Option<String>,
        revision: String,
    ) -> Result<String, String> {
        let members = snapshot["members"].as_array().map_or(0, Vec::len);
        if team.agents.is_empty()
            || team.agents.len() != members
            || snapshot["team"]["name"].as_str() != Some(team.name.as_str())
        {
            return Err("Portable team members do not match their definitions".into());
        }
        let (manifest, payloads) = payload::encode_team_payload(
            &snapshot,
            &self.community,
            &self.viewer,
            &team.id,
            &revision,
        )?;
        self.write_chunks(&payloads).await?;
        let team = Team {
            portable: Some(manifest),
            ..team
        };
        // Confirm the whole payload before the manifest points at it.
        self.load(&team).await?;
        self.save(team, expected, false, &Resume::new()).await
    }

    async fn current_members(&self) -> Result<BTreeMap<String, TeamHead>, String> {
        let mut heads: BTreeMap<String, (bool, TeamHead)> = BTreeMap::new();
        for event in self.heads(true).await? {
            let raw = kit::open(&self.identity, &event, &self.community)?;
            let Some((team, deleted)) = parse_team(&raw) else {
                continue;
            };
            // A portable record supersedes the ordinary record it
            // replaced, even though retiring that record is the later
            // write.
            let portable = team.portable.is_some();
            let head = TeamHead {
                created_at: event.created_at_secs(),
                event_id: event.id.to_hex(),
                members: if deleted { Vec::new() } else { team.agents },
            };
            let newer = heads.get(&team.id).is_none_or(|(known_portable, known)| {
                (portable, head.created_at) > (*known_portable, known.created_at)
                    || (portable == *known_portable
                        && head.created_at == known.created_at
                        && head.event_id < known.event_id)
            });
            if newer {
                heads.insert(team.id, (portable, head));
            }
        }
        Ok(heads
            .into_iter()
            .map(|(id, (_, head))| (id, head))
            .collect())
    }

    /// Reads the list once more, never joining a read that began before
    /// this call, and returns the result.
    async fn reload(self: Arc<Self>) -> Result<Arc<TeamsView>, String> {
        let mut view = self.view.subscribe();
        let loading = |view: &TeamsView| view.status == TeamsStatus::Loading;
        let running = self.lock().loading;
        if running {
            view.wait_for(|view| !loading(view))
                .await
                .map_err(|_| CLOSED.to_string())?;
        }
        // A read that started meanwhile began after this call; wait for it.
        self.clone().refresh().await;
        let done = view
            .wait_for(|view| !loading(view))
            .await
            .map_err(|_| CLOSED.to_string())?
            .clone();
        match done.status {
            TeamsStatus::Error => Err(done.error.clone().unwrap_or_default()),
            _ => Ok(done),
        }
    }
}

impl TeamKit {
    pub fn view(&self) -> watch::Receiver<Arc<TeamsView>> {
        self.inner.view.subscribe()
    }

    pub fn snapshot(&self) -> Arc<TeamsView> {
        self.inner.view.borrow().clone()
    }

    /// The HTTP origin team records are scoped to.
    pub fn community(&self) -> &str {
        &self.inner.community
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

    /// Reads the team list again unless a read is in flight.
    pub fn refresh(&self) {
        self.inner.runtime.spawn(self.inner.clone().refresh());
    }

    /// Reads the list again and waits for the result. It never joins a
    /// read that began before this call (`refresh({after: true})`), so the
    /// result reflects every change visible when it was asked for.
    pub async fn reload(&self) -> Result<Arc<TeamsView>, String> {
        let inner = self.inner.clone();
        self.run(async move { inner.reload().await }).await
    }

    /// Reads the list once, if it was never read.
    pub fn ensure(&self) {
        if self.snapshot().status == TeamsStatus::Idle {
            self.refresh();
        }
    }

    /// Saves (or with `deleted`, tombstones) a team. `expected` is the
    /// head the edit started from; a changed head refuses the save.
    pub async fn save(
        &self,
        team: Team,
        expected: Option<String>,
        deleted: bool,
    ) -> Result<String, String> {
        self.save_resumable(team, expected, deleted, Resume::new())
            .await
    }

    /// [`Self::save`] that confirms the event `resume` holds instead of
    /// enqueueing again, and records the one it enqueues. The list is read
    /// again before it returns.
    pub async fn save_resumable(
        &self,
        team: Team,
        expected: Option<String>,
        deleted: bool,
        resume: Resume,
    ) -> Result<String, String> {
        let inner = self.inner.clone();
        self.run(async move {
            let id = inner.save(team, expected, deleted, &resume).await?;
            let _ = inner.reload().await;
            Ok(id)
        })
        .await
    }

    /// The current text head (`readText`): `None` when a fresh read proves
    /// none exists, otherwise its text (empty for a tombstone). Anything
    /// unreadable is an error, including a head this session saw that is
    /// now missing.
    pub async fn read_text(&self, team: &str) -> Result<Option<TeamText>, String> {
        let (inner, team) = (self.inner.clone(), team.to_owned());
        self.run(async move { inner.read_text(&team).await }).await
    }

    /// The current text head without its chunks (`readTextHead`), so a
    /// delete can retire a head whose text is unreadable.
    pub async fn read_text_head(&self, team: &str) -> Result<Option<TextHead>, String> {
        let (inner, team) = (self.inner.clone(), team.to_owned());
        self.run(async move {
            Ok(inner
                .text_head(&team)
                .await?
                .map(|(record, head)| TextHead {
                    head,
                    deleted: record.deleted,
                }))
        })
        .await
    }

    /// Writes and verifies the immutable chunks of one text revision
    /// (`prepareText`). The head stays untouched until
    /// [`Self::publish_text`].
    pub async fn prepare_text(
        &self,
        team: &str,
        text: &str,
        revision: &str,
    ) -> Result<TeamManifest, String> {
        let inner = self.inner.clone();
        let (team, text, revision) = (team.to_owned(), text.to_owned(), revision.to_owned());
        self.run(async move { inner.prepare_text(&team, &text, &revision).await })
            .await
    }

    /// Moves the text head to prepared chunks, or to a tombstone when
    /// `manifest` is `None` (`publishText`). `expected` is the text head
    /// the edit started from; a live head also needs `team_head`, the
    /// team's current live head.
    pub async fn publish_text(
        &self,
        team: &str,
        manifest: Option<TeamManifest>,
        expected: Option<String>,
        team_head: Option<String>,
        resume: Resume,
    ) -> Result<String, String> {
        let (inner, team) = (self.inner.clone(), team.to_owned());
        self.run(async move {
            inner
                .publish_text(&team, manifest, expected, team_head, &resume)
                .await
        })
        .await
    }

    /// Saves a team with its portable snapshot: the payload chunks first,
    /// then the manifest record. `revision` names this save, so a retry
    /// reuses the chunks already stored.
    pub async fn save_portable(
        &self,
        team: Team,
        snapshot: Value,
        expected: Option<String>,
        revision: String,
    ) -> Result<String, String> {
        let inner = self.inner.clone();
        self.run(async move {
            let id = inner
                .save_portable(team, snapshot, expected, revision)
                .await?;
            let _ = inner.reload().await;
            Ok(id)
        })
        .await
    }

    /// A portable team's saved snapshot, verified against its manifest.
    pub async fn load_team(&self, team: &Team) -> Result<Value, String> {
        let (inner, team) = (self.inner.clone(), team.clone());
        self.run(async move { inner.load(&team).await }).await
    }

    /// Every team's current head, read strong, for the host to reconcile
    /// team bindings (`current_team_members`).
    pub async fn current_members(&self) -> Result<BTreeMap<String, TeamHead>, String> {
        let inner = self.inner.clone();
        self.run(async move { inner.current_members().await }).await
    }

    /// The newest memories `agent` wrote to this viewer (kind 30174),
    /// decoded with the owner key: `{entries, partial}`.
    pub async fn agent_memories(&self, agent: &str) -> Result<Value, String> {
        let (inner, agent) = (self.inner.clone(), agent.to_owned());
        self.run(async move {
            let events = inner
                .fresh(json!({
                    "kinds": [30174],
                    "authors": [agent],
                    "#p": [inner.viewer],
                    "limit": 256,
                }))
                .await?;
            let records: Vec<Option<Value>> = events
                .iter()
                .map(|event| serde_json::to_value(event.event()).ok())
                .collect();
            inner.identity.decode_agent_memories(&agent, &records)
        })
        .await
    }
}
