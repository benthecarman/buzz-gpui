//! The viewer's saved teams in one community (`capability.ts`
//! `createChannelKit`, team operations only): reads, saves through the
//! session's durable outbox, portable payloads and the team heads the
//! agent host reconciles its bindings against.

use super::{Message, Session};
use crate::event::Verified;
use crate::messages::Draft;
use crate::outbox::{Delivery, Outbox, SendOptions};
use crate::reader::{ReadOptions, Reader};
use crate::socket::Filter;
use crate::teams::{
    KIT_TAG, MANIFEST_TAG, ME_KIT_TAG, Team, TeamEntry, TeamHead, check_manifest, decode_payload,
    decode_record, encode_payload, in_catalog, newest, parse_team, payload_record, record_tags,
    team_record,
};
use buzz_identity::Identity;
use nostr::prelude::EventId;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
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
    epoch: u64,
}

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
        let mut heads: HashMap<String, Vec<Verified>> = HashMap::new();
        for event in events {
            let d = event
                .tags
                .iter()
                .map(|tag| tag.as_slice())
                .find(|tag| tag.first().map(String::as_str) == Some("d"))
                .and_then(|tag| tag.get(1))
                .cloned();
            if let Some(d) = d.filter(|d| in_catalog(d, &self.community)) {
                heads.entry(d).or_default().push(event);
            }
        }
        Ok(heads
            .into_values()
            .filter_map(|events| newest(&events).cloned())
            .collect())
    }

    async fn read_entries(&self) -> Result<Vec<TeamEntry>, String> {
        let mut entries = Vec::new();
        for event in self.heads(false).await? {
            let raw = decode_record(&self.identity, &event, &self.community)?;
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
        Ok(entries)
    }

    async fn refresh(self: Arc<Self>) {
        let epoch = {
            let mut state = self.lock();
            if state.loading {
                return;
            }
            state.loading = true;
            state.epoch
        };
        self.update(|view| {
            view.status = TeamsStatus::Loading;
            view.error = None;
        });
        let result = self.read_entries().await;
        let current = {
            let mut state = self.lock();
            state.loading = false;
            state.epoch == epoch
        };
        if current {
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
        }
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
        Ok(newest(&events).cloned())
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
        let mut changes = outbox.changes();
        let delivered = tokio::time::timeout(DELIVERY_WAIT, async {
            loop {
                match outbox.get(&id).map(|op| (op.delivery, op.error.clone())) {
                    Some((Delivery::Accepted | Delivery::Seen, _)) => return Ok(()),
                    Some((Delivery::Failed, error)) => {
                        return Err(error.unwrap_or_else(|| "The relay refused this save.".into()));
                    }
                    None => return Err("The save was dismissed.".to_string()),
                    _ => {}
                }
                if changes.changed().await.is_err() {
                    return Err(CLOSED.to_string());
                }
            }
        })
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
    ) -> Result<String, String> {
        {
            let mut state = self.lock();
            if state.saving {
                return Err("Recipe saving is unavailable or already in progress".into());
            }
            state.saving = true;
        }
        let result = self.save_unguarded(team, expected, deleted).await;
        self.lock().saving = false;
        result
    }

    async fn save_unguarded(
        &self,
        team: Team,
        expected: Option<String>,
        deleted: bool,
    ) -> Result<String, String> {
        let raw = team_record(&team, &self.community, deleted);
        let tags = record_tags(&raw, &self.community)?;
        let d = tags[0][1].clone();
        let outbox = self.outbox().await?;
        let head = self.read_coordinate(&d, false).await?;
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
        self.confirm(&outbox, id).await?;
        if self.read_coordinate(&d, false).await?.map(|e| e.id) != Some(id) {
            return Err("Another recipe save is selected. Your draft is kept; refresh and review before saving again.".into());
        }
        Ok(id.to_hex())
    }

    async fn load(&self, team: &Team) -> Result<Value, String> {
        let manifest = team
            .portable
            .as_ref()
            .ok_or("This team has no portable definition")?;
        check_manifest(manifest)?;
        if manifest.owner != self.viewer {
            return Err("Portable team belongs to another viewer".into());
        }
        let mut payloads = Vec::with_capacity(manifest.chunks as usize);
        for index in 0..manifest.chunks as usize {
            let record = payload_record(
                &self.community,
                &self.viewer,
                &team.id,
                &manifest.revision,
                index,
                String::new(),
            );
            // Exact revision reads only; chunks are never discovered.
            let coordinate = record_tags(&record, &self.community)?
                .swap_remove(0)
                .swap_remove(1);
            let event = self
                .read_coordinate(&coordinate, true)
                .await?
                .ok_or("Portable team payload is unavailable")?;
            let raw = decode_record(&self.identity, &event, &self.community)?;
            if raw["value"]["type"] != "team-payload"
                || record_tags(&raw, &self.community)?[0][1] != coordinate
            {
                return Err("Portable team payload coordinate mismatch".into());
            }
            payloads.push(raw["value"].clone());
        }
        decode_payload(manifest, &payloads, &self.community, &self.viewer, &team.id)
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
        let (manifest, payloads) = encode_payload(
            &snapshot,
            &self.community,
            &self.viewer,
            &team.id,
            &revision,
        )?;
        let outbox = self.outbox().await?;
        for payload in payloads {
            let tags = record_tags(&payload, &self.community)?;
            let d = tags[0][1].clone();
            if let Some(existing) = self.read_coordinate(&d, true).await? {
                let saved = decode_record(&self.identity, &existing, &self.community)?;
                if saved != payload {
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
                            content: self.encrypt(&payload)?,
                            tags,
                        },
                        SendOptions::default(),
                    )
                    .map_err(|error| error.to_string())?,
            };
            self.confirm(&outbox, id).await?;
        }
        let team = Team {
            portable: Some(manifest),
            ..team
        };
        // Confirm the whole payload before the manifest points at it.
        self.load(&team).await?;
        self.save(team, expected, false).await
    }

    async fn current_members(&self) -> Result<BTreeMap<String, TeamHead>, String> {
        let mut heads: BTreeMap<String, TeamHead> = BTreeMap::new();
        for event in self.heads(true).await? {
            let raw = decode_record(&self.identity, &event, &self.community)?;
            let Some((team, deleted)) = parse_team(&raw) else {
                continue;
            };
            let head = TeamHead {
                created_at: event.created_at_secs(),
                event_id: event.id.to_hex(),
                members: if deleted { Vec::new() } else { team.agents },
            };
            let newer = heads.get(&team.id).is_none_or(|known| {
                head.created_at > known.created_at
                    || (head.created_at == known.created_at && head.event_id < known.event_id)
            });
            if newer {
                heads.insert(team.id, head);
            }
        }
        Ok(heads)
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

    /// Reads the list again and waits for the result.
    pub async fn reload(&self) -> Result<Arc<TeamsView>, String> {
        let inner = self.inner.clone();
        self.run(async move {
            inner.clone().refresh().await;
            let mut view = inner.view.subscribe();
            let done = view
                .wait_for(|view| view.status != TeamsStatus::Loading)
                .await
                .map_err(|_| CLOSED.to_string())?
                .clone();
            match done.status {
                TeamsStatus::Error => Err(done.error.clone().unwrap_or_default()),
                _ => Ok(done),
            }
        })
        .await
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
        let inner = self.inner.clone();
        let id = self
            .run(async move { inner.save(team, expected, deleted).await })
            .await?;
        self.refresh();
        Ok(id)
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
        let id = self
            .run(async move {
                inner
                    .save_portable(team, snapshot, expected, revision)
                    .await
            })
            .await?;
        self.refresh();
        Ok(id)
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
