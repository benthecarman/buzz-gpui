//! The session's channel kit and channel creation with setup
//! (`channel-templates/capability.ts` `createChannelKit`, `setup.ts`
//! `createChannelSetup`, `relay/session.ts` `channelCreation`).
//!
//! The kit reads the viewer's private recipes and saves them through the
//! outbox with an expected head, as buzz-app does: a save is refused when
//! the record changed, when an earlier save is unresolved, or within the
//! same second as the head. Channel creation freezes its intent in a
//! device receipt before anything is sent, returns once the channel is
//! confirmed (admission), and finishes its setup on its own afterwards
//! (completion), leaving a notice when setup could not finish.

use super::{Actor, Message, Session};
use crate::event::{Verified, kinds};
use crate::kit::setup::{
    ChannelCreationInput, PendingCreation, Progress, Receipts, SETUP_NOT_CONTINUED, SetupNotice,
    TEMPLATE_NOT_CONTINUED, pending_creation,
};
use crate::kit::{self, DECODE_BATCH, KitEntry, KitRecord, KitValue, Team, payload};
use crate::lifecycle::{self, Action, CONFIRM_DELAYS};
use crate::outbox::{Delivery, Outbox, SendOptions};
use crate::reader::{ReadOptions, Reader};
use crate::socket::Filter;
use crate::store::{Partition, Store};
use buzz_identity::Identity;
use nostr::prelude::EventId;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, watch};

const CLOSED: &str = "The session has closed";
const NOT_CONNECTED: &str = "Not connected to the relay yet";
const UNCONFIRMED: &str = "This may have taken effect, but it is not confirmed yet. Refresh and check before trying again.";
/// An unconfirmed save older than this is never replayed.
const REPLAY_SECS: u64 = 15 * 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KitStatus {
    #[default]
    Idle,
    Loading,
    Ready,
    Error,
    Unavailable,
}

/// The recipe catalog as last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KitSnapshot {
    pub status: KitStatus,
    pub entries: Arc<[KitEntry]>,
    pub error: Option<String>,
}

/// Channel creation the dialog and sidebar show: a creation awaiting
/// "Retry channel", and setups that could not finish.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreationSnapshot {
    pub pending: Option<ChannelCreationInput>,
    pub notices: Vec<SetupNotice>,
}

#[derive(Default)]
pub(super) struct KitShared {
    kit: watch::Sender<Arc<KitSnapshot>>,
    creation: watch::Sender<Arc<CreationSnapshot>>,
    /// Refreshes run one at a time; a caller during one joins its result.
    refreshing: tokio::sync::Mutex<()>,
    saving: AtomicBool,
    epoch: AtomicU64,
    /// The creation in this session whose Create is unconfirmed.
    running: Mutex<Option<Progress>>,
    /// A creation is being admitted now.
    admitting: AtomicBool,
}

impl KitShared {
    fn set_kit(&self, f: impl FnOnce(&mut KitSnapshot)) {
        self.kit.send_modify(|snapshot| {
            let mut next = (**snapshot).clone();
            f(&mut next);
            *snapshot = Arc::new(next);
        });
    }

    fn set_creation(&self) {
        let pending = self
            .running
            .lock()
            .expect("kit lock")
            .as_ref()
            .map(|p| p.input.clone());
        self.creation.send_modify(|snapshot| {
            if snapshot.pending != pending {
                let mut next = (**snapshot).clone();
                next.pending = pending;
                *snapshot = Arc::new(next);
            }
        });
    }

    fn notice(&self, notice: SetupNotice) {
        self.creation.send_modify(|snapshot| {
            let mut next = (**snapshot).clone();
            next.notices.retain(|n| n.id != notice.id);
            next.notices.push(notice);
            *snapshot = Arc::new(next);
        });
    }
}

/// What kit operations need from the actor.
#[derive(Clone)]
struct Kit {
    reader: Reader,
    identity: Arc<Identity>,
    viewer: String,
    community: String,
    relay: String,
    outbox: Outbox,
    store: Store,
    partition: Partition,
    epoch: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

async fn with<R: Send + 'static>(
    commands: &tokio::sync::mpsc::UnboundedSender<Message>,
    f: impl FnOnce(&mut Actor) -> R + Send + 'static,
) -> Result<R, String> {
    let (reply, answer) = oneshot::channel();
    commands
        .send(Message::With(Box::new(move |actor| {
            let _ = reply.send(f(actor));
        })))
        .map_err(|_| CLOSED.to_owned())?;
    answer.await.map_err(|_| CLOSED.to_owned())
}

impl Actor {
    fn kit_context(&self) -> Result<Kit, String> {
        if self.cached {
            return Err(super::channels::RECONNECT_TO_UPDATE.into());
        }
        let relay = self.relay.as_ref().ok_or(NOT_CONNECTED)?.authority.clone();
        let viewer = self.identity.pubkey_hex();
        Ok(Kit {
            reader: self.reader.clone(),
            identity: self.identity.clone(),
            community: self.origin.http(),
            partition: Partition::new(&self.origin, &viewer),
            viewer,
            relay,
            outbox: self.outbox.clone().ok_or(NOT_CONNECTED)?,
            store: self.store.clone(),
            epoch: self.epoch,
        })
    }
}

impl Kit {
    async fn read(&self, filters: Vec<Filter>) -> Result<Vec<Verified>, String> {
        self.reader
            .read(
                filters,
                ReadOptions {
                    fresh: true,
                    ..ReadOptions::default()
                },
            )
            .await
            .map(|events| events.to_vec())
            .map_err(|error| error.message)
    }

    async fn head(&self, coordinate: &str, strong: bool) -> Result<Option<Verified>, String> {
        let events = self
            .read(vec![kit::record_filter(&self.viewer, coordinate, strong)])
            .await?;
        Ok(kit::selected_head(&events).cloned())
    }

    async fn exists(&self, id: &EventId) -> Result<bool, String> {
        Ok(self
            .read(vec![kit::id_filter(id)])
            .await?
            .iter()
            .any(|event| event.id == *id))
    }

    /// `confirm`: read back exactly; replay only a recent unconfirmed
    /// save, never a refused one.
    async fn confirm(&self, id: EventId) -> Result<(), String> {
        if self.exists(&id).await? {
            return Ok(());
        }
        let Some(op) = self.outbox.get(&id) else {
            return Err(TOO_OLD.into());
        };
        if now().saturating_sub(op.created_at()) >= REPLAY_SECS {
            return Err(TOO_OLD.into());
        }
        if matches!(op.delivery, Delivery::Unknown) {
            self.outbox.retry(&id, None).map_err(|e| e.to_string())?;
        }
        delivered(&self.outbox, id).await?;
        if !self.exists(&id).await? {
            return Err("Save is awaiting exact relay confirmation; your draft is kept".into());
        }
        Ok(())
    }

    /// An unresolved earlier send for this coordinate.
    fn unresolved(&self, coordinate: &str) -> bool {
        self.outbox.visible().iter().any(|op| {
            op.kind() == kinds::APP_DATA
                && op.tag("d") == Some(coordinate)
                && !matches!(op.delivery, Delivery::Accepted | Delivery::Seen)
        })
    }
}

const TOO_OLD: &str = "This operation could not be confirmed and is too old to replay. Keep your draft and inspect the current result before making a new save.";

/// Waits until the outbox delivered `id`: accepted or seen.
async fn delivered(outbox: &Outbox, id: EventId) -> Result<(), String> {
    let mut changes = outbox.changes();
    loop {
        match outbox.get(&id) {
            Some(op) => match op.delivery {
                Delivery::Accepted | Delivery::Seen => return Ok(()),
                Delivery::Failed => {
                    return Err(op
                        .error
                        .clone()
                        .unwrap_or_else(|| "The relay refused this change.".into()));
                }
                Delivery::Unknown => return Err(UNCONFIRMED.into()),
                Delivery::Sending => {}
            },
            None => return Err("The change was dismissed.".into()),
        }
        changes.changed().await.map_err(|_| CLOSED.to_owned())?;
    }
}

impl Session {
    async fn kit_context(&self) -> Result<Kit, String> {
        with(&self.commands, |actor| actor.kit_context()).await?
    }

    /// The recipe catalog as last read (`channelKit.snapshot`).
    pub fn kit(&self) -> watch::Receiver<Arc<KitSnapshot>> {
        self.kit.kit.subscribe()
    }

    /// Reads the catalog once (`ensure`).
    pub fn kit_ensure(&self) {
        if self.kit.kit.borrow().status != KitStatus::Idle {
            return;
        }
        let (commands, shared) = (self.commands.clone(), self.kit.clone());
        self.runtime.spawn(async move {
            let _ = refresh(&commands, &shared).await;
        });
    }

    /// Reads the viewer's recipes in this community again (`refresh`). A
    /// call during a refresh waits for it.
    pub async fn kit_refresh(&self) -> Result<(), String> {
        let (commands, shared) = (self.commands.clone(), self.kit.clone());
        self.runtime
            .spawn(async move { refresh(&commands, &shared).await })
            .await
            .unwrap_or_else(|_| Err(CLOSED.into()))
    }

    /// Saves a recipe over `expected`, the head it was edited from (`None`
    /// for a new one), and returns the confirmed event (`save`). Deleting
    /// saves the record with `deleted: true`.
    pub async fn kit_save(
        &self,
        value: KitValue,
        expected: Option<EventId>,
        deleted: bool,
    ) -> Result<EventId, String> {
        let (commands, shared) = (self.commands.clone(), self.kit.clone());
        self.runtime
            .spawn(async move { save(&commands, &shared, value, expected, deleted).await })
            .await
            .unwrap_or_else(|_| Err(CLOSED.into()))
    }

    /// The portable definition of a version 2 team (`loadTeam`): its
    /// chunks, exact revision reads only, checked against the manifest.
    pub async fn kit_load_team(&self, team: Team) -> Result<Value, String> {
        let commands = self.commands.clone();
        self.runtime
            .spawn(async move {
                let ctx = with(&commands, |actor| actor.kit_context()).await??;
                load_team(&ctx, &team).await
            })
            .await
            .unwrap_or_else(|_| Err(CLOSED.into()))
    }

    /// Saves a team with its portable definition (`savePortable`): every
    /// chunk confirmed and read back before the manifest replaces the old
    /// one. `revision` stays the same across retries of one edit.
    pub async fn kit_save_portable(
        &self,
        team: Team,
        snapshot: Value,
        expected: Option<EventId>,
        revision: String,
    ) -> Result<EventId, String> {
        let (commands, shared) = (self.commands.clone(), self.kit.clone());
        self.runtime
            .spawn(async move {
                let ctx = with(&commands, |actor| actor.kit_context()).await??;
                let members = snapshot["members"].as_array().map_or(0, Vec::len);
                if team.agents.is_empty()
                    || team.agents.len() != members
                    || snapshot["team"]["name"].as_str() != Some(team.name.as_str())
                {
                    return Err("Portable team members do not match their definitions".into());
                }
                let (manifest, payloads) = payload::encode_team_payload(
                    &snapshot,
                    &ctx.community,
                    &ctx.viewer,
                    &team.id,
                    &revision,
                )?;
                ctx.outbox.ready().await.map_err(|e| e.to_string())?;
                for chunk in &payloads {
                    let record = chunk.record();
                    let coordinate = chunk.coordinate();
                    if let Some(existing) = ctx.head(&coordinate, true).await? {
                        let raw = kit::open(&ctx.identity, &existing, &ctx.community)?;
                        if payload::parse_payload_record(&raw, &ctx.community)? != *chunk {
                            return Err(
                                "Portable team revision already has different content".into()
                            );
                        }
                        continue;
                    }
                    let pending = ctx
                        .outbox
                        .visible()
                        .iter()
                        .find(|op| op.kind() == kinds::APP_DATA && op.tag("d") == Some(&coordinate))
                        .map(|op| op.id());
                    let id = match pending {
                        Some(id) => id,
                        None => ctx
                            .outbox
                            .send(
                                crate::messages::Draft {
                                    kind: kinds::APP_DATA,
                                    content: kit::seal(&ctx.identity, &record, &ctx.community)?,
                                    tags: vec![
                                        vec!["d".into(), coordinate.clone()],
                                        vec!["t".into(), kit::PAYLOAD_TAG.into()],
                                    ],
                                },
                                SendOptions::default(),
                            )
                            .map_err(|e| e.to_string())?,
                    };
                    ctx.confirm(id).await?;
                }
                let team = Team {
                    portable: Some(manifest),
                    ..team
                };
                // The whole payload must load before the manifest moves.
                load_team(&ctx, &team).await?;
                save(&commands, &shared, KitValue::Team(team), expected, false).await
            })
            .await
            .unwrap_or_else(|_| Err(CLOSED.into()))
    }

    /// The selected Canvas of a channel, if any (`canvas.read`, not
    /// strongly consistent): "Save as template" copies it.
    pub async fn kit_canvas(&self, channel: &str) -> Result<Option<String>, String> {
        let (commands, channel) = (self.commands.clone(), channel.to_owned());
        self.runtime
            .spawn(async move {
                let ctx = with(&commands, |actor| actor.kit_context()).await??;
                Ok(canvas_head(&ctx, &channel, false)
                    .await?
                    .map(|event| event.content.clone()))
            })
            .await
            .unwrap_or_else(|_| Err(CLOSED.into()))
    }

    /// Channel creation state: the creation awaiting "Retry channel" in
    /// this session, and setup notices (`channelCreation`).
    pub fn creation(&self) -> watch::Receiver<Arc<CreationSnapshot>> {
        self.kit.creation.subscribe()
    }

    /// Hides a setup notice; the channel and its receipts are unchanged.
    pub fn dismiss_setup_notice(&self, id: &str) {
        self.kit.creation.send_modify(|snapshot| {
            if snapshot.notices.iter().any(|n| n.id == id) {
                let mut next = (**snapshot).clone();
                next.notices.retain(|n| n.id != id);
                *snapshot = Arc::new(next);
            }
        });
    }

    /// The creation a "Retry channel" would check: this session's
    /// unconfirmed one, else one the outbox kept from before a restart
    /// (`channelCreation.snapshot`).
    pub async fn pending_creation(&self) -> Option<ChannelCreationInput> {
        if let Some(running) = self.kit.running.lock().expect("kit lock").as_ref() {
            return Some(running.input.clone());
        }
        let running = None;
        with(&self.commands, move |actor| restored(actor, running))
            .await
            .ok()
            .flatten()
            .map(|pending| pending.input)
    }

    /// Creates a channel with its setup (`channelCreation.create`) and
    /// returns its id once the viewer is confirmed in it. Group, Canvas and
    /// agents then continue on their own; a failure there becomes a
    /// notice. With an unconfirmed creation pending, this retries only that
    /// creation, whatever `input` says.
    pub async fn create_channel_with(
        self: &Arc<Self>,
        input: ChannelCreationInput,
    ) -> Result<String, String> {
        let mut input = input;
        input.name = lifecycle::canonical_name(&input.name);
        let session = self.clone();
        let (sender, admission) = oneshot::channel();
        self.runtime.spawn(async move {
            let shared = session.kit.clone();
            let name = input.name.clone();
            let mut admitted = Admitted {
                sender: Some(sender),
                opened: None,
            };
            if let Err(error) = run(&session, input, &mut admitted).await {
                match (admitted.sender.take(), admitted.opened) {
                    (Some(sender), _) => {
                        let _ = sender.send(Err(error));
                    }
                    // Admitted: the form is gone, so setup reports here.
                    (None, Some(id)) => shared.notice(SetupNotice { id, name, error }),
                    (None, None) => {}
                }
            }
        });
        admission.await.unwrap_or_else(|_| Err(CLOSED.into()))
    }
}

/// The two outcomes of one run: the form's admission, then completion.
struct Admitted {
    sender: Option<oneshot::Sender<Result<String, String>>>,
    /// The channel, once admitted.
    opened: Option<String>,
}

fn admit(admitted: &mut Admitted, id: &str) {
    admitted.opened = Some(id.to_owned());
    if let Some(sender) = admitted.sender.take() {
        let _ = sender.send(Ok(id.to_owned()));
    }
}

/// `restoredChannelCreation`: the newest 9007 the outbox still holds for a
/// channel this device did not admit and the viewer is not in.
fn restored(actor: &Actor, running: Option<String>) -> Option<PendingCreation> {
    let outbox = actor.outbox.as_ref()?;
    let viewer = actor.identity.pubkey_hex();
    let receipts =
        Receipts::load(&actor.store, &Partition::new(&actor.origin, &viewer)).unwrap_or_default();
    outbox
        .visible()
        .iter()
        .rev()
        .filter_map(|op| pending_creation(op).map(|pending| (op.delivery, pending)))
        .find(|(delivery, pending)| {
            let unsettled = matches!(
                delivery,
                Delivery::Sending | Delivery::Unknown | Delivery::Failed
            ) || receipts.0.get(&pending.id).is_some_and(|p| !p.created);
            unsettled
                && running.as_deref() != Some(pending.id.as_str())
                && !receipts.admitted(&pending.id)
                && !actor
                    .discovery
                    .as_ref()
                    .is_some_and(|discovery| discovery.can_access(&pending.id))
        })
        .map(|(_, pending)| pending)
}

async fn refresh(
    commands: &tokio::sync::mpsc::UnboundedSender<Message>,
    shared: &Arc<KitShared>,
) -> Result<(), String> {
    let _guard = shared.refreshing.lock().await;
    let generation = shared.epoch.load(Ordering::SeqCst);
    shared.set_kit(|s| {
        s.status = KitStatus::Loading;
        s.error = None;
    });
    let result = async {
        let ctx = with(commands, |actor| actor.kit_context()).await??;
        let events = ctx.read(vec![kit::catalog_filter(&ctx.viewer)]).await?;
        if events.len() >= kit::CATALOG_LIMIT {
            return Err(
                "Recipe catalog reached its read limit; no partial catalog will be used".into(),
            );
        }
        let heads = kit::catalog_heads(&events, &ctx.community);
        let mut entries = Vec::with_capacity(heads.len());
        for batch in heads.chunks(DECODE_BATCH) {
            for event in batch {
                entries.push(kit::decode_entry(&ctx.identity, event, &ctx.community)?);
            }
        }
        Ok::<_, String>(entries)
    }
    .await;
    if shared.epoch.load(Ordering::SeqCst) != generation {
        return Ok(());
    }
    match result {
        Ok(entries) => {
            shared.set_kit(|s| {
                s.status = KitStatus::Ready;
                s.entries = entries.into();
                s.error = None;
            });
            Ok(())
        }
        Err(error) => {
            // Before the relay answers there is nothing to read yet: the
            // next `kit_ensure` reads again.
            let waiting = error == NOT_CONNECTED;
            shared.set_kit(|s| {
                s.status = if waiting {
                    KitStatus::Idle
                } else {
                    KitStatus::Error
                };
                s.error = (!waiting).then(|| error.clone());
            });
            Err(error)
        }
    }
}

async fn save(
    commands: &tokio::sync::mpsc::UnboundedSender<Message>,
    shared: &Arc<KitShared>,
    value: KitValue,
    expected: Option<EventId>,
    deleted: bool,
) -> Result<EventId, String> {
    let ctx = with(commands, |actor| actor.kit_context()).await??;
    let record = kit::parse_kit_record(
        &KitRecord::new(value, &ctx.community, deleted).to_json(),
        &ctx.community,
    )?;
    if shared.saving.swap(true, Ordering::SeqCst) {
        return Err("Recipe saving is unavailable or already in progress".into());
    }
    let result = async {
        ctx.outbox.ready().await.map_err(|e| e.to_string())?;
        let coordinate = record.coordinate();
        let head = ctx.head(&coordinate, false).await?;
        if head.as_ref().map(|head| head.id) != expected {
            return Err("This saved recipe changed. Refresh the catalog and review your draft before replacing it.".into());
        }
        if ctx.unresolved(&coordinate) {
            return Err("A save for this recipe is unresolved. Inspect Outbox and refresh before replacing it.".into());
        }
        // Replaceable records use seconds: no ambiguous equal-time replacement.
        if head.is_some_and(|head| head.created_at_secs() >= now()) {
            return Err("Please wait a second before saving this recipe again".into());
        }
        let draft = kit::draft(&ctx.identity, &record)?;
        let id = ctx
            .outbox
            .send(draft, SendOptions::default())
            .map_err(|e| e.to_string())?;
        ctx.confirm(id).await?;
        if ctx.head(&coordinate, false).await?.map(|head| head.id) != Some(id) {
            return Err("Another recipe save is selected. Your draft is kept; refresh and review before saving again.".into());
        }
        Ok::<_, String>(id)
    }
    .await;
    shared.saving.store(false, Ordering::SeqCst);
    let id = result?;
    let _ = refresh(commands, shared).await;
    Ok(id)
}

async fn load_team(ctx: &Kit, team: &Team) -> Result<Value, String> {
    let manifest = team
        .portable
        .as_ref()
        .ok_or("This team has no portable definition")?;
    if manifest.owner != ctx.viewer {
        return Err("Portable team belongs to another viewer".into());
    }
    let mut payloads = Vec::with_capacity(manifest.chunks as usize);
    for coordinate in payload::chunk_coordinates(manifest, &ctx.community, &team.id) {
        let event = ctx
            .head(&coordinate, true)
            .await?
            .ok_or("Portable team payload is unavailable")?;
        let raw = kit::open(&ctx.identity, &event, &ctx.community)?;
        let chunk = payload::parse_payload_record(&raw, &ctx.community)?;
        if chunk.coordinate() != coordinate || event.tag("d") != Some(coordinate.as_str()) {
            return Err("Portable team payload coordinate mismatch".into());
        }
        payloads.push(chunk);
    }
    payload::decode_team_payload(manifest, &payloads, &ctx.community, &ctx.viewer, &team.id)
}

/// The selected Canvas head of a channel.
async fn canvas_head(ctx: &Kit, channel: &str, strong: bool) -> Result<Option<Verified>, String> {
    let mut filter = Filter::new();
    filter.insert("kinds".into(), json!([kinds::CANVAS]));
    filter.insert("#h".into(), json!([channel]));
    filter.insert("limit".into(), json!(1));
    if strong {
        filter.insert("consistency".into(), json!("strong"));
    }
    let events = ctx.read(vec![filter]).await?;
    Ok(kit::selected_head(&events).cloned())
}

/// Seeds a new channel's starting Canvas through the outbox with
/// `expected-revision: none`, and returns the operation.
fn seed_canvas(ctx: &Kit, channel: &str, content: &str) -> Result<EventId, String> {
    let draft = crate::canvas::save_draft(channel, content, None).map_err(|e| e.to_string())?;
    ctx.outbox
        .send(draft, SendOptions::default())
        .map_err(|e| e.to_string())
}

/// Reads the roster until it lists the viewer in `channel`, then feeds
/// the events to discovery.
async fn confirm_created(
    session: &Session,
    ctx: &Kit,
    channel: &str,
    member: &str,
) -> Result<(), String> {
    let filters = lifecycle::created_filters(&ctx.relay, member, channel);
    let mut found = None;
    for delay in CONFIRM_DELAYS {
        tokio::time::sleep(delay).await;
        let events = ctx.read(filters.clone()).await?;
        if lifecycle::confirmed(Action::Join, &events, &ctx.relay, member, channel) {
            found = Some(events);
            break;
        }
    }
    let events = found.ok_or(UNCONFIRMED)?;
    let epoch = ctx.epoch;
    with(&session.commands, move |actor| {
        if actor.epoch == epoch {
            actor.accept(&events);
        }
    })
    .await
}

/// Waits for a 9007 and the viewer's membership; an uncertain delivery
/// still counts when the relay holds the exact command.
async fn admitted_creation(
    session: &Session,
    ctx: &Kit,
    op: EventId,
    channel: &str,
) -> Result<(), String> {
    if let Err(error) = delivered(&ctx.outbox, op).await {
        let receipt = ctx
            .read(vec![lifecycle::creation_receipt_filter(&op, &ctx.viewer)])
            .await?;
        if !receipt.iter().any(|event| event.id == op) {
            return Err(error);
        }
    }
    confirm_created(session, ctx, channel, &ctx.viewer).await
}

/// One creation from start to finish (`run`).
async fn run(
    session: &Arc<Session>,
    input: ChannelCreationInput,
    admitted: &mut Admitted,
) -> Result<(), String> {
    let shared = session.kit.clone();
    let ctx = session.kit_context().await?;
    ctx.outbox.ready().await.map_err(|e| e.to_string())?;
    let running = shared.running.lock().expect("kit lock").clone();
    if let Some(checking) = running {
        return retry_running(session, &ctx, checking, admitted).await;
    }
    let running_id = None;
    let restored = with(&session.commands, move |actor| restored(actor, running_id)).await?;
    if let Some(pending) = restored {
        return retry_restored(session, &ctx, pending, admitted).await;
    }
    if shared.admitting.swap(true, Ordering::SeqCst) {
        return Err("Another channel is still being created.".into());
    }
    let result = start(session, &ctx, input, admitted).await;
    shared.admitting.store(false, Ordering::SeqCst);
    result
}

/// `preflight` and the first steps up to admission, then setup.
async fn start(
    session: &Arc<Session>,
    ctx: &Kit,
    input: ChannelCreationInput,
    admitted: &mut Admitted,
) -> Result<(), String> {
    input.check()?;
    if let Some(setup) = &input.setup {
        if !setup.canvas.is_empty() && crate::canvas::too_large(&setup.canvas) {
            return Err("Canvas writing is unavailable".into());
        }
        if setup.agents.iter().any(|key| !kit::hex_key(key)) {
            return Err("Agent membership is unavailable".into());
        }
        if !setup.group_id.is_empty() && setup.group_source.as_deref() != Some("legacy") {
            return Err(
                "The destination group is unavailable; refresh your sidebar before creating the channel"
                    .into(),
            );
        }
    }
    let mut p = Progress::new(input);
    // Reserve the frozen intent before anything is sent.
    Receipts::write(&ctx.store, &ctx.partition, &p)?;
    *session.kit.running.lock().expect("kit lock") = Some(p.clone());
    session.kit.set_creation();
    let shared = session.kit.clone();
    let outcome = async {
        let draft = lifecycle::create(&p.input.channel(), &p.id).map_err(|e| e.to_string())?;
        let op = ctx
            .outbox
            .send(draft, SendOptions::default())
            .map_err(|e| e.to_string())?;
        p.operations.insert("create".into(), op.to_hex());
        Receipts::write(&ctx.store, &ctx.partition, &p)?;
        admitted_creation(session, ctx, op, &p.id).await?;
        Ok::<EventId, String>(op)
    }
    .await;
    match outcome {
        Ok(_) => {}
        Err(error) => {
            let create = p
                .operations
                .get("create")
                .and_then(|id| EventId::from_hex(id).ok());
            let failed = create
                .and_then(|id| ctx.outbox.get(&id))
                .is_some_and(|op| op.delivery == Delivery::Failed);
            if create.is_none() || failed {
                retire(ctx, &p).await?;
                *shared.running.lock().expect("kit lock") = None;
            } else {
                // Uncertain: "Retry channel" checks this exact Create.
                *shared.running.lock().expect("kit lock") = Some(p.clone());
            }
            shared.set_creation();
            return Err(error);
        }
    }
    p.created = true;
    *shared.running.lock().expect("kit lock") = None;
    shared.set_creation();
    admit(admitted, &p.id);
    Receipts::write(&ctx.store, &ctx.partition, &p)?;
    complete(session, ctx, p).await
}

/// Group, Canvas and agents after admission; then the receipt and its
/// delivery evidence are retired.
async fn complete(session: &Arc<Session>, ctx: &Kit, mut p: Progress) -> Result<(), String> {
    let persist = |p: &Progress| Receipts::write(&ctx.store, &ctx.partition, p);
    let Some(setup) = p.input.setup.clone() else {
        return retire(ctx, &p).await;
    };
    let mut placement_error = None;
    if !setup.group_id.is_empty() {
        let placed = session
            .sidebar_move(
                &p.id,
                crate::sidebar_store::Destination::Section(Some(setup.group_id.clone())),
            )
            .await;
        if let Err(error) = placed {
            placement_error = Some(error);
        }
    }
    if placement_error.is_none() {
        p.group_done = true;
        persist(&p)?;
    }
    let rest = async {
        if !setup.canvas.is_empty() && !p.canvas_done {
            let seeded = p.operations.get("canvas").cloned();
            let head = canvas_head(ctx, &p.id, true).await?.map(|e| e.id.to_hex());
            if head.is_some() && head != seeded {
                return Err("Canvas has a different saved document; no agents were added by this attempt.".to_owned());
            }
            let canvas = match seeded.and_then(|id| EventId::from_hex(&id).ok()) {
                Some(id) => id,
                None => {
                    let id = seed_canvas(ctx, &p.id, &setup.canvas)?;
                    p.operations.insert("canvas".into(), id.to_hex());
                    persist(&p)?;
                    id
                }
            };
            if let Err(error) = ctx.confirm(canvas).await {
                let refused = ctx
                    .outbox
                    .get(&canvas)
                    .is_some_and(|op| crate::canvas::is_definitive_conflict(&op));
                if refused {
                    // A refused seed must not fence a later manual Canvas save.
                    let _ = ctx.outbox.dismiss(&canvas).await;
                    return Err("Canvas changed before the seed was saved; no agents were added by this attempt.".into());
                }
                return Err(error);
            }
            if canvas_head(ctx, &p.id, true).await?.map(|e| e.id) != Some(canvas) {
                return Err("The seed Canvas is not the selected document; no agents were added by this attempt.".into());
            }
            p.canvas_done = true;
            persist(&p)?;
        }
        for agent in &setup.agents {
            if *agent == ctx.viewer || p.added.contains(agent) {
                continue;
            }
            session.add_member(&p.id, agent).await?;
            if !p.accepted.contains(agent) {
                p.accepted.push(agent.clone());
            }
            p.added.push(agent.clone());
            persist(&p)?;
        }
        Ok(())
    }
    .await;
    match (placement_error, rest) {
        (Some(placement), Err(error)) => Err(format!("{placement}; {error}")),
        (Some(placement), Ok(())) => Err(placement),
        (None, Err(error)) => Err(error),
        (None, Ok(())) => retire(ctx, &p).await,
    }
}

/// Removes the receipt and the delivery evidence of its operations.
async fn retire(ctx: &Kit, p: &Progress) -> Result<(), String> {
    for id in p.operations.values() {
        let Ok(id) = EventId::from_hex(id) else {
            continue;
        };
        ctx.outbox.dismiss(&id).await.map_err(|e| e.to_string())?;
        if ctx.outbox.get(&id).is_some() {
            return Err("Channel setup delivery evidence could not be retired".into());
        }
    }
    Receipts::remove(&ctx.store, &ctx.partition, &p.id)
}

/// "Retry channel" in this session: only the saved Create is checked or
/// resent, never a new channel or the remaining setup.
async fn retry_running(
    session: &Arc<Session>,
    ctx: &Kit,
    mut checking: Progress,
    admitted: &mut Admitted,
) -> Result<(), String> {
    let shared = session.kit.clone();
    let op = checking
        .operations
        .get("create")
        .and_then(|id| EventId::from_hex(id).ok())
        .ok_or("Channel creation was not saved. Reconnect before checking it.")?;
    if let Some(saved) = ctx.outbox.get(&op)
        && matches!(saved.delivery, Delivery::Unknown)
    {
        ctx.outbox.retry(&op, None).map_err(|e| e.to_string())?;
    }
    if let Err(error) = admitted_creation(session, ctx, op, &checking.id).await {
        if ctx
            .outbox
            .get(&op)
            .is_some_and(|saved| saved.delivery == Delivery::Failed)
        {
            retire(ctx, &checking).await?;
            *shared.running.lock().expect("kit lock") = None;
            shared.set_creation();
        }
        return Err(error);
    }
    checking.created = true;
    *shared.running.lock().expect("kit lock") = None;
    shared.set_creation();
    admit(admitted, &checking.id);
    Receipts::write(&ctx.store, &ctx.partition, &checking)?;
    if checking.input.setup.is_some() {
        return Err(TEMPLATE_NOT_CONTINUED.into());
    }
    retire(ctx, &checking).await
}

/// "Retry channel" after a restart: the outbox's own 9007, confirmed and
/// then dismissed. A saved setup is reported, never continued.
async fn retry_restored(
    session: &Arc<Session>,
    ctx: &Kit,
    pending: PendingCreation,
    admitted: &mut Admitted,
) -> Result<(), String> {
    if let Some(saved) = ctx.outbox.get(&pending.operation)
        && saved.delivery == Delivery::Unknown
    {
        ctx.outbox
            .retry(&pending.operation, None)
            .map_err(|e| e.to_string())?;
    }
    if let Err(error) = admitted_creation(session, ctx, pending.operation, &pending.id).await {
        if ctx
            .outbox
            .get(&pending.operation)
            .is_some_and(|saved| saved.delivery == Delivery::Failed)
        {
            let _ = ctx.outbox.dismiss(&pending.operation).await;
        }
        return Err(error);
    }
    let _ = ctx.outbox.dismiss(&pending.operation).await;
    admit(admitted, &pending.id);
    let receipts = Receipts::load(&ctx.store, &ctx.partition).unwrap_or_default();
    if receipts.0.contains_key(&pending.id) {
        return Err(SETUP_NOT_CONTINUED.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn snapshots_start_idle_and_empty() {
        let shared = KitShared::default();
        assert_eq!(shared.kit.borrow().status, KitStatus::Idle);
        assert!(shared.creation.borrow().pending.is_none());
        shared.notice(SetupNotice {
            id: "a".into(),
            name: "x".into(),
            error: "e".into(),
        });
        shared.notice(SetupNotice {
            id: "a".into(),
            name: "x".into(),
            error: "f".into(),
        });
        assert_eq!(shared.creation.borrow().notices.len(), 1, "one per channel");
    }
}
