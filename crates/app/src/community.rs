//! The user's communities and their sessions, as GPUI entities.

use crate::idle::Idle;
use crate::mirror::mirror;
use crate::notifications;
use crate::runtime::Runtime;
use buzz_identity::Identity;
use buzz_relay_client::{
    ChannelType, ChannelView, LeaveOutcome, PresenceMode, RelayOrigin, Session, SessionSnapshot,
    Store, TimelineSnapshot,
};
use gpui_kit::{App, AppContext, Context, Entity, Global, Task};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Viewer preference holding the saved communities.
const SAVED: &str = "communities";
/// View state key for the channel last open in a community.
const SELECTED_CHANNEL: &str = "selected-channel";
/// Viewer preference holding the availability choice (`buzz-presence.v1`).
const PRESENCE_MODE: &str = "presence-mode";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Saved {
    origins: Vec<String>,
    selected: Option<String>,
}

pub enum IdentityState {
    Ready(Arc<Identity>),
    /// No identity is saved yet; onboarding creates or imports one.
    Missing,
    Error(String),
}

pub struct AppState {
    pub identity: IdentityState,
    pub store: Store,
    pub communities: Vec<Entity<Community>>,
    pub selected: Option<usize>,
    /// The availability the user chose, applied to every community.
    pub presence_mode: PresenceMode,
}

impl Global for AppState {}

impl AppState {
    pub fn init(cx: &mut App) {
        let path = data_dir().join("buzz.sqlite3");
        let store = Store::open(&path).unwrap_or_else(|error| {
            tracing::error!(%error, path = %path.display(), "could not open local storage; using memory");
            Store::in_memory().expect("in-memory store")
        });
        let identity = load_identity();
        let mut state = Self {
            identity,
            store,
            communities: Vec::new(),
            selected: None,
            presence_mode: PresenceMode::Auto,
        };
        if let IdentityState::Ready(identity) = &state.identity {
            state.presence_mode = load_presence_mode(&state.store, &identity.pubkey_hex());
            let saved = state.saved(&identity.pubkey_hex());
            for origin in saved
                .origins
                .iter()
                .filter_map(|o| RelayOrigin::parse(o).ok())
            {
                let community = Community::new(
                    origin,
                    identity.clone(),
                    state.store.clone(),
                    state.presence_mode,
                    cx,
                );
                state.communities.push(community);
            }
            state.selected = saved
                .selected
                .and_then(|selected| saved.origins.iter().position(|o| *o == selected))
                .or((!state.communities.is_empty()).then_some(0));
        }
        cx.set_global(state);
    }

    /// Adopts a newly created or imported identity and opens its saved
    /// communities.
    pub fn set_identity(identity: buzz_identity::Identity, cx: &mut App) {
        let identity = Arc::new(identity);
        let store = cx.global::<Self>().store.clone();
        let saved = cx.global::<Self>().saved(&identity.pubkey_hex());
        let mode = load_presence_mode(&store, &identity.pubkey_hex());
        let communities: Vec<Entity<Community>> = saved
            .origins
            .iter()
            .filter_map(|origin| RelayOrigin::parse(origin).ok())
            .map(|origin| Community::new(origin, identity.clone(), store.clone(), mode, cx))
            .collect();
        let state = cx.global_mut::<Self>();
        state.selected = (!communities.is_empty()).then_some(0);
        state.communities = communities;
        state.identity = IdentityState::Ready(identity);
        state.presence_mode = mode;
    }

    fn saved(&self, viewer: &str) -> Saved {
        self.store
            .viewer_pref(viewer, SAVED)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    fn save(&self, cx: &App) {
        let IdentityState::Ready(identity) = &self.identity else {
            return;
        };
        let origins: Vec<String> = self
            .communities
            .iter()
            .map(|community| community.read(cx).origin.to_string())
            .collect();
        let saved = Saved {
            selected: self.selected.and_then(|index| origins.get(index).cloned()),
            origins,
        };
        let json = serde_json::to_string(&saved).expect("saved communities serialize");
        if let Err(error) = self
            .store
            .set_viewer_pref(&identity.pubkey_hex(), SAVED, &json)
        {
            tracing::error!(%error, "could not save communities");
        }
    }

    /// Adds a community by relay URL and selects it.
    pub fn add_community(input: &str, cx: &mut App) -> Result<(), String> {
        let origin = RelayOrigin::parse(input).map_err(|error| error.to_string())?;
        let state = cx.global::<Self>();
        let IdentityState::Ready(identity) = &state.identity else {
            return Err("Set up your identity first".into());
        };
        if let Some(index) = state
            .communities
            .iter()
            .position(|community| community.read(cx).origin == origin)
        {
            cx.global_mut::<Self>().selected = Some(index);
            cx.global::<Self>().save(cx);
            return Ok(());
        }
        let (identity, store) = (identity.clone(), state.store.clone());
        let community = Community::new(origin, identity, store, state.presence_mode, cx);
        let state = cx.global_mut::<Self>();
        state.communities.push(community);
        state.selected = Some(state.communities.len() - 1);
        cx.global::<Self>().save(cx);
        Ok(())
    }

    pub fn select(index: usize, cx: &mut App) {
        let state = cx.global_mut::<Self>();
        if index < state.communities.len() {
            state.selected = Some(index);
            cx.global::<Self>().save(cx);
        }
    }

    /// Sets the availability every community publishes, and remembers it.
    pub fn set_presence_mode(mode: PresenceMode, cx: &mut App) {
        cx.global_mut::<Self>().presence_mode = mode;
        let state = cx.global::<Self>();
        for community in &state.communities {
            community.read(cx).session.set_presence_mode(mode);
        }
        let IdentityState::Ready(identity) = &state.identity else {
            return;
        };
        let value = match mode {
            PresenceMode::Auto => "\"auto\"",
            PresenceMode::Away => "\"away\"",
            PresenceMode::Offline => "\"offline\"",
        };
        let saved = state
            .store
            .set_viewer_pref(&identity.pubkey_hex(), PRESENCE_MODE, value);
        if let Err(error) = saved {
            tracing::warn!(%error, "could not save the availability choice");
        }
    }

    /// Leaves a community (NIP-43, relay.md §1.8). On success the
    /// community is dropped from this device and, unless the relay says
    /// access was revoked, its local data is purged. Any failure keeps the
    /// membership so the user can retry.
    pub fn leave(community: Entity<Community>, cx: &mut App) -> Task<Result<LeaveOutcome, String>> {
        let session = community.read(cx).session.clone();
        cx.spawn(async move |cx| {
            let outcome = session.leave().await?;
            let purge = outcome != LeaveOutcome::Revoked;
            cx.update(|cx| Self::remove(&community, purge, cx));
            Ok(outcome)
        })
    }

    /// Drops a community from this device: another community becomes
    /// selected, the session stops, then its local data is purged.
    fn remove(community: &Entity<Community>, purge: bool, cx: &mut App) {
        let id = community.entity_id();
        let state = cx.global_mut::<Self>();
        let Some(index) = state
            .communities
            .iter()
            .position(|other| other.entity_id() == id)
        else {
            return;
        };
        state.communities.remove(index);
        let remaining = state.communities.len();
        state.selected = match state.selected {
            _ if remaining == 0 => None,
            Some(selected) if selected > index => Some(selected - 1),
            Some(selected) if selected == index => Some(index.min(remaining - 1)),
            other => other,
        };
        cx.global::<Self>().save(cx);
        let (session, partition) = {
            let community = community.read(cx);
            (community.session.clone(), community.partition())
        };
        let store = cx.global::<Self>().store.clone();
        Runtime::handle(cx).spawn(async move {
            session.dispose().await;
            if purge && let Err(error) = store.purge(&partition) {
                tracing::error!(%error, "could not purge the community's local data");
            }
        });
    }

    /// Signs out (relay.md §2.4, adapted): deletes the identity from the
    /// keyring, which buzz-app shares, and closes every community. Local
    /// data stays, as in buzz-app's sign-out without a wipe.
    pub fn sign_out(cx: &mut App) -> Result<(), String> {
        if std::env::var_os("BUZZ_DEV_NSEC_FILE").is_some() {
            tracing::warn!("development identity from a file; the keyring is left alone");
        } else {
            buzz_identity::forget().map_err(|error| error.to_string())?;
        }
        let state = cx.global_mut::<Self>();
        let communities = std::mem::take(&mut state.communities);
        state.selected = None;
        state.identity = IdentityState::Missing;
        for community in communities {
            let session = community.read(cx).session.clone();
            Runtime::handle(cx).spawn(async move { session.dispose().await });
        }
        Ok(())
    }

    pub fn current(cx: &App) -> Option<Entity<Community>> {
        let state = cx.global::<Self>();
        state
            .selected
            .and_then(|index| state.communities.get(index).cloned())
    }
}

pub struct Community {
    pub origin: RelayOrigin,
    pub session: Arc<Session>,
    pub snapshot: Arc<SessionSnapshot>,
    /// The channel last opened, which notifications skip while the window
    /// is focused.
    pub reading: Option<String>,
    /// Whose presence the session follows.
    presence_demand: Vec<String>,
    /// People an open view (the members dialog) also follows.
    presence_extra: Vec<String>,
    _watch: Task<()>,
    _notifications: Option<Task<()>>,
}

impl Community {
    fn new(
        origin: RelayOrigin,
        identity: Arc<Identity>,
        store: Store,
        presence_mode: PresenceMode,
        cx: &mut App,
    ) -> Entity<Self> {
        let session = Arc::new(Session::open(
            &Runtime::handle(cx),
            origin.clone(),
            identity.clone(),
            store,
        ));
        if cx.try_global::<Idle>().is_some_and(|idle| idle.idle) {
            session.set_idle(true);
        }
        if presence_mode != PresenceMode::Auto {
            session.set_presence_mode(presence_mode);
        }
        let community = cx.new(|cx| {
            let snapshot = session.snapshot();
            let current = snapshot.borrow().clone();
            Self {
                _watch: mirror(snapshot, cx, |this: &mut Self, value| {
                    this.snapshot = value;
                    this.follow_presence();
                }),
                origin,
                session,
                snapshot: current,
                reading: None,
                presence_demand: Vec::new(),
                presence_extra: Vec::new(),
                _notifications: None,
            }
        });
        let notifications = notifications::listen(&community, cx);
        community.update(cx, |community, _| {
            community._notifications = Some(notifications);
        });
        crate::ui::settings::capture_agent_activity(&community, identity, cx);
        community
    }

    /// Follows the viewer's presence, for the account menu, and that of
    /// everyone with a one-to-one DM, for the sidebar's avatar dots.
    fn follow_presence(&mut self) {
        let viewer = self.session.viewer().to_string();
        let mut demand: Vec<String> = std::iter::once(viewer)
            .chain(
                self.snapshot
                    .channels
                    .iter()
                    .filter(|channel| {
                        channel.channel_type == Some(ChannelType::Dm) && !channel.archived
                    })
                    .filter_map(|channel| match channel.participants.as_deref() {
                        Some([other]) => Some(other.clone()),
                        _ => None,
                    }),
            )
            .collect();
        for person in &self.presence_extra {
            if !demand.contains(person) {
                demand.push(person.clone());
            }
        }
        if demand != self.presence_demand {
            self.presence_demand = demand.clone();
            self.session.watch_presence(demand);
        }
    }

    /// Also follows these people's presence, replacing the previous extra
    /// set.
    pub fn set_presence_extra(&mut self, people: Vec<String>) {
        self.presence_extra = people;
        self.follow_presence();
    }

    pub fn open_channel(&mut self, channel: &str, cx: &mut App) -> Entity<ChannelModel> {
        let view = self.session.open_channel(channel);
        self.session.select(Some(channel.to_string()));
        self.reading = Some(channel.to_string());
        cx.new(|cx| {
            let current = view.snapshot.borrow().clone();
            Self::channel_model(view, current, cx)
        })
    }

    fn channel_model(
        view: ChannelView,
        current: Arc<TimelineSnapshot>,
        cx: &mut Context<ChannelModel>,
    ) -> ChannelModel {
        ChannelModel {
            _watch: mirror(
                view.snapshot.clone(),
                cx,
                |this: &mut ChannelModel, value| this.snapshot = value,
            ),
            view,
            snapshot: current,
        }
    }

    pub fn partition(&self) -> buzz_relay_client::Partition {
        buzz_relay_client::Partition::new(&self.origin, self.session.viewer())
    }

    /// The channel the user last had open here.
    pub fn remembered_channel(&self, cx: &App) -> Option<String> {
        let json = cx
            .global::<AppState>()
            .store
            .view_state(&self.partition(), SELECTED_CHANNEL)
            .ok()??;
        serde_json::from_str(&json).ok()
    }

    pub fn remember_channel(&self, channel: &str, cx: &App) {
        let json = serde_json::to_string(channel).expect("strings serialize");
        let saved = cx.global::<AppState>().store.set_view_state(
            &self.partition(),
            SELECTED_CHANNEL,
            Some(&json),
        );
        if let Err(error) = saved {
            tracing::warn!(%error, "could not remember the selected channel");
        }
    }

    /// Display name: the relay's NIP-11 name, else its host.
    pub fn name(&self) -> String {
        self.snapshot
            .relay
            .as_ref()
            .and_then(|relay| relay.name.clone())
            .unwrap_or_else(|| {
                self.origin
                    .websocket()
                    .split("://")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string()
            })
    }
}

/// One open channel. Dropping the entity releases the session's view.
pub struct ChannelModel {
    pub view: ChannelView,
    pub snapshot: Arc<TimelineSnapshot>,
    _watch: Task<()>,
}

impl ChannelModel {
    pub fn load_older(&self) {
        self.view.load_older();
    }
}

fn load_presence_mode(store: &Store, viewer: &str) -> PresenceMode {
    match store
        .viewer_pref(viewer, PRESENCE_MODE)
        .ok()
        .flatten()
        .as_deref()
    {
        Some("\"away\"") => PresenceMode::Away,
        Some("\"offline\"") => PresenceMode::Offline,
        _ => PresenceMode::Auto,
    }
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("buzz-gpui")
}

/// The keyring identity buzz-app also uses. For development on machines
/// without a keyring, `BUZZ_DEV_NSEC_FILE` names a file holding an nsec.
fn load_identity() -> IdentityState {
    if let Ok(path) = std::env::var("BUZZ_DEV_NSEC_FILE") {
        return match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|nsec| Identity::from_nsec(&nsec).map_err(|error| error.to_string()))
        {
            Ok(identity) => {
                tracing::warn!(%path, "using a development identity from a file");
                IdentityState::Ready(Arc::new(identity))
            }
            Err(error) => IdentityState::Error(error),
        };
    }
    match buzz_identity::load() {
        Ok(Some(identity)) => IdentityState::Ready(Arc::new(identity)),
        Ok(None) => IdentityState::Missing,
        Err(error) => IdentityState::Error(error.to_string()),
    }
}
