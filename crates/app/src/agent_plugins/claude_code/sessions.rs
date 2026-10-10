//! One agent's Claude sessions, a port of buzz-app's `sessions.ts`: one per
//! conversation (a thread, or a whole DM), each continuing across messages
//! and app restarts. A conversation in use keeps its process; a quiet one is
//! stopped and resumed from its saved session when it is next addressed. A
//! spare process waits for the next new conversation, so that starts
//! without a spawn.

use super::claude::{ClaudeProcess, Launch, Settled};
use crate::agent_plugins::process::Spawner;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

/// Processes an agent keeps, the spare included.
pub const MAX_LIVE: usize = 6;
/// A conversation's process stops after this long without a message.
pub const IDLE: Duration = Duration::from_secs(15 * 60);
/// Saved sessions per agent; the least recently used are forgotten.
const SAVED_LIMIT: usize = 200;

/// What is saved for one conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSession {
    pub id: String,
    /// `created_at` of the newest event this session has been shown.
    pub seen: u64,
    /// When it was last used, in milliseconds.
    pub at: u64,
}

pub trait SessionStore: Send + Sync {
    fn get(&self, key: &str) -> Option<SavedSession>;
    fn set(&self, key: &str, session: SavedSession);
    fn delete(&self, key: &str);
}

pub type LaunchFn = Arc<dyn Fn() -> BoxFuture<'static, Result<Launch, String>> + Send + Sync>;

pub struct SessionsOptions {
    pub spawn: Spawner,
    pub store: Arc<dyn SessionStore>,
    /// How to start a process with the agent's current settings; `resume`
    /// and `session_id` are filled in here.
    pub launch: LaunchFn,
    /// Changes when a setting that a running process fixed has changed.
    pub fingerprint: Arc<dyn Fn() -> String + Send + Sync>,
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub new_id: Arc<dyn Fn() -> String + Send + Sync>,
    /// Called when a conversation's process starts or stops.
    pub on_change: Arc<dyn Fn() + Send + Sync>,
    pub idle: Duration,
}

impl SessionsOptions {
    /// Options with the clock, ids and idle time of a real agent.
    pub fn new(
        spawn: Spawner,
        store: Arc<dyn SessionStore>,
        launch: LaunchFn,
        fingerprint: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        Self {
            spawn,
            store,
            launch,
            fingerprint,
            now: Arc::new(|| chrono::Utc::now().timestamp_millis().max(0) as u64),
            new_id: Arc::new(|| uuid::Uuid::new_v4().to_string()),
            on_change: Arc::new(|| {}),
            idle: IDLE,
        }
    }
}

struct Live {
    process: ClaudeProcess,
    fingerprint: String,
    used: Mutex<u64>,
    timer: Mutex<Option<tokio::task::AbortHandle>>,
}

type Starting = Shared<BoxFuture<'static, Option<Arc<Live>>>>;

#[derive(Default)]
struct State {
    /// In the order they started, for the status view.
    live: Vec<(String, Arc<Live>)>,
    opening: BTreeMap<String, (u64, Starting)>,
    spare: Option<(u64, Starting)>,
    disposed: bool,
}

impl State {
    fn get(&self, key: &str) -> Option<Arc<Live>> {
        self.live
            .iter()
            .find(|(other, _)| other == key)
            .map(|(_, live)| live.clone())
    }
    fn holds(&self, key: &str, live: &Arc<Live>) -> bool {
        self.get(key).is_some_and(|held| Arc::ptr_eq(&held, live))
    }
    /// Processes running or starting, the spare included.
    fn count(&self) -> usize {
        self.live.len() + self.opening.len() + usize::from(self.spare.is_some())
    }
}

struct Inner {
    options: SessionsOptions,
    state: Mutex<State>,
    next: AtomicU64,
}

/// One agent's sessions. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct AgentSessions {
    inner: Arc<Inner>,
}

/// One conversation's running process, for the status view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub key: String,
    pub busy: bool,
}

/// Steering and restarting a delivery.
#[derive(Default)]
pub struct DeliverOptions {
    /// Replaces the text when the session is mid-turn.
    pub steer: Option<String>,
    /// Builds the text again for a new session when the saved one is lost.
    pub fresh: Option<BoxFuture<'static, String>>,
}

fn kill_later(live: Arc<Live>) {
    tokio::spawn(async move { live.process.kill().await });
}

fn kill_starting(starting: Starting) {
    tokio::spawn(async move {
        if let Some(live) = starting.await {
            live.process.kill().await;
        }
    });
}

impl AgentSessions {
    pub fn new(options: SessionsOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                state: Mutex::default(),
                next: AtomicU64::new(1),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
    fn options(&self) -> &SessionsOptions {
        &self.inner.options
    }
    fn fingerprint(&self) -> String {
        (self.options().fingerprint)()
    }
    fn next_id(&self) -> u64 {
        self.inner.next.fetch_add(1, Ordering::Relaxed)
    }

    /// What `seen` says this conversation's session has been shown.
    pub fn seen(&self, key: &str) -> u64 {
        self.options().store.get(key).map_or(0, |saved| saved.seen)
    }

    /// Conversations with a running process, and whether each is working.
    pub fn snapshot(&self) -> Vec<SessionView> {
        self.state()
            .live
            .iter()
            .map(|(key, live)| SessionView {
                key: key.clone(),
                busy: live.process.busy(),
            })
            .collect()
    }

    /// Starts a spare process unless one exists or the agent is at its
    /// limit.
    pub fn warm(&self) {
        let mut state = self.state();
        if state.disposed || state.spare.is_some() || state.count() >= MAX_LIVE {
            return;
        }
        let this = self.clone();
        let spare: Starting = async move {
            match this.start(None).await {
                Ok(live) => Some(live),
                Err(error) => {
                    tracing::warn!(%error, "Claude Code could not start a spare session");
                    None
                }
            }
        }
        .boxed()
        .shared();
        let id = self.next_id();
        state.spare = Some((id, spare.clone()));
        drop(state);
        // A spare that dies, or was started with old settings, is not kept.
        let this = self.clone();
        tokio::spawn(async move {
            if let Some(live) = spare.await {
                live.process.exited().await;
                let mut state = this.state();
                if state.spare.as_ref().is_some_and(|(other, _)| *other == id) {
                    state.spare = None;
                }
            }
        });
    }

    /// Sends `text` into the conversation `key`, and resolves when the
    /// session has answered it.
    pub fn deliver(
        &self,
        key: String,
        text: String,
        seen: u64,
        options: DeliverOptions,
    ) -> BoxFuture<'static, Settled> {
        let this = self.clone();
        async move { this.deliver_now(key, text, seen, options).await }.boxed()
    }

    async fn deliver_now(
        &self,
        key: String,
        text: String,
        seen: u64,
        DeliverOptions { steer, fresh }: DeliverOptions,
    ) -> Settled {
        if self.state().disposed {
            return Settled::failed("Claude Code was stopped");
        }
        // A message that arrives while its conversation's process starts
        // uses it.
        let opening = self.state().opening.get(&key).map(|(_, open)| open.clone());
        if let Some(opening) = opening {
            opening.await;
        }
        let fingerprint = self.fingerprint();
        let mut live = self.state().get(&key);
        if let Some(current) = &live
            && (!current.process.running()
                || (current.fingerprint != fingerprint && !current.process.busy()))
        {
            self.drop_live(&key, current);
            live = None;
        }
        if let Some(live) = live {
            self.touch(&key, &live, Some(seen));
            let text = match (live.process.busy(), steer) {
                (true, Some(steer)) => steer,
                _ => text,
            };
            return self.answer(&key, &live, &text).await;
        }
        let saved = self.options().store.get(&key);
        let id = self.next_id();
        let this = self.clone();
        let resume = saved.as_ref().map(|saved| saved.id.clone());
        let opened = key.clone();
        // Adopted as part of the start, so a message that waited on it finds
        // the conversation's process.
        let starting: Starting = async move {
            let started = match resume {
                Some(resume) => this.start(Some(resume)).await,
                None => match this.claim_spare().await {
                    Some(live) => Ok(live),
                    None => this.start(None).await,
                },
            };
            let live = match started {
                Ok(live) => Some(live),
                Err(error) => {
                    tracing::warn!(%error, "Claude Code could not start");
                    None
                }
            };
            let disposed = {
                let mut state = this.state();
                if state
                    .opening
                    .get(&opened)
                    .is_some_and(|(other, _)| *other == id)
                {
                    state.opening.remove(&opened);
                }
                state.disposed
            };
            if let Some(live) = &live
                && !disposed
            {
                this.adopt(&opened, live, seen);
            }
            live
        }
        .boxed()
        .shared();
        let dropped = {
            // A new conversation takes the spare's place; any other start
            // needs room, reserved here, before anything is awaited.
            let mut state = self.state();
            let needs_room = saved.is_some() || state.spare.is_none();
            let (room, dropped) = if needs_room {
                self.room(&mut state)
            } else {
                (true, Vec::new())
            };
            if room {
                state.opening.insert(key.clone(), (id, starting.clone()));
            }
            drop(state);
            if !room {
                for (key, live) in dropped {
                    self.drop_live(&key, &live);
                }
                return Settled::failed(format!(
                    "Claude Code is already working in {MAX_LIVE} conversations; try again when one finishes"
                ));
            }
            dropped
        };
        for (key, live) in dropped {
            self.drop_live(&key, &live);
        }
        let Some(live) = starting.await else {
            return Settled::failed("Claude Code could not start");
        };
        if self.state().disposed {
            live.process.kill().await;
            return Settled::failed("Claude Code was stopped");
        }
        let delivered = self.answer(&key, &live, &text).await;
        // A saved session that no longer loads starts over rather than
        // failing every message in the conversation, shown the
        // conversation so far.
        if !delivered.ok && saved.is_some() && !live.process.started() {
            self.options().store.delete(&key);
            self.drop_live(&key, &live);
            let text = match fresh {
                Some(fresh) => fresh.await,
                None => text,
            };
            return self
                .deliver(key, text, seen, DeliverOptions::default())
                .await;
        }
        delivered
    }

    /// Stops idle processes started with old settings and replaces the
    /// spare; busy ones stop when they next fall idle.
    pub fn reconfigure(&self) {
        let fingerprint = self.fingerprint();
        let (stale, spare) = {
            let mut state = self.state();
            let stale: Vec<(String, Arc<Live>)> = state
                .live
                .iter()
                .filter(|(_, live)| live.fingerprint != fingerprint && !live.process.busy())
                .cloned()
                .collect();
            (stale, state.spare.take())
        };
        for (key, live) in stale {
            self.drop_live(&key, &live);
        }
        if let Some((_, spare)) = spare {
            kill_starting(spare);
        }
        self.warm();
    }

    pub fn dispose(&self) {
        let (live, spare) = {
            let mut state = self.state();
            state.disposed = true;
            (state.live.clone(), state.spare.take())
        };
        for (key, live) in live {
            self.drop_live(&key, &live);
        }
        if let Some((_, spare)) = spare {
            kill_starting(spare);
        }
    }

    async fn start(&self, resume: Option<String>) -> Result<Arc<Live>, String> {
        loop {
            // Read before `launch` reads the settings, so it never
            // describes newer ones.
            let fingerprint = self.fingerprint();
            let mut launch = (self.options().launch)().await?;
            match &resume {
                Some(resume) => launch.resume = Some(resume.clone()),
                None => launch.session_id = Some((self.options().new_id)()),
            }
            let process = ClaudeProcess::start(&self.options().spawn, launch).await?;
            if fingerprint == self.fingerprint() || self.state().disposed {
                return Ok(Arc::new(Live {
                    process,
                    fingerprint,
                    used: Mutex::new((self.options().now)()),
                    timer: Mutex::default(),
                }));
            }
            // Settings saved while it started: it would work with the old
            // ones.
            process.kill().await;
        }
    }

    async fn claim_spare(&self) -> Option<Arc<Live>> {
        let spare = self.state().spare.take()?;
        let live = spare.1.await?;
        if live.process.running() && live.fingerprint == self.fingerprint() {
            return Some(live);
        }
        live.process.kill().await;
        None
    }

    fn adopt(&self, key: &str, live: &Arc<Live>, seen: u64) {
        live.process.set_conversation(key);
        self.state().live.push((key.to_string(), live.clone()));
        self.touch(key, live, Some(seen));
        (self.options().on_change)();
        let this = Arc::downgrade(&self.inner);
        let (key, watched) = (key.to_string(), live.clone());
        tokio::spawn(async move {
            watched.process.exited().await;
            if let Some(sessions) = upgrade(&this)
                && sessions.state().holds(&key, &watched)
            {
                sessions.drop_live(&key, &watched);
            }
        });
        self.warm();
    }

    async fn answer(&self, key: &str, live: &Arc<Live>, text: &str) -> Settled {
        let settled = live.process.send(text).await;
        if self.state().holds(key, live) {
            self.touch(key, live, None);
            if live.fingerprint != self.fingerprint() && !live.process.busy() {
                self.drop_live(key, live);
            }
        }
        settled
    }

    /// Saves the session and restarts its idle countdown.
    fn touch(&self, key: &str, live: &Arc<Live>, seen: Option<u64>) {
        let used = (self.options().now)();
        *live.used.lock().unwrap_or_else(PoisonError::into_inner) = used;
        let this = Arc::downgrade(&self.inner);
        let (owned_key, watched) = (key.to_string(), live.clone());
        let idle = self.options().idle;
        let timer = tokio::spawn(async move {
            tokio::time::sleep(idle).await;
            let Some(sessions) = upgrade(&this) else {
                return;
            };
            if watched.process.busy() {
                sessions.touch(&owned_key, &watched, None);
            } else if sessions.state().holds(&owned_key, &watched) {
                sessions.drop_live(&owned_key, &watched);
            }
        });
        if let Some(previous) = live
            .timer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(timer.abort_handle())
        {
            previous.abort();
        }
        let Some(id) = live.process.session_id() else {
            return;
        };
        let store = &self.options().store;
        let prior = store.get(key);
        let prior_seen = prior
            .filter(|prior| prior.id == id)
            .map_or(0, |prior| prior.seen);
        store.set(
            key,
            SavedSession {
                id,
                seen: seen.unwrap_or(0).max(prior_seen),
                at: used,
            },
        );
    }

    /// Makes room for one more process by stopping the longest-idle
    /// conversation, else the spare. False when every process is working.
    /// Returns the conversations to stop once the lock is released.
    fn room(&self, state: &mut State) -> (bool, Vec<(String, Arc<Live>)>) {
        let mut dropped = Vec::new();
        while state.count() >= MAX_LIVE {
            let idle = state
                .live
                .iter()
                .filter(|(_, live)| !live.process.busy())
                .min_by_key(|(_, live)| *live.used.lock().unwrap_or_else(PoisonError::into_inner))
                .cloned();
            if let Some((key, live)) = idle {
                state
                    .live
                    .retain(|(other, held)| !(other == &key && Arc::ptr_eq(held, &live)));
                dropped.push((key, live));
            } else if let Some((_, spare)) = state.spare.take() {
                kill_starting(spare);
            } else {
                return (false, dropped);
            }
        }
        (true, dropped)
    }

    fn drop_live(&self, key: &str, live: &Arc<Live>) {
        if let Some(timer) = live
            .timer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            timer.abort();
        }
        // Taken out of the list here, or already by `room`.
        self.state()
            .live
            .retain(|(other, held)| !(other == key && Arc::ptr_eq(held, live)));
        (self.options().on_change)();
        kill_later(live.clone());
    }
}

fn upgrade(inner: &Weak<Inner>) -> Option<AgentSessions> {
    inner.upgrade().map(|inner| AgentSessions { inner })
}

/// Sessions saved in a JSON file, by agent and then conversation: what
/// buzz-app keeps in local storage under `buzz.claude-code.sessions.v1`.
pub struct FileSessions {
    path: PathBuf,
    agent: String,
}

/// Agents of one app share the file; their writes take turns.
static FILE_LOCK: Mutex<()> = Mutex::new(());

type AllSessions = BTreeMap<String, BTreeMap<String, SavedSession>>;

impl FileSessions {
    pub fn new(path: PathBuf, agent: &str) -> Self {
        Self {
            path,
            agent: agent.into(),
        }
    }

    /// `<data dir>/buzz-gpui/claude-code-sessions.json`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_dir().map(|dir| dir.join("buzz-gpui").join("claude-code-sessions.json"))
    }

    fn read_all(&self) -> AllSessions {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|value| {
                let object = value.as_object()?;
                Some(
                    object
                        .iter()
                        .map(|(agent, sessions)| {
                            let sessions = sessions
                                .as_object()
                                .map(|sessions| {
                                    sessions
                                        .iter()
                                        .filter_map(|(key, saved)| {
                                            serde_json::from_value(saved.clone())
                                                .ok()
                                                .map(|saved| (key.clone(), saved))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            (agent.clone(), sessions)
                        })
                        .collect(),
                )
            })
            .unwrap_or_default()
    }

    fn update(&self, change: impl FnOnce(&mut BTreeMap<String, SavedSession>)) {
        let _turn = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let mut all = self.read_all();
        let mut sessions = all.remove(&self.agent).unwrap_or_default();
        change(&mut sessions);
        let mut kept: Vec<(String, SavedSession)> = sessions.into_iter().collect();
        kept.sort_by_key(|(_, saved)| std::cmp::Reverse(saved.at));
        kept.truncate(SAVED_LIMIT);
        all.insert(self.agent.clone(), kept.into_iter().collect());
        let written = (|| {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = self.path.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec(&all)?)?;
            std::fs::rename(&temporary, &self.path)
        })();
        if let Err(error) = written {
            tracing::warn!(%error, "Claude Code sessions were not saved");
        }
    }
}

impl SessionStore for FileSessions {
    fn get(&self, key: &str) -> Option<SavedSession> {
        let _turn = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        self.read_all().remove(&self.agent)?.remove(key)
    }
    fn set(&self, key: &str, session: SavedSession) {
        self.update(|sessions| {
            sessions.insert(key.into(), session);
        });
    }
    fn delete(&self, key: &str) {
        self.update(|sessions| {
            sessions.remove(key);
        });
    }
}

#[cfg(test)]
mod tests;
