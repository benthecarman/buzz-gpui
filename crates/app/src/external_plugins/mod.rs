//! Plugins people build themselves (`docs/plugins.md`).
//!
//! buzz-app loads a self-contained `plugin.js` into its WebView. This app
//! has no JavaScript runtime, so a plugin here is a program: a folder with
//! a `buzz-plugin.json` naming the command to run. The app starts it,
//! asks what it adds (panels, pages), and draws the element trees it sends
//! with the app's own components. Clicks and typing go back to it.
//!
//! The trust model is buzz-app's: plugins are trusted code with the
//! user's full access, not sandboxed. Settings → Plugins shows the command
//! before a plugin is turned on, and a new plugin starts off.
//!
//! Plugins load from the folder the user picked, and the app watches it:
//! saving a file restarts the plugin, and open views ask for a new tree.

mod agents;
mod capabilities;
mod channels;
mod content;
mod contribute;
mod dialogs;
pub use dialogs::dialog;
mod draw;
mod events;
pub mod git;
mod message_view;
mod naming;
mod observed;
mod outbox;
mod process;
pub mod protocol;
mod reading;
pub mod release;
mod view;
mod workflows;

use crate::community::AppState;
use crate::runtime::Runtime;
use crate::ui::channel_panels::{self, AppLauncher, ChannelPanel, Launcher, MenuEntry, Placement};
use crate::ui::pages::{self, Page, PageSpec};
use crate::ui::toasts::{self, Notice};
use buzz_relay_client::ChannelType;
use buzz_ui::{IconName, ToastTone};
use futures::StreamExt;
use gpui_kit::*;
use process::{Connection, Event};
use protocol::{
    Contributions, INVALID_PARAMS, LauncherPlacement, MANIFEST, METHOD_NOT_FOUND, PagePlacement,
    Permission,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use protocol::Manifest;
pub use view::ExternalView;

/// Most watches one plugin holds at once, of every kind.
const MAX_WATCHES: usize = 64;
/// A request the plugin has not been allowed to make.
const FORBIDDEN: i64 = 4;

/// An answer now, or one that comes when a read or send finishes.
enum Reply {
    Now(Result<Value, capabilities::Failure>),
    Later(Task<Result<Value, capabilities::Failure>>),
}

/// The saved list of plugin folders, beside buzz-app's plugin registry.
const SAVED: &str = "external.json";
/// How often plugin folders are checked for changes.
const POLL: Duration = Duration::from_secs(1);

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    version: u32,
    plugins: Vec<SavedPlugin>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SavedPlugin {
    path: PathBuf,
    enabled: bool,
    /// The permissions the user saw when turning it on.
    #[serde(default)]
    approved: Vec<Permission>,
    /// The key that signed it when it was first seen signed; later
    /// versions must come from the same publisher.
    #[serde(default)]
    publisher: Option<String>,
    /// Where an installed copy came from.
    #[serde(default)]
    source: Option<GitSource>,
}

/// A plugin installed from a Git repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSource {
    pub repository: String,
    /// A branch or tag; empty for the default branch.
    pub reference: String,
    /// The plugin's folder in the repository.
    pub folder: String,
    pub commit: String,
}

/// What installing a plugin from a repository does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Add,
    /// Replaces the copy installed from commit `from`.
    Update {
        from: String,
    },
}

/// Where installed copies live.
fn installed_dir() -> PathBuf {
    crate::plugins::home().join("installed")
}

/// Where a plugin is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Off,
    /// On, but safe mode keeps plugins from running this launch.
    Paused,
    Starting,
    Running,
    /// It could not start, or it stopped by itself.
    Failed(String),
}

/// What a plugin added, so it can be taken away again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Added {
    Panel(&'static str),
    Page(Page),
    Other(contribute::Added),
    Agent(agents::AgentAdded),
}

pub struct Plugin {
    pub path: PathBuf,
    pub enabled: bool,
    pub manifest: Result<Manifest, String>,
    pub status: Status,
    approved: Vec<Permission>,
    /// The publisher trusted for this plugin.
    pub publisher: Option<String>,
    pub source: Option<GitSource>,
    /// Bumped on every start; events from an older run are ignored.
    generation: u64,
    connection: Option<Connection>,
    added: Vec<Added>,
    fingerprint: Option<u64>,
    /// Open views, by view id.
    views: HashMap<String, WeakEntity<ExternalView>>,
    /// Channels this run follows, by watch id.
    watches: HashMap<u64, capabilities::Watch>,
    next_watch: u64,
    /// The services this run offers other plugins.
    services: Vec<String>,
    /// The services this run hears events from, as `(plugin, service)`.
    subscriptions: std::collections::HashSet<(String, String)>,
}

impl Plugin {
    fn load(saved: SavedPlugin) -> Self {
        Self {
            manifest: read_manifest(&saved.path),
            fingerprint: fingerprint(&saved.path),
            path: saved.path,
            enabled: saved.enabled,
            approved: saved.approved,
            publisher: saved.publisher,
            source: saved.source,
            status: Status::Off,
            generation: 0,
            connection: None,
            added: Vec::new(),
            views: HashMap::new(),
            watches: HashMap::new(),
            next_watch: 0,
            services: Vec::new(),
            subscriptions: Default::default(),
        }
    }

    /// Whether the user allowed `permission` and the manifest asks for it.
    pub fn may(&self, permission: Permission) -> bool {
        self.approved.contains(&permission)
            && self
                .manifest
                .as_ref()
                .is_ok_and(|manifest| manifest.permissions.contains(&permission))
    }

    /// Permissions the manifest asks for that the user has not allowed.
    fn unapproved(&self) -> Vec<Permission> {
        self.manifest.as_ref().map_or_else(
            |_| Vec::new(),
            |manifest| {
                manifest
                    .permissions
                    .iter()
                    .copied()
                    .filter(|permission| !self.approved.contains(permission))
                    .collect()
            },
        )
    }

    pub fn id(&self) -> Option<&str> {
        self.manifest
            .as_ref()
            .ok()
            .map(|manifest| manifest.id.as_str())
    }

    /// The manifest's name, else the folder's.
    pub fn name(&self) -> String {
        match &self.manifest {
            Ok(manifest) => manifest.name.clone(),
            Err(_) => self
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.display().to_string()),
        }
    }
}

pub struct ExternalPlugins {
    /// `BUZZODZ_SAFE_MODE=1`, as in buzz-app: plugins people added stay
    /// paused for this launch.
    pub safe_mode: bool,
    file: PathBuf,
    plugins: Vec<Plugin>,
    next_view: u64,
}

impl Global for ExternalPlugins {}

fn read_manifest(folder: &Path) -> Result<Manifest, String> {
    let path = folder.join(MANIFEST);
    let text = std::fs::read_to_string(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => format!("This folder has no {MANIFEST}."),
        _ => format!("Could not read {MANIFEST}: {error}"),
    })?;
    Manifest::parse(&text)
}

/// A hash of the folder's file names, sizes and times, to notice saves.
/// Hidden entries, `node_modules`, `target` and `__pycache__` are skipped,
/// so a plugin can keep state under a dot folder without restarting.
fn fingerprint(folder: &Path) -> Option<u64> {
    const SKIP: [&str; 4] = ["node_modules", "target", "__pycache__", "venv"];
    const MAX_ENTRIES: usize = 5_000;
    const MAX_DEPTH: usize = 8;
    let mut hasher = DefaultHasher::new();
    let mut stack = vec![(folder.to_path_buf(), 0)];
    let mut seen = 0;
    while let Some((directory, depth)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&directory).ok()?.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            seen += 1;
            if seen > MAX_ENTRIES {
                return Some(hasher.finish());
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIP.contains(&name.as_ref()) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            entry.path().hash(&mut hasher);
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
                continue;
            }
            metadata.len().hash(&mut hasher);
            if let Ok(modified) = metadata.modified() {
                modified.hash(&mut hasher);
            }
        }
    }
    Some(hasher.finish())
}

thread_local! {
    static INTERNED: RefCell<HashSet<&'static str>> = RefCell::default();
}

/// Registries key contributions by `&'static str`. Each distinct key or
/// title a plugin sends is leaked once, however often it restarts.
fn intern(text: String) -> &'static str {
    INTERNED.with_borrow_mut(|interned| match interned.get(text.as_str()) {
        Some(existing) => *existing,
        None => {
            let leaked: &'static str = Box::leak(text.into_boxed_str());
            interned.insert(leaked);
            leaked
        }
    })
}

fn icon(name: Option<&str>) -> IconName {
    name.and_then(|name| {
        IconName::ALL
            .iter()
            .copied()
            .find(|icon| icon.name().eq_ignore_ascii_case(name))
    })
    .unwrap_or(IconName::Plug)
}

impl ExternalPlugins {
    pub fn init(cx: &mut App) {
        contribute::init(cx);
        let file = crate::plugins::home().join(SAVED);
        let saved: Saved = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        cx.set_global(Self {
            safe_mode: std::env::var("BUZZODZ_SAFE_MODE").as_deref() == Ok("1"),
            file,
            plugins: saved.plugins.into_iter().map(Plugin::load).collect(),
            next_view: 0,
        });
        let enabled: Vec<PathBuf> = cx
            .global::<Self>()
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled)
            .map(|plugin| plugin.path.clone())
            .collect();
        for path in enabled {
            Self::start(&path, cx);
        }
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let paths = cx.update(|cx| {
                    cx.global::<Self>()
                        .plugins
                        .iter()
                        .map(|plugin| plugin.path.clone())
                        .collect::<Vec<_>>()
                });
                let prints = cx
                    .background_executor()
                    .spawn(async move {
                        paths
                            .into_iter()
                            .map(|path| {
                                let print = fingerprint(&path);
                                (path, print)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                cx.update(|cx| Self::changed(prints, cx));
            }
        })
        .detach();
        // Tells running plugins when the selected community, or its
        // connection, changes (`communityReader.subscribe`).
        // `communities/changed` likewise follows joining and leaving.
        cx.spawn(async move |cx| {
            let (mut last, mut last_list) = (Value::Null, Value::Null);
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                cx.update(|cx| {
                    let ids: Vec<String> = cx
                        .global::<Self>()
                        .plugins
                        .iter()
                        .filter(|plugin| plugin.status == Status::Running)
                        .filter_map(|plugin| plugin.id().map(str::to_owned))
                        .collect();
                    let now = Self::current_community(cx);
                    if now != last {
                        last = now.clone();
                        for id in &ids {
                            Self::notify_running(id, "community/changed", now.clone(), cx);
                        }
                    }
                    let list = Self::community_list(cx);
                    if list != last_list {
                        // Not on the first look: plugins read it at start.
                        let first = last_list.is_null();
                        last_list = list.clone();
                        if !first {
                            let params = json!({ "communities": list });
                            for id in &ids {
                                Self::notify_running(id, "communities/changed", params.clone(), cx);
                            }
                        }
                    }
                });
            }
        })
        .detach();
    }

    /// `community/current`: the selected community, who the user is there,
    /// and its connection.
    /// The communities you joined, and which one is selected.
    fn community_list(cx: &App) -> Value {
        let state = cx.global::<AppState>();
        Value::Array(
            state
                .communities
                .iter()
                .enumerate()
                .map(|(index, community)| {
                    let community = community.read(cx);
                    json!({
                        "name": community.name(),
                        "relay": community.origin.websocket(),
                        "icon": community.icon(),
                        "selected": state.selected == Some(index),
                    })
                })
                .collect(),
        )
    }

    /// `community/client` (`communityReader.snapshot`): who is signed in,
    /// the personal profile new communities start from, and the
    /// communities joined.
    fn client(cx: &App) -> Value {
        use crate::community::IdentityState;
        let state = cx.global::<AppState>();
        let (status, viewer, error) = match &state.identity {
            IdentityState::Loading => ("loading", None, None),
            IdentityState::Ready(identity) => ("ready", Some(identity.pubkey_hex()), None),
            IdentityState::Missing => ("unavailable", None, None),
            IdentityState::Error(error) => ("unavailable", None, Some(error.clone())),
        };
        let profile = crate::ui::settings::default_profile(cx);
        json!({
            "status": status,
            "viewer": viewer,
            "error": error,
            "profile": {
                "name": profile.name,
                "picture": profile.picture,
                "about": profile.about,
            },
            "communities": Self::community_list(cx),
        })
    }

    fn current_community(cx: &App) -> Value {
        let Some(community) = AppState::current(cx) else {
            return Value::Null;
        };
        let viewer = match &cx.global::<AppState>().identity {
            crate::community::IdentityState::Ready(identity) => Some(identity.pubkey_hex()),
            _ => None,
        };
        let community = community.read(cx);
        let snapshot = &community.snapshot;
        json!({
            "name": community.name(),
            "relay": community.origin.websocket(),
            "viewer": viewer,
            "status": match &snapshot.status {
                buzz_relay_client::SessionStatus::Connecting => "connecting",
                buzz_relay_client::SessionStatus::Ready => "ready",
                buzz_relay_client::SessionStatus::Failed(_) => "failed",
            },
            "live": snapshot.live,
            "cached": snapshot.cached,
        })
    }

    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    fn index(&self, path: &Path) -> Option<usize> {
        self.plugins.iter().position(|plugin| plugin.path == path)
    }

    fn by_id(&self, id: &str) -> Option<&Plugin> {
        self.plugins
            .iter()
            .find(|plugin| plugin.enabled && plugin.id() == Some(id))
    }

    fn by_id_mut(&mut self, id: &str) -> Option<&mut Plugin> {
        self.plugins
            .iter_mut()
            .find(|plugin| plugin.enabled && plugin.id() == Some(id))
    }

    fn save(&self) -> Result<(), String> {
        let saved = Saved {
            version: 1,
            plugins: self
                .plugins
                .iter()
                .map(|plugin| SavedPlugin {
                    path: plugin.path.clone(),
                    enabled: plugin.enabled,
                    approved: plugin.approved.clone(),
                    publisher: plugin.publisher.clone(),
                    source: plugin.source.clone(),
                })
                .collect(),
        };
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let bytes = serde_json::to_vec_pretty(&saved).map_err(|error| error.to_string())?;
        crate::atomic_file::write(&self.file, &bytes).map_err(|error| error.to_string())
    }

    fn saved(cx: &mut App) {
        if let Err(reason) = cx.global::<Self>().save() {
            toasts::show(
                Notice::error("plugin-save", "Plugin settings weren’t saved").description(reason),
                cx,
            );
        }
    }

    /// Adds the plugin in `folder`, off. A folder already added is kept
    /// as it is.
    pub fn add(folder: PathBuf, cx: &mut App) -> Result<(), String> {
        let folder = folder.canonicalize().unwrap_or(folder);
        if cx.global::<Self>().index(&folder).is_some() {
            return Ok(());
        }
        let manifest = read_manifest(&folder)?;
        if let Some(other) = cx
            .global::<Self>()
            .plugins
            .iter()
            .find(|plugin| plugin.id() == Some(manifest.id.as_str()))
        {
            return Err(format!(
                "{} already uses the id {}. Remove it first.",
                other.name(),
                manifest.id
            ));
        }
        cx.global_mut::<Self>()
            .plugins
            .push(Plugin::load(SavedPlugin {
                path: folder,
                enabled: false,
                approved: Vec::new(),
                publisher: None,
                source: None,
            }));
        Self::saved(cx);
        cx.refresh_windows();
        Ok(())
    }

    pub fn remove(path: &Path, cx: &mut App) {
        Self::stop(path, Status::Off, cx);
        let state = cx.global_mut::<Self>();
        let installed = state
            .plugins
            .iter()
            .any(|plugin| plugin.path == path && plugin.source.is_some());
        state.plugins.retain(|plugin| plugin.path != path);
        Self::saved(cx);
        // An installed copy is the app's own; a folder the user added is
        // theirs and stays.
        if installed && path.starts_with(installed_dir()) {
            let path = path.to_path_buf();
            cx.background_executor()
                .spawn(async move {
                    let _ = std::fs::remove_dir_all(path);
                })
                .detach();
        }
        cx.refresh_windows();
    }

    /// What installing `candidate` would do, or why it cannot.
    pub fn install_plan(candidate: &git::Candidate, cx: &App) -> Result<Plan, String> {
        let manifest = candidate.manifest.as_ref().map_err(Clone::clone)?;
        let signer = candidate
            .signed
            .as_ref()
            .map_err(|reason| format!("Its signature check failed. {reason}"))?;
        let Some(existing) = cx
            .global::<Self>()
            .plugins
            .iter()
            .find(|plugin| plugin.id() == Some(manifest.id.as_str()))
        else {
            return Ok(Plan::Add);
        };
        let Some(source) = &existing.source else {
            return Err(format!(
                "{} is already added from {}. Remove it first.",
                existing.name(),
                existing.path.display()
            ));
        };
        match (&existing.publisher, signer) {
            (Some(_), None) => Err(
                "The installed copy is signed and this one is not. Remove it first to trust an \
                 unsigned copy."
                    .to_owned(),
            ),
            (Some(trusted), Some(signer)) if trusted != signer => Err(format!(
                "The installed copy is signed by {}, and this one by {}. Remove it first to \
                 trust the new publisher.",
                release::short_npub(trusted),
                release::short_npub(signer)
            )),
            _ => Ok(Plan::Update {
                from: source.commit.clone(),
            }),
        }
    }

    /// Installs `folder` from `fetched`, or updates the installed copy.
    /// A new plugin starts off; an update keeps its switch and restarts.
    pub fn install(
        fetched: std::sync::Arc<git::Fetched>,
        folder: String,
        cx: &mut App,
    ) -> Task<Result<String, String>> {
        let planned = fetched
            .candidate(&folder)
            .ok_or_else(|| "That plugin is not in the repository.".to_owned())
            .and_then(|candidate| {
                let plan = Self::install_plan(candidate, cx)?;
                let manifest = candidate.manifest.clone()?;
                let signer = candidate.signed.clone().unwrap_or_default();
                Ok((plan, manifest, signer))
            });
        let (plan, manifest, signer) = match planned {
            Ok(planned) => planned,
            Err(reason) => return Task::ready(Err(reason)),
        };
        let path = match plan {
            Plan::Add => installed_dir().join(&manifest.id),
            Plan::Update { .. } => cx
                .global::<Self>()
                .plugins
                .iter()
                .find(|plugin| plugin.id() == Some(manifest.id.as_str()))
                .map(|plugin| plugin.path.clone())
                .unwrap_or_else(|| installed_dir().join(&manifest.id)),
        };
        let source = GitSource {
            repository: fetched.repository.clone(),
            reference: fetched.reference.clone(),
            folder: folder.clone(),
            commit: fetched.commit.clone(),
        };
        let into = path.clone();
        let copy = cx
            .background_executor()
            .spawn(async move { fetched.copy(&folder, &into) });
        cx.spawn(async move |cx| {
            copy.await?;
            cx.update(|cx| {
                let state = cx.global_mut::<Self>();
                match state.index(&path) {
                    Some(index) => {
                        let plugin = &mut state.plugins[index];
                        plugin.source = Some(source);
                        if plugin.publisher.is_none() {
                            plugin.publisher = signer;
                        }
                        Self::saved(cx);
                        Self::reload(&path, cx);
                    }
                    None => {
                        state.plugins.push(Plugin::load(SavedPlugin {
                            path,
                            enabled: false,
                            approved: Vec::new(),
                            publisher: signer,
                            source: Some(source),
                        }));
                        Self::saved(cx);
                    }
                }
                cx.refresh_windows();
            });
            Ok(manifest.name)
        })
    }

    /// Signs a plugin added from a folder as the user.
    pub fn sign(path: &Path, cx: &mut App) -> Task<Result<String, String>> {
        let identity = match &cx.global::<AppState>().identity {
            crate::community::IdentityState::Ready(identity) => identity.clone(),
            _ => return Task::ready(Err("Sign in to sign plugins.".into())),
        };
        let Some(id) = cx
            .global::<Self>()
            .plugins
            .iter()
            .find(|plugin| plugin.path == path && plugin.source.is_none())
            .and_then(|plugin| plugin.id().map(str::to_owned))
        else {
            return Task::ready(Err(
                "Only a plugin added from a folder can be signed.".into()
            ));
        };
        let folder = path.to_path_buf();
        cx.background_executor()
            .spawn(async move { release::sign(&folder, &id, &identity) })
    }

    /// Turning a plugin on allows the permissions Settings shows for it.
    pub fn set_enabled(path: &Path, enabled: bool, cx: &mut App) {
        let state = cx.global_mut::<Self>();
        let Some(index) = state.index(path) else {
            return;
        };
        let plugin = &mut state.plugins[index];
        plugin.enabled = enabled;
        if enabled && let Ok(manifest) = &plugin.manifest {
            plugin.approved = manifest.permissions.clone();
        }
        Self::saved(cx);
        if enabled {
            Self::start(path, cx);
        } else {
            Self::stop(path, Status::Off, cx);
        }
    }

    /// Reads the manifest again and restarts the plugin if it is on.
    pub fn reload(path: &Path, cx: &mut App) {
        let state = cx.global_mut::<Self>();
        let Some(index) = state.index(path) else {
            return;
        };
        let plugin = &mut state.plugins[index];
        let before = plugin.id().map(str::to_owned);
        plugin.manifest = read_manifest(path);
        // Already restarting for this version: the poll need not.
        plugin.fingerprint = fingerprint(path);
        if plugin.id() != before.as_deref() {
            // Another id: what the old one added cannot reach it.
            let added = std::mem::take(&mut plugin.added);
            take_away(added, cx);
        }
        let plugin = &cx.global::<Self>().plugins[index];
        if plugin.enabled {
            Self::start(path, cx);
        }
        cx.refresh_windows();
    }

    /// A view's Restart button.
    pub fn restart(id: &str, cx: &mut App) {
        if let Some(path) = cx
            .global::<Self>()
            .by_id(id)
            .map(|plugin| plugin.path.clone())
        {
            Self::reload(&path, cx);
        }
    }

    pub fn status(id: &str, cx: &App) -> Status {
        cx.try_global::<Self>()
            .and_then(|state| state.by_id(id))
            .map_or(Status::Off, |plugin| plugin.status.clone())
    }

    fn changed(prints: Vec<(PathBuf, Option<u64>)>, cx: &mut App) {
        let mut restart = Vec::new();
        let state = cx.global_mut::<Self>();
        for (path, print) in prints {
            let Some(index) = state.index(&path) else {
                continue;
            };
            let plugin = &mut state.plugins[index];
            if plugin.fingerprint == print {
                continue;
            }
            plugin.fingerprint = print;
            tracing::info!(path = %path.display(), "plugin folder changed");
            restart.push(path);
        }
        for path in restart {
            Self::reload(&path, cx);
        }
    }

    /// Starts (or restarts) the plugin at `path`.
    fn start(path: &Path, cx: &mut App) {
        let state = cx.global_mut::<Self>();
        let Some(index) = state.index(path) else {
            return;
        };
        let manifest = match state.plugins[index].manifest.clone() {
            Ok(manifest) => manifest,
            Err(reason) => {
                Self::stop(path, Status::Failed(reason), cx);
                return;
            }
        };
        let unapproved = state.plugins[index].unapproved();
        if !unapproved.is_empty() {
            // A new version asks for more: the user reviews it again.
            let name = state.plugins[index].name();
            state.plugins[index].enabled = false;
            Self::saved(cx);
            let asks = unapproved
                .iter()
                .map(|permission| permission.describe())
                .collect::<Vec<_>>()
                .join(", ");
            toasts::show(
                Notice::error(exit_notice(path), format!("{name} was turned off")).description(
                    format!(
                        "It now asks to: {asks}. Review it in Settings → Plugins and turn it on \
                         again to allow it."
                    ),
                ),
                cx,
            );
            Self::stop(path, Status::Off, cx);
            return;
        }
        let state = cx.global_mut::<Self>();
        let taken = state.plugins.iter().any(|other| {
            other.path != path
                && other.enabled
                && other.id() == Some(manifest.id.as_str())
                && other.status != Status::Off
        });
        if taken {
            let reason = format!("Another plugin already uses the id {}.", manifest.id);
            Self::stop(path, Status::Failed(reason), cx);
            return;
        }
        if state.safe_mode {
            Self::stop(path, Status::Paused, cx);
            return;
        }
        let plugin = &mut state.plugins[index];
        // The old run ends; what it added stays until the new run says
        // what it adds, so open views do not flicker on a save.
        plugin.connection = None;
        plugin.watches.clear();
        plugin.subscriptions.clear();
        plugin.generation += 1;
        let offered = plugin.status == Status::Running && !plugin.services.is_empty();
        plugin.status = Status::Starting;
        let generation = plugin.generation;
        agents::close_all(&manifest.id);
        if offered {
            Self::services_changed(cx);
        }
        // A signed folder is checked before every start, off this thread.
        let folder = path.to_path_buf();
        let id = manifest.id.clone();
        let check = cx
            .background_executor()
            .spawn(async move { release::verify(&folder, &id) });
        let path = path.to_path_buf();
        cx.spawn(async move |cx| {
            let signed = check.await;
            cx.update(|cx| Self::verified(&path, generation, manifest, signed, cx));
        })
        .detach();
        cx.refresh_windows();
    }

    /// The signature check finished: start the program if it passed.
    fn verified(
        path: &Path,
        generation: u64,
        manifest: Manifest,
        signed: Result<Option<String>, String>,
        cx: &mut App,
    ) {
        let runtime = Runtime::handle(cx);
        let state = cx.global_mut::<Self>();
        let Some(plugin) = state.current(path, generation) else {
            return;
        };
        let publisher = match (signed, plugin.publisher.clone()) {
            (Err(reason), _) => Err(format!("Its signature check failed. {reason}")),
            (Ok(None), Some(_)) => Err(
                "It was signed before and is not now. Remove it and add it again to trust an \
                 unsigned copy."
                    .to_owned(),
            ),
            (Ok(Some(signer)), Some(trusted)) if signer != trusted => Err(format!(
                "It is now signed by {}, not {}. Remove it and add it again to trust the new \
                 publisher.",
                release::short_npub(&signer),
                release::short_npub(&trusted)
            )),
            (Ok(signer), _) => Ok(signer),
        };
        let signer = match publisher {
            Ok(signer) => signer,
            Err(reason) => {
                Self::stop(path, Status::Failed(reason), cx);
                return;
            }
        };
        if plugin.publisher.is_none() && signer.is_some() {
            // First seen signed: later versions must match.
            plugin.publisher = signer;
            Self::saved(cx);
        }
        let state = cx.global_mut::<Self>();
        let Some(plugin) = state.current(path, generation) else {
            return;
        };
        let (connection, mut events) = match Connection::start(&manifest, path, &runtime) {
            Ok(started) => started,
            Err(reason) => {
                Self::stop(path, Status::Failed(reason), cx);
                return;
            }
        };
        let initialize = connection.request(
            "initialize",
            json!({
                "apiVersion": protocol::API_VERSION,
                "app": { "name": "Buzz", "version": env!("CARGO_PKG_VERSION") },
                // buzz-app's `browser.available`: this app has no embedded
                // browser, so links open in the system's.
                "host": { "browser": false },
            }),
        );
        plugin.connection = Some(connection);
        let path = path.to_path_buf();
        {
            let path = path.clone();
            cx.spawn(async move |cx| {
                let result = match initialize.await {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                };
                cx.update(|cx| Self::initialized(&path, generation, result, cx));
            })
            .detach();
        }
        cx.spawn(async move |cx| {
            while let Some(event) = events.next().await {
                cx.update(|cx| Self::event(&path, generation, event, cx));
            }
        })
        .detach();
        cx.refresh_windows();
    }

    /// Ends the plugin's run. Turned off, it takes away what it added; a
    /// plugin that failed keeps its pages and panels, which show why and
    /// offer a restart.
    fn stop(path: &Path, status: Status, cx: &mut App) {
        let state = cx.global_mut::<Self>();
        let Some(index) = state.index(path) else {
            return;
        };
        let plugin = &mut state.plugins[index];
        plugin.connection = None;
        plugin.watches.clear();
        plugin.subscriptions.clear();
        plugin.generation += 1;
        let off = status == Status::Off;
        let offered = plugin.status == Status::Running && !plugin.services.is_empty();
        plugin.status = status;
        if let Some(id) = plugin.id() {
            contribute::forget(id);
            agents::close_all(id);
        }
        if off {
            let added = std::mem::take(&mut plugin.added);
            take_away(added, cx);
        }
        if offered {
            Self::services_changed(cx);
        }
        cx.refresh_windows();
    }

    fn current(&mut self, path: &Path, generation: u64) -> Option<&mut Plugin> {
        self.plugins
            .iter_mut()
            .find(|plugin| plugin.path == path && plugin.generation == generation)
    }

    fn initialized(
        path: &Path,
        generation: u64,
        result: Result<serde_json::Value, String>,
        cx: &mut App,
    ) {
        let state = cx.global_mut::<Self>();
        let Some(plugin) = state.current(path, generation) else {
            return;
        };
        let Some(id) = plugin.id().map(str::to_owned) else {
            return;
        };
        let name = plugin.name();
        let contributions = match result.and_then(Contributions::parse) {
            Ok(contributions) => contributions,
            // It exited; the exit says why, with its stderr.
            Err(reason) if reason == process::STOPPED => return,
            Err(reason) => {
                Self::stop(path, Status::Failed(reason), cx);
                return;
            }
        };
        plugin.status = Status::Running;
        toasts::dismiss(&exit_notice(path), cx);
        let state = cx.global_mut::<Self>();
        let Some(plugin) = state.current(path, generation) else {
            return;
        };
        let before = std::mem::take(&mut plugin.added);
        // Renderer answers and marks belong to the old run.
        contribute::forget(&id);
        let requester = plugin.connection.as_ref().map(Connection::requester);
        let mut added = Self::register(&id, &name, &contributions, requester.clone(), cx);
        if let Some(requester) = requester {
            let (agents, errors) = agents::register(
                &id,
                &contributions.agent_tools,
                &contributions.agent_types,
                requester,
                cx,
            );
            added.extend(agents.into_iter().map(Added::Agent));
            if !errors.is_empty() {
                toasts::show(
                    Notice::error(
                        exit_notice(path),
                        format!("{name} could not add everything"),
                    )
                    .description(errors.join(" ")),
                    cx,
                );
            }
        }
        take_away(
            before
                .into_iter()
                .filter(|old| !added.contains(old))
                .collect(),
            cx,
        );
        let state = cx.global_mut::<Self>();
        let Some(plugin) = state.current(path, generation) else {
            return;
        };
        plugin.added = added;
        plugin.services = contributions
            .services
            .iter()
            .map(|service| service.id.clone())
            .collect();
        let offers = !plugin.services.is_empty();
        // Views open before this run ask the new run for their trees.
        let views: Vec<_> = plugin.views.values().cloned().collect();
        if offers {
            Self::services_changed(cx);
        }
        for view in views {
            if let Some(view) = view.upgrade() {
                let params = view.read(cx).open_params();
                if let Some(connection) = cx
                    .global::<Self>()
                    .by_id(&id)
                    .and_then(|plugin| plugin.connection.as_ref())
                {
                    connection.notify("view/open", params);
                }
            }
        }
        cx.refresh_windows();
    }

    fn register(
        id: &str,
        name: &str,
        contributions: &Contributions,
        requester: Option<process::Requester>,
        cx: &mut App,
    ) -> Vec<Added> {
        let mut added = Vec::new();
        for panel in &contributions.panels {
            let key = intern(format!("{id}/{}", panel.id));
            let title = intern(panel.title.clone());
            let prefixes = panel.matches.clone();
            let plugin = id.to_owned();
            let contribution = panel.id.clone();
            channel_panels::register(
                ChannelPanel {
                    key,
                    order: panel.order,
                    title,
                    icon: icon(panel.icon.as_deref()),
                    matches: channel_panels::matches(move |target| {
                        prefixes
                            .iter()
                            .any(|prefix| target.starts_with(prefix.as_str()))
                    }),
                    launcher: panel.launcher.as_ref().map(|launcher| Launcher {
                        icon: icon(panel.icon.as_deref()),
                        label: launcher.label.clone().map_or(title, intern),
                        placement: match launcher.placement {
                            LauncherPlacement::Side => Placement::Side,
                            LauncherPlacement::Bottom => Placement::Bottom,
                        },
                        target: launcher.target.clone().map(intern),
                    }),
                    menu: panel.menu.as_ref().map(|menu| {
                        let (kinds, private) = (menu.kinds.clone(), menu.private);
                        MenuEntry {
                            label: intern(menu.label.clone()),
                            eligible: std::rc::Rc::new(move |channel, _| {
                                let kind = match channel.channel_type {
                                    Some(ChannelType::Dm) => "dm",
                                    Some(ChannelType::Forum) => "forum",
                                    Some(ChannelType::Session) => "session",
                                    Some(ChannelType::Stream) | None => "stream",
                                };
                                (kinds.is_empty() || kinds.iter().any(|wanted| wanted == kind))
                                    && private.is_none_or(|private| private == channel.private)
                            }),
                        }
                    }),
                    app_launcher: panel.app_launcher.as_ref().map(|launcher| AppLauncher {
                        icon: icon(panel.icon.as_deref()),
                        label: launcher.label.clone().map_or(title, intern),
                        target: intern(launcher.target.clone()),
                    }),
                    build: channel_panels::build(move |opening, window, cx| {
                        ExternalView::panel(&plugin, &contribution, opening, window, cx)
                    }),
                },
                cx,
            );
            added.push(Added::Panel(key));
        }
        for page in &contributions.pages {
            let key = Page::new(intern(format!("{id}/{}", page.id)));
            let plugin = id.to_owned();
            let contribution = page.id.clone();
            let edge_to_edge = page.layout == protocol::PageLayout::Workspace;
            // buzz-app's page icons are `data:image/` URLs; names of
            // Buzz's icons work too.
            let image = page
                .icon
                .as_deref()
                .filter(|icon| icon.starts_with("data:") && icon.len() <= 64 * 1024)
                .and_then(crate::ui::media::inline_image);
            if let Some(image) = image {
                pages::register_image(key, image, cx);
            }
            let mut spec = PageSpec::sidebar(
                key,
                intern(page.title.clone()),
                icon(page.icon.as_deref()),
                move |window, cx| {
                    ExternalView::page(&plugin, &contribution, edge_to_edge, window, cx)
                },
            );
            spec.placement = match page.placement {
                PagePlacement::Sidebar => pages::Placement::Sidebar,
                PagePlacement::Topbar | PagePlacement::Toolbar => pages::Placement::Topbar,
                PagePlacement::Hidden => pages::Placement::Hidden,
            };
            spec.collapsible = matches!(
                page.placement,
                PagePlacement::Topbar | PagePlacement::Toolbar
            );
            if page.routes {
                pages::register_routes(
                    key,
                    pages::PageRoutes {
                        version: page.route_version,
                        validate: view::route_valid,
                        open: view::open_route,
                    },
                    cx,
                );
            }
            spec.badge = Some(contribute::badge(key.key()));
            pages::register(spec, cx);
            added.push(Added::Page(key));
        }
        added.extend(
            contribute::register(id, name, contributions, requester, cx)
                .into_iter()
                .map(Added::Other),
        );
        added
    }

    /// The connection of plugin `id` while it runs.
    fn running_connection<'a>(id: &str, cx: &'a App) -> Option<&'a Connection> {
        cx.try_global::<Self>()?
            .by_id(id)
            .filter(|plugin| plugin.status == Status::Running)?
            .connection
            .as_ref()
    }

    /// `service/publish`: hands an event of a service `from` offers to
    /// every running plugin that subscribed to it, as `service/event`.
    fn publish_service(from: &str, params: &Value, cx: &App) {
        let Some(service) = params.get("service").and_then(Value::as_str) else {
            return;
        };
        let state = cx.global::<Self>();
        let offered = state
            .by_id(from)
            .is_some_and(|plugin| plugin.services.iter().any(|id| id == service));
        if !offered {
            return;
        }
        let key = (from.to_owned(), service.to_owned());
        let event = json!({
            "plugin": from,
            "service": service,
            "event": params.get("event").cloned().unwrap_or(Value::Null),
            "data": params.get("data").cloned().unwrap_or(Value::Null),
        });
        for plugin in &state.plugins {
            if plugin.status == Status::Running
                && plugin.subscriptions.contains(&key)
                && let Some(connection) = &plugin.connection
            {
                connection.notify("service/event", event.clone());
            }
        }
    }

    /// `services/request`: asks the running plugin that offers `service`
    /// to answer `method`, as `service/request` with who asks.
    fn service_request(
        from: Option<String>,
        params: &Value,
        cx: &App,
    ) -> Result<Task<Result<Value, capabilities::Failure>>, capabilities::Failure> {
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .ok_or((INVALID_PARAMS, format!("{key} is required")))
        };
        let (target, service, method) = (text("plugin")?, text("service")?, text("method")?);
        let offered = cx.global::<Self>().by_id(target).is_some_and(|plugin| {
            plugin.status == Status::Running && plugin.services.iter().any(|id| id == service)
        });
        let connection = Self::running_connection(target, cx)
            .filter(|_| offered)
            .ok_or((
                3,
                format!("{target} does not offer the service {service} now."),
            ))?;
        let request = connection.requester().request(
            "service/request",
            json!({
                "from": from,
                "service": service,
                "method": method,
                "params": params.get("params").cloned().unwrap_or(Value::Null),
            }),
            std::time::Duration::from_secs(30),
        );
        Ok(cx.background_spawn(async move {
            match request.await {
                Ok(Ok(answer)) => Ok(answer),
                Ok(Err(error)) => Err((3, error)),
                Err(error) => Err((3, error.to_string())),
            }
        }))
    }

    /// The services running plugins offer, as `services/list` answers.
    fn service_list(&self) -> Value {
        Value::Array(
            self.plugins
                .iter()
                .filter(|plugin| plugin.status == Status::Running)
                .flat_map(|plugin| {
                    let id = plugin.id().unwrap_or_default().to_owned();
                    plugin
                        .services
                        .iter()
                        .map(move |service| json!({ "plugin": id, "service": service }))
                })
                .collect(),
        )
    }

    /// Tells running plugins the services on offer changed, as a Cordis
    /// `inject` learns a service came or went.
    fn services_changed(cx: &App) {
        let state = cx.global::<Self>();
        let params = json!({ "services": state.service_list() });
        for plugin in &state.plugins {
            if plugin.status == Status::Running
                && let Some(connection) = &plugin.connection
            {
                connection.notify("services/changed", params.clone());
            }
        }
    }

    /// Sends a notification to plugin `id` if it runs.
    pub(crate) fn notify_running(id: &str, method: &str, params: Value, cx: &App) {
        if let Some(connection) = Self::running_connection(id, cx) {
            connection.notify(method, params);
        }
    }

    fn event(path: &Path, generation: u64, event: Event, cx: &mut App) {
        let state = cx.global_mut::<Self>();
        let Some(plugin) = state.current(path, generation) else {
            return;
        };
        let name = plugin.name();
        match event {
            Event::Exited(reason) => {
                tracing::warn!(plugin = %name, %reason, "plugin stopped");
                toasts::show(
                    Notice::error(exit_notice(path), format!("{name} stopped"))
                        .description(reason.lines().next().unwrap_or_default().to_owned()),
                    cx,
                );
                Self::stop(path, Status::Failed(reason), cx);
            }
            Event::Notification { method, params } => match method.as_str() {
                "view/render" => {
                    let view = params
                        .get("view")
                        .and_then(Value::as_str)
                        .and_then(|id| plugin.views.get(id))
                        .and_then(WeakEntity::upgrade);
                    let Some(view) = view else {
                        return;
                    };
                    let tree = params
                        .get("tree")
                        .cloned()
                        .ok_or_else(|| "view/render has no tree".to_owned())
                        .and_then(protocol::Node::parse);
                    view.update(cx, |view, cx| view.set_tree(tree, cx));
                }
                "message/markers" => {
                    if let Some(id) = plugin.id().map(str::to_owned) {
                        contribute::set_marks(&id, &params, cx);
                    }
                }
                "page/badge" => {
                    if let Some(id) = plugin.id().map(str::to_owned) {
                        contribute::set_badge(&id, &params, cx);
                    }
                }
                "shortcut/enable" => {
                    if let (Some(id), Some(shortcut)) = (
                        plugin.id().map(str::to_owned),
                        params.get("shortcut").and_then(Value::as_str),
                    ) {
                        let enabled = params
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(true);
                        crate::shortcuts::set_external_enabled(
                            &format!("{id}/{shortcut}"),
                            enabled,
                        );
                    }
                }
                "agentType/readiness" => {
                    if let Some(id) = plugin.id().map(str::to_owned) {
                        agents::set_readiness(&id, &params, cx);
                    }
                }
                "service/publish" => {
                    if let Some(id) = plugin.id().map(str::to_owned) {
                        Self::publish_service(&id, &params, cx);
                    }
                }
                "settings/visibility" => {
                    if let Some(id) = plugin.id().map(str::to_owned) {
                        contribute::set_card_visibility(&id, &params, cx);
                    }
                }
                _ => tracing::debug!(plugin = %name, %method, "unknown plugin notification"),
            },
            Event::Request { id, method, params } => {
                let reply = capabilities::in_community(&params, cx, |cx| {
                    Self::answer(path, generation, &method, &params, cx)
                })
                .unwrap_or_else(|failure| Reply::Now(Err(failure)));
                match reply {
                    Reply::Now(result) => Self::respond(path, generation, &id, result, cx),
                    Reply::Later(task) => {
                        let path = path.to_path_buf();
                        cx.spawn(async move |cx| {
                            let result = task.await;
                            cx.update(|cx| Self::respond(&path, generation, &id, result, cx));
                        })
                        .detach();
                    }
                }
            }
        }
    }

    fn respond(
        path: &Path,
        generation: u64,
        id: &Value,
        result: Result<Value, capabilities::Failure>,
        cx: &mut App,
    ) {
        if let Some(connection) = cx
            .global_mut::<Self>()
            .current(path, generation)
            .and_then(|plugin| plugin.connection.as_ref())
        {
            connection.respond(id, result);
        }
    }

    /// Sends a notification to the plugin's current run.
    fn notify(path: &Path, generation: u64, method: &str, params: Value, cx: &mut App) {
        if let Some(connection) = cx
            .global_mut::<Self>()
            .current(path, generation)
            .and_then(|plugin| plugin.connection.as_ref())
        {
            connection.notify(method, params);
        }
    }

    /// `channel/watch`, `thread/watch` and `events/watch`: follows a
    /// channel, a thread or live events for this run of the plugin, telling
    /// it with `method`.
    fn watch(
        path: &Path,
        generation: u64,
        method: &'static str,
        params: &Value,
        cx: &mut App,
    ) -> Reply {
        let Some(plugin) = cx.global_mut::<Self>().current(path, generation) else {
            return Reply::Now(Err((1, "The plugin stopped.".to_owned())));
        };
        if plugin.watches.len() >= MAX_WATCHES {
            return Reply::Now(Err((
                1,
                format!("A plugin can hold at most {MAX_WATCHES} watches; unwatch one first."),
            )));
        }
        plugin.next_watch += 1;
        let id = plugin.next_watch;
        let notify_path = path.to_path_buf();
        let changed = match method {
            "thread/watch" => "thread/changed",
            "events/watch" => "events/live",
            "unread/watch" => "unread/changed",
            "reminders/watch" => "reminders/changed",
            "outbox/watch" => "outbox/changed",
            "navigation/watch" => "navigation/changed",
            "presence/watch" => "presence/changed",
            "inbox/watch" => "inbox/changed",
            "names/watch" => "names/changed",
            "activity/watch" => "activity/changed",
            "agents/watch" => "agents/changed",
            "session/watch" => "session/changed",
            _ => "channel/changed",
        };
        let notify = move |mut value: Value, cx: &mut App| {
            value["watch"] = json!(id);
            Self::notify(&notify_path, generation, changed, value, cx);
        };
        let started = match method {
            "thread/watch" => capabilities::watch_thread(params, notify, cx),
            "events/watch" => events::watch(params, notify, cx),
            "unread/watch" => reading::watch(params, notify, cx),
            "reminders/watch" => channels::watch(params, notify, cx),
            "outbox/watch" => outbox::watch(params, notify, cx),
            "navigation/watch" => capabilities::watch_navigation(params, notify, cx),
            "presence/watch" => capabilities::watch_presence(params, notify, cx),
            "inbox/watch" => reading::watch_inbox(params, notify, cx),
            "names/watch" => capabilities::watch_names(params, notify, cx),
            "activity/watch" => observed::watch(params, notify, cx),
            "agents/watch" => agents::watch(params, notify, cx),
            "session/watch" => capabilities::watch_session(params, notify, cx),
            _ => capabilities::watch(params, notify, cx),
        };
        Reply::Now(started.map(|(watch, mut value)| {
            if let Some(plugin) = cx.global_mut::<Self>().current(path, generation) {
                plugin.watches.insert(id, watch);
            }
            value["watch"] = json!(id);
            value
        }))
    }

    /// The app's side of a plugin's request.
    fn answer(path: &Path, generation: u64, method: &str, params: &Value, cx: &mut App) -> Reply {
        let later = |task: Result<Task<_>, _>| match task {
            Ok(task) => Reply::Later(task),
            Err(failure) => Reply::Now(Err(failure)),
        };
        match method {
            "channel/messages" => later(capabilities::channel_messages(params, cx)),
            "thread/messages" => later(capabilities::thread_messages(params, cx)),
            "profiles/get" => Reply::Now(capabilities::profiles(params, cx)),
            "presence/get" => Reply::Now(capabilities::presence(params, cx)),
            "names/get" => Reply::Now(capabilities::names(params, cx)),
            "statuses/get" => later(capabilities::statuses(params, cx)),
            "emoji/list" => Reply::Now(capabilities::emoji(cx)),
            "typing/get" => Reply::Now(capabilities::typing(params, cx)),
            "people/search" => later(capabilities::search_people(params, cx)),
            "ui/openConversation" => Reply::Now(capabilities::open_conversation(params, cx)),
            "composer/insert" => Reply::Now(contribute::insert(params, cx)),
            "composer/insertMentions" => Reply::Now(contribute::insert_mentions(params, cx)),
            "composer/insertResource" => Reply::Now(contribute::insert_resource(params, cx)),
            "composer/focus" => Reply::Now(contribute::focus(params, cx)),
            "composer/capture" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                Reply::Now(match plugin {
                    Some(plugin) => contribute::capture(&plugin, params, cx),
                    None => Err((1, "The plugin stopped.".to_owned())),
                })
            }
            "composer/accept" => {
                let view = params.get("view").and_then(Value::as_str).and_then(|view| {
                    cx.global_mut::<Self>()
                        .current(path, generation)?
                        .views
                        .get(view)?
                        .upgrade()
                });
                match view.and_then(|view| view.read(cx).acceptor()) {
                    Some(accept) => Reply::Now(contribute::accept(params, accept, cx)),
                    None => Reply::Now(Err((
                        INVALID_PARAMS,
                        "composer/accept takes an open capture view".to_owned(),
                    ))),
                }
            }
            "agent/tool" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                match plugin {
                    Some(plugin) => later(agents::act(&plugin, params, cx)),
                    None => Reply::Now(Err((1, "The plugin stopped.".to_owned()))),
                }
            }
            "agent/step" | "agent/acknowledged" | "agent/superseded" | "agent/ignored" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                Reply::Now(match plugin {
                    Some(plugin) => agents::presence(&plugin, method, params),
                    None => Err((1, "The plugin stopped.".to_owned())),
                })
            }
            "agents/activity" => Reply::Now(Ok(agents::activity(cx))),
            "agent/current" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                match plugin {
                    Some(plugin) => later(agents::current(&plugin, params, cx)),
                    None => Reply::Now(Err((1, "The plugin stopped.".to_owned()))),
                }
            }
            "agent/query" | "agent/publish" | "agent/upload" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                match plugin {
                    Some(plugin) => later(agents::act_raw(&plugin, method, params, cx)),
                    None => Reply::Now(Err((1, "The plugin stopped.".to_owned()))),
                }
            }
            "notifications/submit" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                Reply::Now(match plugin {
                    Some(plugin) => capabilities::notify(&plugin, params, cx),
                    None => Err((1, "The plugin stopped.".to_owned())),
                })
            }
            "reaction/select" => {
                let view = params.get("view").and_then(Value::as_str).and_then(|view| {
                    cx.global_mut::<Self>()
                        .current(path, generation)?
                        .views
                        .get(view)?
                        .upgrade()
                });
                let emoji = params
                    .get("emoji")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|emoji| !emoji.is_empty() && emoji.chars().count() <= 64)
                    .map(str::to_owned);
                let select = view.and_then(|view| view.read(cx).selector());
                Reply::Now(
                    match (select, emoji, crate::ui::navigation::main_window(cx)) {
                        (Some(select), Some(emoji), Some(window)) => window
                            .update(cx, |_, window, cx| select(emoji, window, cx))
                            .map(|()| Value::Null)
                            .map_err(|_| (1, "The window is not open.".to_owned())),
                        _ => Err((
                            INVALID_PARAMS,
                            "reaction/select takes an open reactionPicker view and an emoji"
                                .to_owned(),
                        )),
                    },
                )
            }
            "agent/createConfig" => {
                let view = params.get("view").and_then(Value::as_str).and_then(|view| {
                    cx.global_mut::<Self>()
                        .current(path, generation)?
                        .views
                        .get(view)?
                        .upgrade()
                });
                let change = view.and_then(|view| view.read(cx).changer());
                Reply::Now(match (change, params.get("config").filter(|config| config.is_object())) {
                    (Some(change), Some(config)) => {
                        change(config.clone(), cx);
                        Ok(Value::Null)
                    }
                    _ => Err((
                        INVALID_PARAMS,
                        "agent/createConfig takes an open agentCreate view and an object config"
                            .to_owned(),
                    )),
                })
            }
            "ui/openPanel" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                Reply::Now(match plugin {
                    Some(plugin) => capabilities::open_panel(&plugin, params, cx),
                    None => Err((1, "The plugin stopped.".to_owned())),
                })
            }
            "panel/canOpen" => Reply::Now(
                params
                    .get("target")
                    .and_then(Value::as_str)
                    .map(|target| json!(channel_panels::resolve(target, cx).is_some()))
                    .ok_or_else(|| (INVALID_PARAMS, "target is required".to_owned())),
            ),
            "panel/resolve" => Reply::Now(
                params
                    .get("target")
                    .and_then(Value::as_str)
                    .map(|target| match channel_panels::resolve(target, cx) {
                        Some(panel) => json!({
                            "key": panel.key,
                            "title": panel.title,
                            "order": panel.order,
                        }),
                        None => Value::Null,
                    })
                    .ok_or_else(|| (INVALID_PARAMS, "target is required".to_owned())),
            ),
            "panel/open" | "panel/push" => {
                let view = params.get("view").and_then(Value::as_str).and_then(|view| {
                    cx.global_mut::<Self>()
                        .current(path, generation)?
                        .views
                        .get(view)?
                        .upgrade()
                });
                let target = params
                    .get("target")
                    .and_then(Value::as_str)
                    .filter(|target| target.len() <= 2048);
                Reply::Now(match (view, target) {
                    (Some(view), Some(target)) => Ok(json!(ExternalView::open_target(
                        &view,
                        target,
                        method == "panel/push",
                        cx
                    ))),
                    _ => Err((
                        INVALID_PARAMS,
                        format!("{method} takes an open panel view and a target"),
                    )),
                })
            }
            "ui/openDialog" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                Reply::Now(match plugin {
                    Some(plugin) => dialogs::open(&plugin, params, cx),
                    None => Err((1, "The plugin stopped.".to_owned())),
                })
            }
            "composer/settle" => {
                let view = params.get("view").and_then(Value::as_str).and_then(|view| {
                    cx.global_mut::<Self>()
                        .current(path, generation)?
                        .views
                        .get(view)?
                        .upgrade()
                });
                let element = params
                    .get("element")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let sent = params.get("sent").and_then(Value::as_bool).unwrap_or(false);
                let settled = match (view, element, crate::ui::navigation::main_window(cx)) {
                    (Some(view), Some(element), Some(window)) => window
                        .update(cx, |_, window, cx| {
                            view.update(cx, |view, cx| view.settle(&element, sent, window, cx))
                        })
                        .unwrap_or(false),
                    _ => false,
                };
                Reply::Now(if settled {
                    Ok(Value::Null)
                } else {
                    Err((
                        INVALID_PARAMS,
                        "composer/settle takes a view and the element of a draft it took"
                            .to_owned(),
                    ))
                })
            }
            "ui/closeView" => {
                let view = params.get("view").and_then(Value::as_str).and_then(|view| {
                    cx.global_mut::<Self>()
                        .current(path, generation)?
                        .views
                        .get(view)?
                        .upgrade()
                });
                let close = view.and_then(|view| view.read(cx).closer());
                let closed = match (close, crate::ui::navigation::main_window(cx)) {
                    (Some(close), Some(window)) => {
                        window.update(cx, |_, window, cx| close(window, cx)).is_ok()
                    }
                    _ => false,
                };
                Reply::Now(if closed {
                    Ok(Value::Null)
                } else {
                    Err((
                        INVALID_PARAMS,
                        "ui/closeView takes an open dialog".to_owned(),
                    ))
                })
            }
            "agent/save" => {
                let view = params.get("view").and_then(Value::as_str).and_then(|view| {
                    cx.global_mut::<Self>()
                        .current(path, generation)?
                        .views
                        .get(view)?
                        .upgrade()
                });
                let config = params.get("config").filter(|config| config.is_object());
                let (Some(view), Some(config)) = (view, config) else {
                    return Reply::Now(Err((
                        INVALID_PARAMS,
                        "agent/save takes an open agentTab view and an object config".to_owned(),
                    )));
                };
                let config = config.clone();
                match view.update(cx, |view, cx| view.save_agent(config, cx)) {
                    Some(task) => Reply::Later(cx.background_spawn(async move {
                        task.await.map(|()| Value::Null).map_err(|error| (3, error))
                    })),
                    None => Reply::Now(Err((
                        INVALID_PARAMS,
                        "agent/save takes an agentTab view".to_owned(),
                    ))),
                }
            }
            "channel/watch" => Self::watch(path, generation, "channel/watch", params, cx),
            "thread/watch" => Self::watch(path, generation, "thread/watch", params, cx),
            "events/watch" => Self::watch(path, generation, "events/watch", params, cx),
            "unread/watch" => Self::watch(path, generation, "unread/watch", params, cx),
            "session/watch" => Self::watch(path, generation, "session/watch", params, cx),
            "unread/get" => Reply::Now(reading::get(cx)),
            "unread/sync" => Reply::Now(reading::sync(false, cx)),
            "unread/retrySync" => Reply::Now(reading::sync(true, cx)),
            "unread/message" => later(reading::message(params, cx)),
            "unread/attention" => later(reading::attention(params, cx)),
            "threads/follow" => later(channels::follow(params, cx)),
            "reminders/list" => Reply::Now(channels::list(cx)),
            "reminders/watch" => Self::watch(path, generation, "reminders/watch", params, cx),
            "outbox/watch" => Self::watch(path, generation, "outbox/watch", params, cx),
            "navigation/watch" => Self::watch(path, generation, "navigation/watch", params, cx),
            "presence/watch" => Self::watch(path, generation, "presence/watch", params, cx),
            "inbox/watch" => Self::watch(path, generation, "inbox/watch", params, cx),
            "names/watch" => Self::watch(path, generation, "names/watch", params, cx),
            "activity/watch" => Self::watch(path, generation, "activity/watch", params, cx),
            "activity/observed" => Reply::Now(observed::get(params, cx)),
            "activity/history" => Reply::Now(observed::history(params, cx)),
            "media/get" => later(capabilities::media(params, cx)),
            "media/url" => Reply::Now(capabilities::media_url(params, cx)),
            "community/clearCache" => later(capabilities::community(cx).map(|community| {
                let session = community.read(cx).session.clone();
                capabilities::on_tokio(cx, async move {
                    session.clear_cache().await;
                    Ok(Value::Null)
                })
            })),
            "sidebar/get" => Reply::Now(channels::sidebar(cx)),
            "projects/gitAuthorization" => Reply::Now(capabilities::git_authorization(params, cx)),
            "agents/logProof" => Reply::Now(capabilities::log_proof(params, cx)),
            "navigation/get" => Reply::Now(Ok(capabilities::navigation_state(cx))),
            "navigation/retry" => Reply::Now(capabilities::retry_navigation(cx)),
            "outbox/get" => later(outbox::get(params, cx)),
            "archives/get" => later(channels::archives(params, cx)),
            "read/clearLocal" => later(reading::local(true, params, cx)),
            "read/markLocal" => later(reading::local(false, params, cx)),
            "sessions/memberOf" => later(channels::member_of(params, cx)),
            "reminders/create" | "reminders/snooze" | "reminders/complete" | "reminders/cancel" => {
                later(channels::change(method, params, cx))
            }
            "inbox/get" => Reply::Now(reading::inbox(cx)),
            "threads/activity" => later(reading::threads(params, cx)),
            "read/mark" => later(reading::mark(params, cx)),
            "channel/unwatch" | "thread/unwatch" | "events/unwatch" | "unread/unwatch"
            | "reminders/unwatch" | "agents/unwatch" | "session/unwatch" | "outbox/unwatch"
            | "navigation/unwatch" | "presence/unwatch" | "activity/unwatch" | "names/unwatch"
            | "inbox/unwatch" => {
                let id = params.get("watch").and_then(Value::as_u64);
                if let (Some(id), Some(plugin)) =
                    (id, cx.global_mut::<Self>().current(path, generation))
                {
                    plugin.watches.remove(&id);
                }
                Reply::Now(Ok(Value::Null))
            }
            "messages/send"
            | "messages/edit"
            | "messages/react"
            | "messages/delete"
            | "files/upload"
            | "events/publish"
            | "agents/create"
            | "agents/save"
            | "agents/remove"
            | "dm/open"
            | "channels/create"
            | "channels/action"
            | "teams/save"
            | "teams/remove"
            | "agents/retype"
            | "agents/port"
            | "agents/attest"
            | "agents/presence"
            | "channels/addMember"
            | "channels/changeMember"
            | "messages/report"
            | "messages/retry"
            | "messages/dismiss"
            | "canvas/save"
            | "channels/saveDetails"
            | "kit/save"
            | "workflows/save"
            | "workflows/delete"
            | "workflows/trigger"
            | "workflows/dismiss"
            | "sessions/create"
            | "sidebar/star"
            | "sidebar/mute"
            | "sidebar/sort"
            | "sidebar/assign"
            | "sidebar/removeGroup"
            | "archives/request" => {
                let permission = match method {
                    "events/publish" => Permission::PublishEvents,
                    "agents/create" | "agents/save" | "agents/remove" | "teams/save"
                    | "teams/remove" | "agents/retype" | "agents/port" | "agents/attest"
                    | "agents/presence" => Permission::ManageAgents,
                    "dm/open"
                    | "channels/create"
                    | "channels/action"
                    | "channels/addMember"
                    | "channels/changeMember"
                    | "channels/saveDetails"
                    | "kit/save"
                    | "workflows/save"
                    | "workflows/delete"
                    | "workflows/trigger"
                    | "workflows/dismiss"
                    | "sessions/create"
                    | "sidebar/star"
                    | "sidebar/mute"
                    | "sidebar/sort"
                    | "sidebar/assign"
                    | "sidebar/removeGroup"
                    | "archives/request" => Permission::ManageChannels,
                    _ => Permission::SendMessages,
                };
                let allowed = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .is_some_and(|plugin| plugin.may(permission));
                if !allowed {
                    return Reply::Now(Err((
                        FORBIDDEN,
                        format!(
                            "Declare the {} permission in buzz-plugin.json.",
                            serde_json::to_value(permission)
                                .ok()
                                .and_then(|name| name.as_str().map(str::to_owned))
                                .unwrap_or_default()
                        ),
                    )));
                }
                if method == "agents/attest" {
                    return Reply::Now(capabilities::attest(params, cx));
                }
                let plugin_id = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned))
                    .unwrap_or_default();
                if method.starts_with("teams/") {
                    return Reply::Now(agents::team(method, params, cx));
                }
                later(match method {
                    "messages/edit" => capabilities::edit(params, cx),
                    "messages/react" => capabilities::react(params, cx),
                    "messages/delete" => capabilities::delete(params, cx),
                    "messages/report" => capabilities::report(params, cx),
                    "messages/retry" => capabilities::retry(false, params, cx),
                    "messages/dismiss" => capabilities::retry(true, params, cx),
                    "files/upload" => capabilities::upload(&plugin_id, params, cx),
                    "events/publish" => events::publish(params, cx),
                    "agents/create" => agents::create(params, cx),
                    "agents/save" => agents::save(params, cx),
                    "agents/remove" => agents::remove(params, cx),
                    "agents/retype" => agents::retype(params, cx),
                    "agents/port" => agents::port(params, cx),
                    "agents/presence" => capabilities::agent_presence(params, cx),
                    "dm/open" => channels::open_dm(params, cx),
                    "channels/create" => channels::create(params, cx),
                    "channels/action" => channels::action(params, cx),
                    "channels/addMember" => channels::add_member(params, cx),
                    "channels/changeMember" => channels::change_member(params, cx),
                    "canvas/save" => content::canvas_save(params, cx),
                    "channels/saveDetails" => content::save_details(params, cx),
                    "kit/save" => content::save_kit(params, cx),
                    "workflows/dismiss" => workflows::dismiss(params, cx),
                    "sessions/create" => channels::create_session(params, cx),
                    "archives/request" => channels::request_archive(params, cx),
                    "sidebar/star"
                    | "sidebar/mute"
                    | "sidebar/sort"
                    | "sidebar/assign"
                    | "sidebar/removeGroup" => channels::change_sidebar(method, params, cx),
                    "workflows/save" | "workflows/delete" | "workflows/trigger" => {
                        workflows::command(method, params, cx)
                    }
                    _ => capabilities::send(params, cx),
                })
            }
            "agents/memories" => later(content::memories(params, cx)),
            "canvas/read" => later(content::canvas_read(params, cx)),
            "canvas/history" => later(content::canvas_history(params, cx)),
            "channels/details" => later(content::details(params, cx)),
            "kit/get" => Reply::Now(content::kit(cx)),
            "projects/list" => later(content::projects(cx)),
            "community/info" => later(capabilities::info(cx)),
            "contributions/list" => Reply::Now(Ok(capabilities::contributions(cx))),
            "workflows/list" => later(workflows::list(params, cx)),
            "workflows/runs" => later(workflows::runs(params, cx)),
            "workflows/operations" => Reply::Now(workflows::operations(cx)),
            "workflows/takeSecret" => Reply::Now(workflows::take_secret(params, cx)),
            "projects/load" => later(content::project(params, cx)),
            "projects/home" => later(content::project_home(params, cx)),
            "projects/git" => later(content::git(params, cx)),
            "events/query" => later(events::query(params, cx)),
            "agents/list" => Reply::Now(agents::list(cx)),
            "services/list" => Reply::Now(Ok(cx.global::<Self>().service_list())),
            "services/subscribe" | "services/unsubscribe" => {
                let target = |key: &str| params.get(key).and_then(Value::as_str).map(str::to_owned);
                let (Some(target), Some(service)) = (target("plugin"), target("service")) else {
                    return Reply::Now(Err((
                        INVALID_PARAMS,
                        "plugin and service are required".to_owned(),
                    )));
                };
                if let Some(plugin) = cx.global_mut::<Self>().current(path, generation) {
                    if method == "services/subscribe" {
                        plugin.subscriptions.insert((target, service));
                    } else {
                        plugin.subscriptions.remove(&(target, service));
                    }
                }
                Reply::Now(Ok(Value::Null))
            }
            "services/request" => {
                let from = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                later(Self::service_request(from, params, cx))
            }
            "agents/previewRetype" => Reply::Now(agents::preview_retype(params, cx)),
            "format/time" => Reply::Now(capabilities::format_time(params)),
            "agent/open" | "agent/close" => {
                let plugin = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .and_then(|plugin| plugin.id().map(str::to_owned));
                Reply::Now(match plugin {
                    Some(plugin) if method == "agent/open" => {
                        agents::open_agent(&plugin, params, cx)
                    }
                    Some(plugin) => agents::close_agent(&plugin, params),
                    None => Err((1, "The plugin stopped.".to_owned())),
                })
            }
            "attachment/remove" => Reply::Now(match params.get("url").and_then(Value::as_str) {
                Some(url) => Ok(json!(crate::ui::renderers::remove(url, cx))),
                None => Err((INVALID_PARAMS, "url is required".to_owned())),
            }),
            "agents/dismissError" => Reply::Now(agents::dismiss_error(params, cx)),
            "agents/clearActivity" => Reply::Now(agents::clear_activity(params, cx)),
            "agents/portable" => later(agents::portable(cx)),
            "agents/watch" => Self::watch(path, generation, "agents/watch", params, cx),
            "ui/navigate" => Reply::Now(capabilities::navigate(params, cx)),
            "ui/openLink" => Reply::Now(capabilities::open_link(params, cx)),
            _ => Reply::Now(Self::answer_now(method, params, cx)),
        }
    }

    fn answer_now(method: &str, params: &Value, cx: &mut App) -> Result<Value, (i64, String)> {
        let text = |key: &str| params.get(key).and_then(Value::as_str);
        match method {
            "ui/toast" => {
                let title = text("title")
                    .filter(|title| !title.trim().is_empty())
                    .ok_or((INVALID_PARAMS, "ui/toast needs a title".to_owned()))?;
                let tone = match text("tone") {
                    Some("success") => ToastTone::Success,
                    Some("warning") => ToastTone::Warning,
                    Some("error") => ToastTone::Error,
                    _ => ToastTone::Info,
                };
                let mut notice = Notice::error("plugin-toast", bounded(title, 128));
                notice.tone = tone;
                if tone != ToastTone::Error {
                    notice.timeout = Some(Duration::from_secs(6));
                }
                if let Some(body) = text("body") {
                    notice = notice.description(bounded(body, 200));
                }
                toasts::show(notice, cx);
                Ok(Value::Null)
            }
            "ui/openUrl" => {
                let url = text("url")
                    .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                    .ok_or((INVALID_PARAMS, "ui/openUrl takes an http(s) url".to_owned()))?;
                cx.open_url(url);
                Ok(Value::Null)
            }
            "community/current" => Ok(Self::current_community(cx)),
            "community/list" => Ok(Self::community_list(cx)),
            "community/client" => Ok(Self::client(cx)),
            "community/retry" => {
                if let Ok(community) = capabilities::community(cx) {
                    community.read(cx).session.retry();
                }
                Ok(Value::Null)
            }
            "community/channels" => Ok(capabilities::channels(cx)),
            _ => Err((METHOD_NOT_FOUND, format!("Buzz has no method {method}"))),
        }
    }

    pub(crate) fn next_view_id(cx: &mut App) -> String {
        let state = cx.global_mut::<Self>();
        state.next_view += 1;
        format!("v{}", state.next_view)
    }

    /// A view showed: the plugin draws it once it runs.
    pub(crate) fn open_view(
        plugin: &str,
        view: &str,
        entity: WeakEntity<ExternalView>,
        params: Value,
        cx: &mut App,
    ) {
        let Some(plugin) = cx.global_mut::<Self>().by_id_mut(plugin) else {
            return;
        };
        plugin.views.insert(view.to_owned(), entity);
        if plugin.status == Status::Running
            && let Some(connection) = &plugin.connection
        {
            connection.notify("view/open", params);
        }
    }

    pub(crate) fn close_view(plugin: &str, view: &str, cx: &mut App) {
        if !cx.has_global::<Self>() {
            return;
        }
        let Some(plugin) = cx.global_mut::<Self>().by_id_mut(plugin) else {
            return;
        };
        if plugin.views.remove(view).is_some()
            && let Some(connection) = &plugin.connection
        {
            connection.notify("view/close", json!({ "view": view }));
        }
    }

    pub(crate) fn send_event(plugin: &str, params: Value, cx: &App) {
        if let Some(connection) = cx
            .global::<Self>()
            .by_id(plugin)
            .filter(|plugin| plugin.status == Status::Running)
            .and_then(|plugin| plugin.connection.as_ref())
        {
            connection.notify("view/event", params);
        }
    }
}

/// The toast for a plugin that stopped by itself.
fn exit_notice(path: &Path) -> SharedString {
    format!("plugin-exit-{}", path.display()).into()
}

fn take_away(added: Vec<Added>, cx: &mut App) {
    for added in added {
        match added {
            Added::Panel(key) => channel_panels::unregister(key, cx),
            Added::Page(page) => pages::unregister(page, cx),
            Added::Other(added) => contribute::take_away(added, cx),
            Added::Agent(added) => agents::take_away(added, cx),
        }
    }
}

/// `text` cut to `max` characters, as buzz-app bounds plugin
/// notification text.
fn bounded(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn keys_are_leaked_once() {
        let a = intern("x.y/panel".to_owned());
        let b = intern("x.y/panel".to_owned());
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn icons_fall_back_to_the_plug() {
        assert_eq!(icon(Some("house")), IconName::House);
        assert_eq!(icon(Some("nope")), IconName::Plug);
        assert_eq!(icon(None), IconName::Plug);
    }

    #[test]
    fn saving_a_file_changes_the_fingerprint_but_hidden_state_does_not() {
        let folder = std::env::temp_dir().join(format!("buzz-plugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join(".state")).unwrap();
        std::fs::write(folder.join("plugin.py"), "a").unwrap();
        let first = fingerprint(&folder);
        std::fs::write(folder.join(".state/cache"), "x").unwrap();
        std::fs::create_dir_all(folder.join("node_modules")).unwrap();
        std::fs::write(folder.join("node_modules/dep.js"), "x").unwrap();
        assert_eq!(fingerprint(&folder), first);
        std::fs::write(folder.join("plugin.py"), "ab").unwrap();
        assert_ne!(fingerprint(&folder), first);
        assert_eq!(fingerprint(&folder.join("missing")), None);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn manifests_are_read_from_the_folder() {
        let folder = std::env::temp_dir().join(format!("buzz-plugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        assert!(
            read_manifest(&folder)
                .unwrap_err()
                .contains("no buzz-plugin.json")
        );
        std::fs::write(
            folder.join(MANIFEST),
            r#"{ "id": "com.example.a", "name": "A", "command": ["./run"] }"#,
        )
        .unwrap();
        assert_eq!(read_manifest(&folder).unwrap().id, "com.example.a");
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn permissions_need_the_manifest_and_the_user() {
        let folder = std::env::temp_dir().join(format!("buzz-plugin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(MANIFEST),
            r#"{ "id": "com.example.a", "name": "A", "command": ["./run"],
                 "permissions": ["send-messages"] }"#,
        )
        .unwrap();
        let load = |approved| {
            Plugin::load(SavedPlugin {
                path: folder.clone(),
                enabled: true,
                approved,
                publisher: None,
                source: None,
            })
        };
        // Saved before the manifest asked: the user has not seen it.
        let plugin = load(Vec::new());
        assert!(!plugin.may(Permission::SendMessages));
        assert_eq!(plugin.unapproved(), [Permission::SendMessages]);
        let plugin = load(vec![Permission::SendMessages]);
        assert!(plugin.may(Permission::SendMessages));
        assert!(plugin.unapproved().is_empty());
        // Allowed once, then dropped from the manifest: no longer allowed.
        std::fs::write(
            folder.join(MANIFEST),
            r#"{ "id": "com.example.a", "name": "A", "command": ["./run"] }"#,
        )
        .unwrap();
        assert!(!load(vec![Permission::SendMessages]).may(Permission::SendMessages));
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn text_is_bounded() {
        assert_eq!(bounded("hello", 3), "hel…");
        assert_eq!(bounded("hi", 3), "hi");
    }
}
