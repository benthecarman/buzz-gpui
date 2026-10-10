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
    /// The saved identity is being read (`identity_restore`).
    Loading,
    Ready(Arc<Identity>),
    /// No identity is saved yet; onboarding creates or imports one.
    Missing,
    /// The saved identity could not be read; onboarding offers a retry.
    Error(String),
}

pub struct AppState {
    pub identity: IdentityState,
    pub store: Store,
    pub communities: Vec<Entity<Community>>,
    pub selected: Option<usize>,
    /// The availability the user chose, applied to every community.
    pub presence_mode: PresenceMode,
    /// The saved availability could not be read or saved (`activity.ts`).
    pub presence_error: Option<String>,
}

impl Global for AppState {}

impl AppState {
    pub fn init(cx: &mut App) {
        let path = data_dir().join("buzz.sqlite3");
        let store = Store::open(&path).unwrap_or_else(|error| {
            tracing::error!(%error, path = %path.display(), "could not open local storage; using memory");
            Store::in_memory().expect("in-memory store")
        });
        cx.set_global(Self {
            identity: IdentityState::Loading,
            store,
            communities: Vec::new(),
            selected: None,
            presence_mode: PresenceMode::Auto,
            presence_error: None,
        });
        // A development identity file is read at once, so development
        // launches open straight into the workspace.
        if dev_identity_file().is_some() {
            Self::restored(identity::restore(), cx);
        } else {
            Self::restore(cx);
        }
    }

    /// Reads the saved identity off the UI thread, as buzz-app's
    /// `identity_restore` does; the keyring may wait on an unlock prompt.
    /// Also the "Retry secure storage access" action.
    pub fn restore(cx: &mut App) {
        if matches!(cx.global::<Self>().identity, IdentityState::Ready(_)) {
            return;
        }
        cx.global_mut::<Self>().identity = IdentityState::Loading;
        let read = cx.background_spawn(async { identity::restore() });
        cx.spawn(async move |cx| {
            let result = read.await;
            cx.update(|cx| Self::restored(result, cx));
        })
        .detach();
    }

    fn restored(result: Result<Option<Identity>, String>, cx: &mut App) {
        match result {
            Ok(Some(identity)) => Self::set_identity(identity, cx),
            Ok(None) => cx.global_mut::<Self>().identity = IdentityState::Missing,
            Err(error) => cx.global_mut::<Self>().identity = IdentityState::Error(error),
        }
    }

    /// Adopts a restored, created or imported identity and opens its saved
    /// communities.
    pub fn set_identity(identity: buzz_identity::Identity, cx: &mut App) {
        let identity = Arc::new(identity);
        // Agents start only for this key.
        if let Some(host) = crate::hosts::Hosts::embedded(cx) {
            host.set_owner(identity.pubkey_hex());
        }
        let store = cx.global::<Self>().store.clone();
        let saved = cx.global::<Self>().saved(&identity.pubkey_hex());
        let (mode, presence_error) = load_presence_mode(&store, &identity.pubkey_hex());
        let communities: Vec<Entity<Community>> = saved
            .origins
            .iter()
            .filter_map(|origin| RelayOrigin::parse(origin).ok())
            .map(|origin| Community::new(origin, identity.clone(), store.clone(), mode, cx))
            .collect();
        let state = cx.global_mut::<Self>();
        state.selected = saved
            .selected
            .and_then(|selected| saved.origins.iter().position(|o| *o == selected))
            .or((!communities.is_empty()).then_some(0));
        state.communities = communities;
        state.identity = IdentityState::Ready(identity);
        state.presence_mode = mode;
        state.presence_error = presence_error;
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
        let error = saved.err().map(|error| {
            tracing::warn!(%error, "could not save the availability choice");
            "Could not save your status. It applies to this window until you close it.".to_string()
        });
        cx.global_mut::<Self>().presence_error = error;
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
        if purge {
            crate::ui::emoji_picker::forget_quick_reactions(&partition, cx);
        }
        let store = cx.global::<Self>().store.clone();
        Runtime::handle(cx).spawn(async move {
            session.dispose().await;
            if purge && let Err(error) = store.purge(&partition) {
                tracing::error!(%error, "could not purge the community's local data");
            }
        });
    }

    /// Signs out: deletes the saved key and closes every community.
    pub fn sign_out(cx: &mut App) -> Result<(), String> {
        identity::remove()?;
        Self::close_sessions(cx);
        let state = cx.global_mut::<Self>();
        state.communities.clear();
        state.selected = None;
        state.identity = IdentityState::Missing;
        Ok(())
    }

    /// Sign out's commit point (`IdentityHost::close`): every session
    /// stops, so nothing more is signed with this key before Buzz restarts
    /// and the next launch removes it.
    pub fn close_sessions(cx: &mut App) {
        let sessions: Vec<_> = cx
            .global::<Self>()
            .communities
            .iter()
            .map(|community| community.read(cx).session.clone())
            .collect();
        let runtime = Runtime::handle(cx);
        for session in sessions {
            runtime.spawn(async move { session.dispose().await });
        }
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
    /// Agents whose messages the open timeline shows, for their badges.
    presence_rows: Vec<String>,
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
        if cx.try_global::<Idle>().is_some_and(|idle| !idle.visible) {
            session.set_visible(false);
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
                presence_rows: Vec::new(),
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

    /// Follows the viewer's presence, for the account menu, and the
    /// presence and status of everyone with a one-to-one DM, for the
    /// sidebar's avatar dots and status emoji.
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
        for person in self.presence_extra.iter().chain(&self.presence_rows) {
            if !demand.contains(person) {
                demand.push(person.clone());
            }
        }
        if demand != self.presence_demand {
            self.presence_demand = demand.clone();
            self.session.watch_presence(demand);
        }
    }

    /// Follows the presence of agents shown in the timeline, replacing the
    /// previous set.
    pub fn set_presence_rows(&mut self, people: Vec<String>) {
        if self.presence_rows != people {
            self.presence_rows = people;
            self.follow_presence();
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

/// The saved availability; an unreadable one is Auto with the error the
/// account menu shows.
fn load_presence_mode(store: &Store, viewer: &str) -> (PresenceMode, Option<String>) {
    match store.viewer_pref(viewer, PRESENCE_MODE) {
        Ok(saved) => (
            match saved.as_deref() {
                Some("\"away\"") => PresenceMode::Away,
                Some("\"offline\"") => PresenceMode::Offline,
                _ => PresenceMode::Auto,
            },
            None,
        ),
        Err(error) => {
            tracing::warn!(%error, "could not read the availability choice");
            (
                PresenceMode::Auto,
                Some("Could not read your saved status. Changes apply to this window.".into()),
            )
        }
    }
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("buzz-gpui")
}

/// For development on machines without a keyring, `BUZZ_DEV_NSEC_FILE`
/// names a file that stands in for the keyring entry.
pub fn dev_identity_file() -> Option<PathBuf> {
    std::env::var_os("BUZZ_DEV_NSEC_FILE")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// The human key's store: the keyring entry buzz-app also uses, or the
/// development file in its place. With the file, the keyring is never
/// read, written or deleted.
pub mod identity {
    use super::dev_identity_file;
    use buzz_identity::{Identity, IdentityError};
    use std::io::ErrorKind;
    use std::path::Path;

    /// The saved identity; `None` when none is saved.
    pub fn restore() -> Result<Option<Identity>, String> {
        let Some(path) = dev_identity_file() else {
            return buzz_identity::load().map_err(|error| error.to_string());
        };
        tracing::warn!(path = %path.display(), "using a development identity file");
        read_file(&path).map_err(|error| error.to_string())
    }

    /// Saves a new random identity, or the imported `nsec`.
    pub fn save(nsec: Option<&str>) -> Result<Identity, String> {
        let Some(path) = dev_identity_file() else {
            return match nsec {
                Some(nsec) => buzz_identity::import(nsec),
                None => buzz_identity::create(),
            }
            .map_err(|error| error.to_string());
        };
        if read_file(&path)
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err(IdentityError::AlreadySaved.to_string());
        }
        let identity = match nsec {
            Some(nsec) => Identity::from_nsec(nsec).map_err(|error| error.to_string())?,
            None => Identity::ephemeral(),
        };
        std::fs::write(&path, identity.nsec().as_bytes())
            .map_err(|_| IdentityError::Unavailable.to_string())?;
        Ok(identity)
    }

    /// Sign out's last step at the next launch: deletes the saved key and
    /// confirms a fresh read finds nothing. An absent key is success.
    pub fn remove() -> Result<(), String> {
        let Some(path) = dev_identity_file() else {
            return buzz_identity::forget().map_err(|error| error.to_string());
        };
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        }
    }

    /// An absent or empty file is no identity.
    fn read_file(path: &Path) -> Result<Option<Identity>, IdentityError> {
        match std::fs::read_to_string(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err(IdentityError::Unavailable),
            Ok(text) if text.trim().is_empty() => Ok(None),
            Ok(text) => Identity::from_nsec(&text)
                .map(Some)
                .map_err(|_| IdentityError::Malformed),
        }
    }
}
