//! Channel administration, lifecycle, direct messages and search for one
//! session (`docs/port/relay.md` §4.2, §5e, §5g, §5m).
//!
//! Each operation runs as a task on the I/O runtime. It reads what it needs
//! from the actor, does its reads and writes, and hands relay-signed results
//! back to the actor, which feeds them through discovery like any other
//! event. Results started before a revocation are fenced by the access
//! epoch. Writes that confirm by polling use [`lifecycle::CONFIRM_DELAYS`]
//! within [`lifecycle::RUN_TIMEOUT`].

use super::{Actor, Message, Session};
use crate::discovery::{ChannelSummary, ResolutionStart, metadata_name, open_metadata};
use crate::dm;
use crate::event::{Verified, kinds};
use crate::http::RelayHttp;
use crate::lifecycle::{
    self, Action, CONFIRM_DELAYS, ChannelDetails, ChannelState, DetailsDraft, LifecyclePermissions,
    NewChannel, RUN_TIMEOUT,
};
use crate::members::{self, MemberChange, Person};
use crate::messages::EventTemplate;
use crate::outbox::{Delivery, Outbox, PublishError, Recovery, SendOptions, accept_publish};
use crate::reader::{ReadOptions, Reader};
use crate::search::{self, Author, SearchHit, SearchRequest};
use crate::socket::Filter;
use buzz_identity::{Identity, Purpose};
use nostr::prelude::{Event, EventId};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

const CLOSED: &str = "The session has closed";
const NOT_CONNECTED: &str = "Not connected to the relay yet";
const UNCONFIRMED: &str = "This may have taken effect, but it is not confirmed yet. Refresh and check before trying again.";
/// Public channels read for a name search (`PUBLIC_CHANNEL_PAGE`).
const PUBLIC_CHANNEL_PAGE: usize = 500;
/// Public channel matches kept per search.
const PUBLIC_CHANNEL_MATCHES: usize = 8;

/// A message search as typed: free text, a raw `from:` value (key or
/// name), a channel id and time bounds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageSearch {
    pub text: String,
    pub from: Option<String>,
    pub channel: Option<String>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    /// The viewer's selectable agents, `(pubkey, agent name)`. A `from:`
    /// name also matches an agent by its own name, as the author picker
    /// does.
    pub agents: Vec<(String, String)>,
}

/// Public channels found by name, as read-only previews.
#[derive(Clone, Debug, Default)]
pub struct PublicChannels {
    pub channels: Vec<ChannelSummary>,
    /// The relay returned a full page of channel metadata, so matches past
    /// it are missing.
    pub partial: bool,
}

/// Hits in relay relevance order, every one in a channel the viewer can
/// read now.
#[derive(Clone, Debug, Default)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    /// The hits' channels, joined or previewable.
    pub channels: Vec<ChannelSummary>,
    /// The `from:` name matched several people.
    pub ambiguous_author: bool,
}

/// One page of people from a profile search.
#[derive(Clone, Debug, Default)]
pub struct PeoplePage {
    pub people: Vec<Person>,
    pub more: bool,
}

/// What an operation needs from the actor to work on its own.
#[derive(Clone)]
struct Context {
    reader: Reader,
    http: RelayHttp,
    identity: Arc<Identity>,
    viewer: String,
    relay: String,
    epoch: u64,
    outbox: Outbox,
}

/// The cheap, cloneable half of a session that operations run against.
#[derive(Clone)]
struct Ops {
    commands: mpsc::UnboundedSender<Message>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl Session {
    fn ops(&self) -> Ops {
        Ops {
            commands: self.commands.clone(),
        }
    }

    /// Runs `work` on the I/O runtime, whatever executor awaits it.
    pub(super) async fn run<R: Send + 'static>(
        &self,
        work: impl Future<Output = Result<R, String>> + Send + 'static,
    ) -> Result<R, String> {
        self.runtime
            .spawn(async move {
                tokio::time::timeout(RUN_TIMEOUT, work)
                    .await
                    .unwrap_or_else(|_| Err(UNCONFIRMED.to_owned()))
            })
            .await
            .unwrap_or_else(|_| Err(CLOSED.to_owned()))
    }

    /// Reads profiles for these people if they are not known yet.
    pub fn want_profiles(&self, people: Vec<String>) {
        let _ = self.commands.send(Message::With(Box::new(move |actor| {
            actor.ensure_profiles(people);
        })));
    }

    /// The channel's validated relay state: roles, permissions and details.
    pub async fn channel_state(&self, channel: &str) -> Result<ChannelState, String> {
        let (ops, channel) = (self.ops(), channel.to_owned());
        self.run(async move {
            let ctx = ops.context().await?;
            ops.state(&ctx, &channel).await
        })
        .await
    }

    /// What the viewer may do to the channel as a whole, including what an
    /// owner agent of theirs grants (`channel-lifecycle.ts` `load`).
    pub async fn channel_permissions(&self, channel: &str) -> Result<LifecyclePermissions, String> {
        let (ops, channel) = (self.ops(), channel.to_owned());
        self.run(async move {
            let ctx = ops.context().await?;
            ops.permissions(&ctx, &channel).await
        })
        .await
    }

    /// The editable details, with the version a save must still match.
    pub async fn channel_details(&self, channel: &str) -> Result<ChannelDetails, String> {
        let state = self.channel_state(channel).await?;
        state
            .details(&self.viewer)
            .map_err(|error| error.to_string())
    }

    /// Saves details edited from `version`; refused when the channel
    /// changed since.
    pub async fn save_channel_details(
        &self,
        channel: &str,
        version: EventId,
        draft: DetailsDraft,
    ) -> Result<(), String> {
        let (ops, channel) = (self.ops(), channel.to_owned());
        self.run(async move { ops.save_details(&channel, version, draft).await })
            .await
    }

    /// Joins, leaves, archives, unarchives, deletes or hides, and waits
    /// until the relay shows it.
    pub async fn channel_action(&self, action: Action, channel: &str) -> Result<(), String> {
        let (ops, channel) = (self.ops(), channel.to_owned());
        self.run(async move { ops.lifecycle(action, &channel).await })
            .await
    }

    /// Creates a stream through the outbox and returns its id once the
    /// viewer is confirmed as its owner.
    pub async fn create_channel(&self, channel: NewChannel) -> Result<String, String> {
        let ops = self.ops();
        self.run(async move { ops.create(channel).await }).await
    }

    /// Adds someone to a channel through the outbox and waits for the
    /// roster to list them.
    pub async fn add_member(&self, channel: &str, pubkey: &str) -> Result<(), String> {
        let (ops, channel, pubkey) = (self.ops(), channel.to_owned(), pubkey.to_owned());
        self.run(async move { ops.add_member(&channel, &pubkey).await })
            .await
    }

    /// Changes a member's role or removes them, published directly.
    pub async fn change_member(&self, channel: &str, change: MemberChange) -> Result<(), String> {
        let (ops, channel) = (self.ops(), channel.to_owned());
        self.run(async move { ops.change_member(&channel, change).await })
            .await
    }

    /// One page of people to add to a channel (§5g.6).
    pub async fn search_members(&self, query: &str, page: usize) -> Result<PeoplePage, String> {
        let filter = members::search_filter(query, page)?;
        let exact = members::exact_key(query);
        let (ops, query) = (self.ops(), query.trim().to_owned());
        self.run(async move {
            let ctx = ops.context().await?;
            let events = read(&ctx, vec![filter]).await?;
            ops.adopt_profiles(&events).await?;
            Ok(PeoplePage {
                more: exact.is_none() && events.len() >= members::SEARCH_PAGE,
                people: members::rank_people(&events, &query, exact.as_deref()),
            })
        })
        .await
    }

    /// One page of the New message people picker (§5e.6).
    pub async fn people(&self, query: &str, page: usize) -> Result<PeoplePage, String> {
        let (filter, limit) = dm::people_filter(query, page)?;
        let (ops, query, viewer) = (self.ops(), query.to_owned(), self.viewer.clone());
        self.run(async move {
            let ctx = ops.context().await?;
            let events = read(&ctx, vec![filter]).await?;
            ops.adopt_profiles(&events).await?;
            let (people, more) = dm::people_page(&events, &query, &viewer, limit);
            Ok(PeoplePage { people, more })
        })
        .await
    }

    /// Opens (or reopens and unhides) the DM with exactly these people and
    /// returns its channel id once discovery confirms it.
    pub async fn open_dm(&self, pubkeys: Vec<String>) -> Result<String, String> {
        let ops = self.ops();
        self.run(async move { ops.open_dm(pubkeys).await }).await
    }

    /// Exact relay resolution of channels the viewer has not joined:
    /// readable ones come back, others are dropped.
    pub async fn resolve_channels(&self, ids: Vec<String>) -> Result<Vec<ChannelSummary>, String> {
        let ops = self.ops();
        self.run(async move {
            let ctx = ops.context().await?;
            ops.resolve(&ctx, ids).await
        })
        .await
    }

    /// Open public channels the viewer has not joined whose name contains
    /// `query` (or equals it with `exact`), as read-only previews.
    pub async fn search_public_channels(
        &self,
        query: &str,
        exact: bool,
    ) -> Result<Vec<ChannelSummary>, String> {
        Ok(self
            .search_public_channel_page(query, exact)
            .await?
            .channels)
    }

    /// [`Session::search_public_channels`], saying whether the relay's
    /// page of channels was full (`searchPublic`'s `partial`).
    pub async fn search_public_channel_page(
        &self,
        query: &str,
        exact: bool,
    ) -> Result<PublicChannels, String> {
        let (ops, query) = (self.ops(), query.to_owned());
        self.run(async move { ops.public_channels(&query, exact).await })
            .await
    }

    /// Message search (`useSearchMessages`).
    pub async fn search_messages(&self, search: MessageSearch) -> Result<SearchOutcome, String> {
        let ops = self.ops();
        self.run(async move { ops.search(search).await }).await
    }
}

async fn read(ctx: &Context, filters: Vec<Filter>) -> Result<Vec<Verified>, String> {
    ctx.reader
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

/// Signs and posts a command outside the outbox. The raw result is kept
/// for receipts that carry data (41010).
async fn post(
    ctx: &Context,
    template: EventTemplate,
) -> Result<(Event, Result<Vec<u8>, crate::http::HttpError>), String> {
    let unsigned = ctx
        .identity
        .template(
            Purpose::Content,
            template.kind,
            template.created_at,
            template.tags,
            template.content,
        )
        .map_err(|error| error.to_string())?;
    let event = ctx
        .identity
        .sign(Purpose::Content, unsigned)
        .map_err(|error| error.to_string())?;
    let body = serde_json::to_string(&event).expect("events serialize");
    let result = ctx.http.post("/events", body).await;
    Ok((event, result))
}

/// Publishes a command. A refusal is final; an uncertain outcome lets the
/// caller try to confirm it anyway.
async fn publish(ctx: &Context, template: EventTemplate) -> Result<bool, String> {
    let (event, result) = post(ctx, template).await?;
    match accept_publish(result, &event.id) {
        Ok(_) => Ok(true),
        Err(PublishError::Rejected(message)) => Err(message),
        Err(PublishError::Uncertain(_)) => Ok(false),
    }
}

/// Evaluates `$check` (an `Option`) at each confirmation delay until it
/// is `Some`. A macro, not a function: the checks borrow the context and
/// must stay `Send`.
macro_rules! confirm {
    ($check:expr) => {{
        let mut found = None;
        for delay in CONFIRM_DELAYS {
            tokio::time::sleep(delay).await;
            if let Some(value) = $check {
                found = Some(value);
                break;
            }
        }
        found
    }};
}

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

impl Ops {
    /// Runs `f` on the actor and returns its result.
    async fn with<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Actor) -> R + Send + 'static,
    ) -> Result<R, String> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Message::With(Box::new(move |actor| {
                let _ = reply.send(f(actor));
            })))
            .map_err(|_| CLOSED.to_owned())?;
        answer.await.map_err(|_| CLOSED.to_owned())
    }

    async fn context(&self) -> Result<Context, String> {
        self.with(|actor| actor.context()).await?
    }

    /// Feeds relay-signed discovery events read at `epoch` to the actor.
    async fn adopt(&self, epoch: u64, events: Vec<Verified>) -> Result<(), String> {
        self.with(move |actor| {
            if actor.epoch == epoch {
                actor.accept(&events);
            }
        })
        .await
    }

    async fn adopt_profiles(&self, events: &[Verified]) -> Result<(), String> {
        let profiles: Vec<Verified> = events
            .iter()
            .filter(|event| event.kind_u16() == kinds::PROFILE)
            .cloned()
            .collect();
        self.with(move |actor| {
            if actor.profiles.accept(&profiles) {
                actor.publish();
            }
        })
        .await
    }

    async fn state(&self, ctx: &Context, channel: &str) -> Result<ChannelState, String> {
        let events = read(ctx, lifecycle::state_filters(&ctx.relay, channel)).await?;
        ChannelState::parse(&events, channel, &ctx.relay).map_err(|error| error.to_string())
    }

    /// Lifecycle permissions from a fresh state. When a direct role does
    /// not grant Delete (or Unarchive), the owners' profiles are read, four
    /// at a time within [`lifecycle::OWNER_EVIDENCE_TIMEOUT`]: an owner
    /// agent attested by the viewer grants it. The relay still authorizes
    /// every command.
    async fn permissions(
        &self,
        ctx: &Context,
        channel: &str,
    ) -> Result<LifecyclePermissions, String> {
        let state = self.state(ctx, channel).await?;
        let mut permissions = state
            .lifecycle(&ctx.viewer)
            .map_err(|error| error.to_string())?;
        let owners = state.owner_keys();
        if !permissions.wants_owner_evidence(state.archived) || owners.is_empty() {
            return Ok(permissions);
        }
        let lookup = async {
            for batch in owners.chunks(lifecycle::OWNER_BATCH) {
                let events = read(ctx, lifecycle::owner_profile_filters(batch)).await?;
                if events.iter().any(|event| {
                    event.kind_u16() != kinds::PROFILE || !batch.contains(&event.pubkey_hex())
                }) {
                    return Err("Unexpected channel owner profile response".to_owned());
                }
                let owned = batch.iter().any(|owner| {
                    let newest = crate::account::newest_profile(&events, owner);
                    newest.and_then(crate::profiles::attested_owner).as_deref()
                        == Some(ctx.viewer.as_str())
                });
                if owned {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        match tokio::time::timeout(lifecycle::OWNER_EVIDENCE_TIMEOUT, lookup).await {
            Ok(Ok(true)) => permissions.grant_owned_agent(state.archived),
            Ok(Ok(false)) => {}
            // Only owner-agent actions are affected; an archived channel
            // has no Delete to retry.
            Ok(Err(_)) | Err(_) => permissions.delete_unavailable = !state.archived,
        }
        Ok(permissions)
    }

    /// The verified NIP-OA owner of `agent`, from a fresh strong read of
    /// its kind 0 (`administration.ts:187-225`).
    async fn agent_owner(&self, ctx: &Context, agent: &str) -> Result<Option<String>, String> {
        let events = read(ctx, vec![members::owner_filter(agent)]).await?;
        Ok(
            crate::account::newest_profile(&events, agent)
                .and_then(crate::profiles::attested_owner),
        )
    }

    // ----- lifecycle -----

    async fn lifecycle(&self, action: Action, channel: &str) -> Result<(), String> {
        let ctx = self.context().await?;
        let mut adopted = Vec::new();
        if action == Action::Join {
            let events = read(
                &ctx,
                lifecycle::join_filters(&ctx.relay, &ctx.viewer, channel),
            )
            .await?;
            let member = lifecycle::join_ready(&events, &ctx.relay, &ctx.viewer, channel)
                .map_err(|error| error.to_string())?;
            if member {
                return self.adopt(ctx.epoch, events).await;
            }
            adopted = events;
        } else {
            let permissions = self.permissions(&ctx, channel).await?;
            if !action.allowed(&permissions) {
                return Err(match (action, permissions.leave_reason) {
                    (Action::Leave, Some(reason)) => reason,
                    _ => "You can't do that in this channel.".into(),
                });
            }
        }
        let template = lifecycle::template(action, channel, now()).map_err(|e| e.to_string())?;
        publish(&ctx, template).await?;
        let filter = lifecycle::confirm_filter(action, &ctx.relay, &ctx.viewer, channel);
        let events = confirm!({
            let events = read(&ctx, vec![filter.clone()]).await?;
            lifecycle::confirmed(action, &events, &ctx.relay, &ctx.viewer, channel)
                .then_some(events)
        })
        .ok_or(UNCONFIRMED)?;
        let channel = channel.to_owned();
        match action {
            Action::Join | Action::Archive | Action::Unarchive => {
                adopted.extend(events);
                self.adopt(ctx.epoch, adopted).await
            }
            Action::Leave | Action::Delete => self.with(move |actor| actor.removed(&channel)).await,
            Action::Hide => {
                let (relay, viewer) = (ctx.relay.clone(), ctx.viewer.clone());
                self.with(move |actor| {
                    if let Some(hidden) = lifecycle::hidden_dms(&events, &relay, &viewer) {
                        actor.set_hidden_dms(hidden);
                    }
                })
                .await
            }
        }
    }

    async fn save_details(
        &self,
        channel: &str,
        version: EventId,
        draft: DetailsDraft,
    ) -> Result<(), String> {
        let ctx = self.context().await?;
        let state = self.state(&ctx, channel).await?;
        let details = state
            .details(&ctx.viewer)
            .map_err(|error| error.to_string())?;
        if !details.can_edit {
            return Err("Only channel owners and admins can edit details.".into());
        }
        if details.version != version {
            return Err(
                "Channel details changed since you opened them. Review the current details and try again."
                    .into(),
            );
        }
        let template = lifecycle::edit_details(channel, &draft, &details.draft, now())
            .map_err(|error| error.to_string())?;
        publish(&ctx, template).await?;
        let state = confirm!({
            let state = self.state(&ctx, channel).await?;
            lifecycle::details_confirmed(&state, &draft).then_some(state)
        })
        .ok_or(UNCONFIRMED)?;
        self.adopt(ctx.epoch, vec![state.metadata]).await
    }

    async fn create(&self, channel: NewChannel) -> Result<String, String> {
        let ctx = self.context().await?;
        let id = uuid::Uuid::new_v4().to_string();
        let draft = lifecycle::create(&channel, &id).map_err(|error| error.to_string())?;
        let op = ctx
            .outbox
            .send(draft, SendOptions::default())
            .map_err(|error| error.to_string())?;
        if let Err(error) = delivered(&ctx.outbox, op).await {
            // An uncertain delivery may still have created the channel.
            let receipt = read(
                &ctx,
                vec![lifecycle::creation_receipt_filter(&op, &ctx.viewer)],
            )
            .await?;
            if !receipt.iter().any(|event| event.id == op) {
                return Err(error);
            }
        }
        let filters = lifecycle::created_filters(&ctx.relay, &ctx.viewer, &id);
        let events = confirm!({
            let events = read(&ctx, filters.clone()).await?;
            lifecycle::confirmed(Action::Join, &events, &ctx.relay, &ctx.viewer, &id)
                .then_some(events)
        })
        .ok_or(UNCONFIRMED)?;
        self.adopt(ctx.epoch, events).await?;
        Ok(id)
    }

    // ----- members -----

    async fn add_member(&self, channel: &str, pubkey: &str) -> Result<(), String> {
        let ctx = self.context().await?;
        let state = self.state(&ctx, channel).await?;
        let (id, key) = (channel.to_owned(), pubkey.to_owned());
        let (summary, agent) = self
            .with(move |actor| {
                (
                    actor.discovery.as_ref().and_then(|d| d.get(&id)),
                    actor
                        .profiles
                        .snapshot()
                        .get(&key)
                        .is_some_and(|profile| profile.is_agent),
                )
            })
            .await?;
        let member = state.role(&ctx.viewer).is_some();
        if !summary.is_some_and(|summary| members::can_add_members(&summary, member)) {
            return Err("You can't add people to this channel.".into());
        }
        if state.role(pubkey).is_some() {
            return Ok(());
        }
        ctx.outbox
            .ready()
            .await
            .map_err(|error| error.to_string())?;
        let key = members::addition_key(channel, pubkey);
        let pending = ctx
            .outbox
            .pending()
            .iter()
            .find(|op| op.recovery.as_ref().is_some_and(|r| r.key == key))
            .cloned();
        let op = match pending {
            Some(op) => {
                if op.delivery == Delivery::Failed {
                    ctx.outbox
                        .retry(&op.id(), None)
                        .map_err(|error| error.to_string())?;
                }
                op.id()
            }
            None => {
                let draft = members::add(channel, pubkey, agent).map_err(|e| e.to_string())?;
                let recovery = Recovery {
                    key,
                    value: pubkey.to_owned(),
                };
                ctx.outbox
                    .send(
                        draft,
                        SendOptions {
                            recovery: Some(recovery),
                            gate: None,
                        },
                    )
                    .map_err(|error| error.to_string())?
            }
        };
        delivered(&ctx.outbox, op).await?;
        let state = confirm!({
            let state = self.state(&ctx, channel).await?;
            state.role(pubkey).is_some().then_some(state)
        })
        .ok_or(UNCONFIRMED)?;
        ctx.outbox
            .acknowledge(&op)
            .await
            .map_err(|error| error.to_string())?;
        self.adopt(ctx.epoch, vec![state.metadata, state.roster])
            .await
    }

    async fn change_member(&self, channel: &str, change: MemberChange) -> Result<(), String> {
        let ctx = self.context().await?;
        let state = self.state(&ctx, channel).await?;
        // Removing someone's own agent needs its attestation, read fresh.
        let owner = if change.change == members::Change::Remove
            && !state.can_manage_member(&ctx.viewer, &change.pubkey)
            && state.can_remove_member(&ctx.viewer, &change.pubkey, Some(&ctx.viewer))
        {
            self.agent_owner(&ctx, &change.pubkey).await?
        } else {
            None
        };
        members::authorize(&state, &ctx.viewer, &change, owner.as_deref())
            .map_err(|e| e.to_string())?;
        let template = members::administration(channel, &change, &ctx.viewer, now())
            .map_err(|error| error.to_string())?;
        publish(&ctx, template).await?;
        let state = confirm!({
            let state = self.state(&ctx, channel).await?;
            members::change_confirmed(&state, &change).then_some(state)
        })
        .ok_or(UNCONFIRMED)?;
        self.adopt(ctx.epoch, vec![state.metadata, state.roster])
            .await
    }

    // ----- direct messages -----

    async fn open_dm(&self, pubkeys: Vec<String>) -> Result<String, String> {
        let ctx = self.context().await?;
        let template = dm::open_dm(&ctx.viewer, &pubkeys, now()).map_err(|e| e.to_string())?;
        let (event, result) = post(&ctx, template).await?;
        let body =
            result.map_err(|_| "The direct message could not be opened. Try again.".to_owned())?;
        let id = dm::parse_open_dm_receipt(&body, &event.id).map_err(|e| e.to_string())?;
        let events = read(&ctx, vec![dm::confirm_filter(&ctx.relay, &id)]).await?;
        self.adopt(ctx.epoch, events).await?;
        let (channel, viewer) = (id.clone(), ctx.viewer.clone());
        let confirmed = self
            .with(move |actor| {
                let summary = actor.discovery.as_ref().and_then(|d| d.get(&channel));
                let confirmed = dm::confirmed(summary.as_ref(), &viewer, &pubkeys);
                if confirmed {
                    // The relay unhides a reopened DM.
                    actor.unhide(&channel);
                }
                confirmed
            })
            .await?;
        if !confirmed {
            return Err(
                "The conversation’s participants could not be confirmed. Try again.".into(),
            );
        }
        Ok(id)
    }

    // ----- channels and search -----

    async fn resolve(
        &self,
        ctx: &Context,
        ids: Vec<String>,
    ) -> Result<Vec<ChannelSummary>, String> {
        let wanted = ids.clone();
        let plan = self
            .with(move |actor| actor.plan_resolution(&ids))
            .await??;
        if let Some((targets, filters, start)) = plan {
            let events = read(ctx, filters).await?;
            let epoch = ctx.epoch;
            self.with(move |actor| actor.finish_resolution(epoch, &targets, &start, &events))
                .await??;
        }
        self.with(move |actor| {
            let Some(discovery) = &actor.discovery else {
                return Vec::new();
            };
            wanted.iter().filter_map(|id| discovery.get(id)).collect()
        })
        .await
    }

    async fn public_channels(&self, query: &str, exact: bool) -> Result<PublicChannels, String> {
        let needle = query.trim().trim_start_matches('#').to_lowercase();
        if needle.is_empty() {
            return Ok(PublicChannels::default());
        }
        let ctx = self.context().await?;
        let page = Filter::from_iter([
            ("kinds".to_owned(), json!([kinds::CHANNEL_METADATA])),
            ("authors".to_owned(), json!([ctx.relay])),
            ("limit".to_owned(), json!(PUBLIC_CHANNEL_PAGE)),
        ]);
        let events = read(&ctx, vec![page]).await?;
        let partial = events.len() >= PUBLIC_CHANNEL_PAGE;
        let joined: HashSet<String> = self
            .with(|actor| {
                actor
                    .discovery
                    .as_ref()
                    .map(|d| d.channels().into_iter().map(|c| c.id).collect())
                    .unwrap_or_default()
            })
            .await?;
        let mut newest: std::collections::HashMap<&str, &Verified> = Default::default();
        for event in events
            .iter()
            .filter(|e| e.kind_u16() == kinds::CHANNEL_METADATA && e.pubkey_hex() == ctx.relay)
        {
            if let Some(id) = event.tag("d")
                && newest
                    .get(id)
                    .is_none_or(|current| crate::event::is_newer(event, current))
            {
                newest.insert(id, event);
            }
        }
        let mut candidates: Vec<(u8, String, String)> = newest
            .into_iter()
            .filter(|(id, event)| {
                open_metadata(event)
                    && event.tag("archived") != Some("true")
                    && !joined.contains(*id)
            })
            .filter_map(|(id, event)| {
                let name = metadata_name(event)?;
                let lower = name.to_lowercase();
                let matches = if exact {
                    lower == needle
                } else {
                    lower.contains(&needle)
                };
                matches.then(|| {
                    let rank = search::match_rank(&name, &needle).unwrap_or(3);
                    (rank, name, id.to_owned())
                })
            })
            .collect();
        candidates.sort();
        candidates.truncate(PUBLIC_CHANNEL_MATCHES);
        let ids: Vec<String> = candidates.into_iter().map(|(_, _, id)| id).collect();
        let mut channels: Vec<ChannelSummary> = self
            .resolve(&ctx, ids.clone())
            .await?
            .into_iter()
            .filter(|c| c.read_only && !c.archived && !c.cached)
            .collect();
        // Best match first, as ranked above.
        channels.sort_by_key(|c| ids.iter().position(|id| *id == c.id));
        Ok(PublicChannels { channels, partial })
    }

    async fn search(&self, search: MessageSearch) -> Result<SearchOutcome, String> {
        let ctx = self.context().await?;
        let mut outcome = SearchOutcome::default();
        let author = match search.from.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(key) if search::is_hex_key(key) => Some(key.to_ascii_lowercase()),
            Some(name) => {
                let handle = name.strip_prefix('@').unwrap_or(name).to_owned();
                match self
                    .author(&ctx, &handle, search.channel.clone(), search.agents.clone())
                    .await?
                {
                    Author::Found(key) => Some(key),
                    Author::Missing => return Ok(outcome),
                    Author::Ambiguous => {
                        outcome.ambiguous_author = true;
                        return Ok(outcome);
                    }
                }
            }
        };
        if let Some(channel) = &search.channel
            && self.resolve(&ctx, vec![channel.clone()]).await?.is_empty()
        {
            return Ok(outcome);
        }
        let request = SearchRequest {
            text: search.text.trim().to_owned(),
            author,
            channel: search.channel.clone(),
            since: search.since,
            until: search.until,
        };
        if request.is_empty() {
            return Ok(outcome);
        }
        let events = read(&ctx, vec![request.filter()]).await?;
        let hits = search::hits(&events, search.channel.as_deref(), Some(&ctx.relay));
        let mut ids: Vec<String> = hits.iter().map(|hit| hit.channel.clone()).collect();
        ids.sort();
        ids.dedup();
        ids.truncate(crate::discovery::RESOLVE_LIMIT);
        outcome.channels = self.resolve(&ctx, ids).await?;
        let readable: HashSet<&str> = outcome.channels.iter().map(|c| c.id.as_str()).collect();
        outcome.hits = hits
            .into_iter()
            .filter(|hit| readable.contains(hit.channel.as_str()))
            .collect();
        let authors = outcome
            .hits
            .iter()
            .flat_map(|hit| std::iter::once(hit.author.clone()).chain(hit.workflow_owner.clone()))
            .collect();
        self.with(move |actor| actor.ensure_profiles(authors))
            .await?;
        Ok(outcome)
    }

    /// Resolves a `from:` name: the scoped channel's members first, then a
    /// relay profile lookup. Selectable agents are candidates either way,
    /// by profile name or by their own name.
    async fn author(
        &self,
        ctx: &Context,
        handle: &str,
        channel: Option<String>,
        agents: Vec<(String, String)>,
    ) -> Result<Author, String> {
        let wanted = handle.to_owned();
        let (scoped, agents) = self
            .with(move |actor| {
                let members = channel
                    .and_then(|id| actor.discovery.as_ref()?.get(&id))
                    .map(|c| c.members)
                    .unwrap_or_default();
                let profiles = actor.profiles.snapshot();
                let named: Vec<(String, String)> = members
                    .into_iter()
                    .filter_map(|key| {
                        let name = profiles.get(&key)?.name.clone();
                        Some((key, name))
                    })
                    .collect();
                let mut aliases = Vec::new();
                for (key, name) in agents {
                    if let Some(profile) = profiles.get(&key) {
                        aliases.push((key.clone(), profile.name.clone()));
                    }
                    aliases.push((key, name));
                }
                let scoped = match search::resolve_author(
                    &wanted,
                    named.iter().map(|(k, n)| (k.as_str(), n.as_str())),
                ) {
                    Author::Missing => None,
                    _ => Some(named),
                };
                (scoped, aliases)
            })
            .await?;
        let pairs = |people: &[(String, String)]| {
            search::resolve_author(
                handle,
                people
                    .iter()
                    .chain(&agents)
                    .map(|(k, n)| (k.as_str(), n.as_str())),
            )
        };
        if let Some(scoped) = scoped {
            return Ok(pairs(&scoped));
        }
        let events = read(ctx, vec![search::author_lookup_filter(handle)]).await?;
        self.adopt_profiles(&events).await?;
        let people: Vec<(String, String)> = members::rank_people(&events, handle, None)
            .into_iter()
            .map(|p| (p.pubkey, p.profile.name))
            .collect();
        Ok(pairs(&people))
    }
}

impl Actor {
    fn context(&self) -> Result<Context, String> {
        let relay = self.relay.as_ref().ok_or(NOT_CONNECTED)?.authority.clone();
        Ok(Context {
            reader: self.reader.clone(),
            http: self.http.clone(),
            identity: self.identity.clone(),
            viewer: self.identity.pubkey_hex(),
            relay,
            epoch: self.epoch,
            outbox: self.outbox.clone().ok_or(NOT_CONNECTED)?,
        })
    }

    /// The viewer left: the channel is gone from discovery and its views.
    fn removed(&mut self, channel: &str) {
        if let Some(discovery) = &mut self.discovery {
            discovery.deny(channel);
        }
        self.purge_denied();
        self.update_interests();
        self.reads_channels(false);
        self.publish();
    }

    // ----- exact resolution (`store.ts` `resolve`) -----

    #[allow(clippy::type_complexity)]
    fn plan_resolution(
        &self,
        ids: &[String],
    ) -> Result<Option<(Vec<String>, Vec<Filter>, ResolutionStart)>, String> {
        let discovery = self.discovery.as_ref().ok_or(NOT_CONNECTED)?;
        let targets = discovery
            .resolve_targets(ids)
            .map_err(|error| error.to_string())?;
        if targets.is_empty() {
            return Ok(None);
        }
        let filters = crate::discovery::resolve_filters(
            discovery.relay_author(),
            discovery.viewer(),
            &targets,
            true,
        );
        let start = discovery.resolution_start(&targets);
        Ok(Some((targets, filters, start)))
    }

    fn finish_resolution(
        &mut self,
        epoch: u64,
        targets: &[String],
        start: &ResolutionStart,
        events: &[Verified],
    ) -> Result<(), String> {
        if epoch != self.epoch {
            return Ok(());
        }
        let Some(discovery) = &mut self.discovery else {
            return Ok(());
        };
        let resolution = discovery
            .apply_resolution(targets, start, events)
            .map_err(|error| error.to_string())?;
        if resolution.applied.changed || !resolution.denied.is_empty() || resolution.resumed {
            self.purge_denied();
            self.update_interests();
            self.reads_channels(false);
            self.publish();
        }
        Ok(())
    }

    // ----- hidden DMs (30622) -----

    /// Re-reads the viewer's hidden DM list.
    pub(super) fn refresh_visibility(&mut self) {
        let Ok(ctx) = self.context() else {
            return;
        };
        let commands = self.commands.clone();
        self.runtime.spawn(async move {
            let filter = lifecycle::visibility_filter(&ctx.relay, &ctx.viewer, false);
            let Ok(events) = read(&ctx, vec![filter]).await else {
                return;
            };
            let hidden =
                lifecycle::hidden_dms(&events, &ctx.relay, &ctx.viewer).unwrap_or_default();
            let _ = commands.send(Message::With(Box::new(move |actor| {
                if actor.epoch == ctx.epoch {
                    actor.set_hidden_dms(hidden);
                }
            })));
        });
    }

    fn set_hidden_dms(&mut self, mut hidden: Vec<String>) {
        hidden.sort();
        hidden.dedup();
        if *self.hidden_dms != *hidden {
            self.hidden_dms = hidden.into();
            self.publish();
        }
    }

    fn unhide(&mut self, channel: &str) {
        if self.hidden_dms.iter().any(|id| id == channel) {
            let hidden = self
                .hidden_dms
                .iter()
                .filter(|id| *id != channel)
                .cloned()
                .collect();
            self.set_hidden_dms(hidden);
        }
    }
}
