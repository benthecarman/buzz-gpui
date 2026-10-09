//! The user's communities and their sessions, as GPUI entities.

use crate::idle::Idle;
use crate::mirror::mirror;
use crate::notifications;
use crate::runtime::Runtime;
use buzz_identity::Identity;
use buzz_relay_client::{
    ChannelType, ChannelView, RelayOrigin, Session, SessionSnapshot, Store, TimelineSnapshot,
};
use gpui_kit::{App, AppContext, Context, Entity, Global, Task};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Viewer preference holding the saved communities.
const SAVED: &str = "communities";
/// View state key for the channel last open in a community.
const SELECTED_CHANNEL: &str = "selected-channel";

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
        };
        if let IdentityState::Ready(identity) = &state.identity {
            let saved = state.saved(&identity.pubkey_hex());
            for origin in saved
                .origins
                .iter()
                .filter_map(|o| RelayOrigin::parse(o).ok())
            {
                let community = Community::new(origin, identity.clone(), state.store.clone(), cx);
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
        let communities: Vec<Entity<Community>> = saved
            .origins
            .iter()
            .filter_map(|origin| RelayOrigin::parse(origin).ok())
            .map(|origin| Community::new(origin, identity.clone(), store.clone(), cx))
            .collect();
        let state = cx.global_mut::<Self>();
        state.selected = (!communities.is_empty()).then_some(0);
        state.communities = communities;
        state.identity = IdentityState::Ready(identity);
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
        let community = Community::new(origin, identity, store, cx);
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
        cx: &mut App,
    ) -> Entity<Self> {
        let session = Arc::new(Session::open(
            &Runtime::handle(cx),
            origin.clone(),
            identity,
            store,
        ));
        if cx.try_global::<Idle>().is_some_and(|idle| idle.idle) {
            session.set_idle(true);
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
        community
    }

    /// Follows the presence of everyone with a one-to-one DM, for the
    /// sidebar's avatar dots.
    fn follow_presence(&mut self) {
        let mut demand: Vec<String> = self
            .snapshot
            .channels
            .iter()
            .filter(|channel| channel.channel_type == Some(ChannelType::Dm) && !channel.archived)
            .filter_map(|channel| match channel.participants.as_deref() {
                Some([other]) => Some(other.clone()),
                _ => None,
            })
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
