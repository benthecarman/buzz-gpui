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

mod capabilities;
pub mod git;
mod process;
pub mod protocol;
pub mod release;
mod view;

use crate::community::AppState;
use crate::runtime::Runtime;
use crate::ui::channel_panels::{self, ChannelPanel, Launcher, Placement};
use crate::ui::pages::{self, Page, PageSpec};
use crate::ui::toasts::{self, Notice};
use buzz_relay_client::ChannelType;
use buzz_ui::{IconName, ToastTone};
use futures::StreamExt;
use gpui_kit::*;
use process::{Connection, Event};
use protocol::{
    Contributions, INVALID_PARAMS, LauncherPlacement, MANIFEST, METHOD_NOT_FOUND, Permission,
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

/// Most channels one plugin follows at once.
const MAX_WATCHES: usize = 16;
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
        plugin.generation += 1;
        plugin.status = Status::Starting;
        let generation = plugin.generation;
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
        plugin.generation += 1;
        let off = status == Status::Off;
        plugin.status = status;
        if off {
            let added = std::mem::take(&mut plugin.added);
            take_away(added, cx);
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
        let added = Self::register(&id, &contributions, cx);
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
        // Views open before this run ask the new run for their trees.
        let views: Vec<_> = plugin.views.values().cloned().collect();
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

    fn register(id: &str, contributions: &Contributions, cx: &mut App) -> Vec<Added> {
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
                    }),
                    menu: None,
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
            pages::register(
                PageSpec::sidebar(
                    key,
                    intern(page.title.clone()),
                    icon(page.icon.as_deref()),
                    move |window, cx| ExternalView::page(&plugin, &contribution, window, cx),
                ),
                cx,
            );
            added.push(Added::Page(key));
        }
        added
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
                _ => tracing::debug!(plugin = %name, %method, "unknown plugin notification"),
            },
            Event::Request { id, method, params } => {
                match Self::answer(path, generation, &method, &params, cx) {
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

    /// `channel/watch`: follows a channel for this run of the plugin.
    fn watch(path: &Path, generation: u64, params: &Value, cx: &mut App) -> Reply {
        let Some(plugin) = cx.global_mut::<Self>().current(path, generation) else {
            return Reply::Now(Err((1, "The plugin stopped.".to_owned())));
        };
        if plugin.watches.len() >= MAX_WATCHES {
            return Reply::Now(Err((
                1,
                format!("A plugin can follow at most {MAX_WATCHES} channels."),
            )));
        }
        plugin.next_watch += 1;
        let id = plugin.next_watch;
        let notify_path = path.to_path_buf();
        let watch = capabilities::watch(
            params,
            move |mut timeline, cx| {
                timeline["watch"] = json!(id);
                Self::notify(&notify_path, generation, "channel/changed", timeline, cx);
            },
            cx,
        );
        Reply::Now(watch.map(|watch| {
            let channel = watch.channel.clone();
            if let Some(plugin) = cx.global_mut::<Self>().current(path, generation) {
                plugin.watches.insert(id, watch);
            }
            json!({ "watch": id, "channel": channel })
        }))
    }

    /// The app's side of a plugin's request.
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
            "ui/openConversation" => Reply::Now(capabilities::open_conversation(params, cx)),
            "channel/watch" => Self::watch(path, generation, params, cx),
            "channel/unwatch" => {
                let id = params.get("watch").and_then(Value::as_u64);
                if let (Some(id), Some(plugin)) =
                    (id, cx.global_mut::<Self>().current(path, generation))
                {
                    plugin.watches.remove(&id);
                }
                Reply::Now(Ok(Value::Null))
            }
            "messages/send" => {
                let allowed = cx
                    .global_mut::<Self>()
                    .current(path, generation)
                    .is_some_and(|plugin| plugin.may(Permission::SendMessages));
                if allowed {
                    later(capabilities::send(params, cx))
                } else {
                    Reply::Now(Err((
                        FORBIDDEN,
                        "Declare the send-messages permission in buzz-plugin.json.".to_owned(),
                    )))
                }
            }
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
            "community/current" => Ok(AppState::current(cx).map_or(Value::Null, |community| {
                let community = community.read(cx);
                json!({ "name": community.name(), "relay": community.origin.websocket() })
            })),
            "community/channels" => {
                let Some(community) = AppState::current(cx) else {
                    return Ok(json!([]));
                };
                let channels: Vec<Value> = community
                    .read(cx)
                    .snapshot
                    .channels
                    .iter()
                    .map(|channel| {
                        json!({
                            "id": channel.id,
                            "name": channel.name,
                            "description": channel.description,
                            "private": channel.private,
                            "archived": channel.archived,
                            "kind": match channel.channel_type {
                                Some(ChannelType::Dm) => "dm",
                                Some(ChannelType::Forum) => "forum",
                                Some(ChannelType::Session) => "session",
                                Some(ChannelType::Stream) | None => "stream",
                            },
                        })
                    })
                    .collect();
                Ok(Value::Array(channels))
            }
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
