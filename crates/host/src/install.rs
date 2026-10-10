//! Installs Pi and Claude Code for this host: a pinned Node.js and the
//! harness's npm packages in a prefix the host owns, never user-global.
//!
//! Ported from buzz-app's `src-tauri/src/managed_npm.rs` and
//! `src-tauri/src/harness_setup.rs`. buzz-app's app-data directory is this
//! host's state directory, so the controller finds the installed tools
//! (and their Node.js) the same way.

use crate::api::InstallReport;
use buzz_agent_controller::ProcessStatus;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Checksums are pinned to the official Node v24.18.0 tarballs.
const VERSION: &str = "v24.18.0";
const MAX_ARCHIVE: u64 = 90 * 1024 * 1024;
// Native steering needs Pi's steer disposition, added in 0.99.0.
const PI: &str = "@earendil-works/pi-coding-agent@>=0.99.0";
const ADAPTER: &str = "git+https://github.com/salman1993/buzz-pi-acp.git#72015de";
// Both packages are pinned to versions available through the configured npm registry.
const CLAUDE: &str = "@anthropic-ai/claude-code@2.1.289";
const CLAUDE_ADAPTER: &str = "@agentclientprotocol/claude-agent-acp@0.85.1";

/// The harnesses this host can install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pi,
    Claude,
}

impl Tool {
    fn prefix(self) -> &'static str {
        match self {
            Self::Pi => "node-tools",
            Self::Claude => "claude-tools",
        }
    }

    fn revision(self) -> &'static str {
        match self {
            Self::Pi => adapter_rev(),
            Self::Claude => "claude-2.1.289-acp-0.85.1",
        }
    }

    /// The CLI, then its ACP adapter.
    pub fn binaries(self) -> [&'static str; 2] {
        match self {
            Self::Pi => ["pi", "buzz-pi-acp"],
            Self::Claude => ["claude", "claude-agent-acp"],
        }
    }

    fn packages(self) -> [(&'static str, bool, &'static str); 2] {
        match self {
            Self::Pi => [
                (PI, false, "Installing Pi failed"),
                (ADAPTER, true, "Installing the Pi ACP adapter failed"),
            ],
            Self::Claude => [
                (CLAUDE, false, "Installing Claude Code failed"),
                (
                    CLAUDE_ADAPTER,
                    false,
                    "Installing the Claude Code ACP adapter failed",
                ),
            ],
        }
    }

    fn log(self) -> &'static str {
        match self {
            Self::Pi => "agent-controller/pi-install.log",
            Self::Claude => "agent-controller/claude-install.log",
        }
    }
}

/// Whether this platform has a pinned Node.js to install with.
pub fn supported() -> bool {
    cfg!(all(
        any(target_os = "macos", target_os = "linux"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
}

/// Host-lifetime install owner: one install at a time, and the running
/// installer's process group so shutdown can stop it.
#[derive(Default)]
pub struct Setup(AtomicBool, Mutex<InstallProcess>);

#[derive(Default)]
struct InstallProcess {
    shutting_down: bool,
    group: Option<u32>,
}

pub struct InstallGuard<'a>(&'a Setup);

impl Drop for InstallGuard<'_> {
    fn drop(&mut self) {
        self.0.0.store(false, Ordering::Release);
    }
}

impl Setup {
    pub fn claim(&self) -> Result<InstallGuard<'_>, String> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "A Harness installation is already in progress".to_owned())?;
        let guard = InstallGuard(self);
        if self
            .1
            .lock()
            .map_err(|_| "Harness install state is unavailable")?
            .shutting_down
        {
            return Err("Buzz is quitting".into());
        }
        Ok(guard)
    }

    /// Shutdown either prevents spawning or observes the registered
    /// process group.
    fn spawn(
        &self,
        spawn: impl FnOnce() -> std::io::Result<tokio::process::Child>,
    ) -> Result<InstallerChild<'_>, String> {
        let mut state = self
            .1
            .lock()
            .map_err(|_| "Harness install state is unavailable")?;
        if state.shutting_down {
            return Err("Buzz is quitting".into());
        }
        let child = spawn().map_err(|_| "Could not start the Harness installer".to_owned())?;
        let group = child.id();
        state.group = Some(group.ok_or("Could not start the Harness installer")?);
        Ok(InstallerChild {
            child,
            group,
            setup: self,
            reaped: false,
        })
    }

    fn untrack(&self, group: u32, kill: bool) {
        if let Ok(mut state) = self.1.lock()
            && state.group == Some(group)
        {
            // Clearing and signalling are serialized with shutdown, so the
            // same (possibly reused) group is never signalled twice.
            if kill {
                kill_group(group);
            }
            state.group = None;
        }
    }

    /// Stops the active installer and fences later spawns.
    pub fn shutdown(&self) {
        if let Ok(mut state) = self.1.lock() {
            state.shutting_down = true;
            if let Some(group) = state.group.take() {
                kill_group(group);
            }
        }
    }
}

fn kill_group(group: u32) {
    #[cfg(unix)]
    // SAFETY: signals the process group this module started.
    unsafe {
        libc::kill(-(group as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = group;
}

struct InstallerChild<'a> {
    child: tokio::process::Child,
    group: Option<u32>,
    setup: &'a Setup,
    reaped: bool,
}

impl Drop for InstallerChild<'_> {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            // A reaped leader's group may already be reused; only pending or
            // cancelled work needs the kill.
            self.setup.untrack(group, !self.reaped);
        }
        if !self.reaped {
            let _ = self.child.start_kill();
        }
    }
}

fn open_log(path: &Path) -> Result<File, String> {
    std::fs::create_dir_all(path.parent().ok_or("Invalid install log location")?)
        .map_err(|_| "Could not create the Harnesses install log directory")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "Could not open the Harnesses install log".into())
}

/// Runs `installer` with a fresh log, then reports the log's last 8 KiB.
pub async fn run_install<F, Fut>(path: &Path, installer: F) -> Result<InstallReport, String>
where
    F: FnOnce(File) -> Fut,
    Fut: Future<Output = Result<bool, String>>,
{
    use std::io::{Read, Seek, SeekFrom, Write};
    let log = open_log(path)?;
    let status = installer(log).await;
    if let Err(message) = &status {
        let mut file = OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|_| "Could not write to the Harnesses install log")?;
        writeln!(file, "\n{message}")
            .map_err(|_| "Could not write to the Harnesses install log")?;
    }
    let unreadable = "Could not read the Harnesses install log";
    let mut file = File::open(path).map_err(|_| unreadable)?;
    let length = file.metadata().map_err(|_| unreadable)?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(8192)))
        .map_err(|_| unreadable)?;
    let mut tail = Vec::new();
    file.take(8192)
        .read_to_end(&mut tail)
        .map_err(|_| unreadable)?;
    let error = match status {
        Ok(true) => None,
        Ok(false) => Some("Harness installer failed. See the install log.".to_owned()),
        Err(message) => Some(message),
    };
    Ok(InstallReport {
        ready: error.is_none(),
        restarted: 0,
        restart_failures: 0,
        log_path: path.to_string_lossy().into_owned(),
        output: String::from_utf8_lossy(&tail).into_owned(),
        error,
    })
}

/// The install log for `tool` under the host's state directory.
pub fn log_path(app_data: &Path, tool: Tool) -> PathBuf {
    app_data.join(tool.log())
}

/// An agent whose start failed only because Pi was missing.
pub fn waiting_for_pi(agent: &buzz_agent_controller::AgentView) -> bool {
    waiting(
        agent.enabled,
        agent.status,
        &agent.harness.command,
        "buzz-pi-acp",
        agent.error.as_deref(),
    )
}

fn waiting(
    enabled: bool,
    status: ProcessStatus,
    command: &str,
    name: &str,
    error: Option<&str>,
) -> bool {
    if !enabled || status != ProcessStatus::Failed {
        return false;
    }
    if Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(name)
    {
        return false;
    }
    // Only a start that failed on the missing CLI was waiting. Stopped agents
    // are indistinguishable from ones the person chose not to run, and a
    // relative command still fails after install.
    error == Some("Required runtime executable is missing")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Artifact {
    platform: &'static str,
    filename: &'static str,
    sha256: &'static str,
}

fn artifact(os: &str, arch: &str) -> Option<Artifact> {
    let (platform, sha256) = match (os, arch) {
        ("macos", "aarch64") => (
            "darwin-arm64",
            "e1a97e14c99c803e96c7339403282ea05a499c32f8d83defe9ef5ec66f979ed1",
        ),
        ("macos", "x86_64") => (
            "darwin-x64",
            "dfd0dbd3e721503434df7b7205e719f61b3a3a31b2bcf9729b8b91fea240f080",
        ),
        ("linux", "x86_64") => (
            "linux-x64",
            "783130984963db7ba9cbd01089eaf2c2efb055c7c1693c943174b967b3050cb8",
        ),
        ("linux", "aarch64") => (
            "linux-arm64",
            "6b4484c2190274175df9aa8f28e2d758a819cb1c1fe6ab481e2f95b463ab8508",
        ),
        _ => return None,
    };
    Some(Artifact {
        platform,
        filename: match platform {
            "darwin-arm64" => "node-v24.18.0-darwin-arm64.tar.gz",
            "darwin-x64" => "node-v24.18.0-darwin-x64.tar.gz",
            "linux-x64" => "node-v24.18.0-linux-x64.tar.gz",
            _ => "node-v24.18.0-linux-arm64.tar.gz",
        },
        sha256,
    })
}

fn refuse_linked_prefix(prefix: &Path) -> Result<(), String> {
    for path in [
        prefix.to_path_buf(),
        prefix.join("bin"),
        prefix.join("lib"),
        prefix.join("lib/node_modules"),
        prefix.join("lib/node_modules/@earendil-works"),
        prefix.join("lib/node_modules/@earendil-works/pi-coding-agent"),
        prefix.join("lib/node_modules/buzz-pi-acp"),
        prefix.join("lib/node_modules/@anthropic-ai"),
        prefix.join("lib/node_modules/@agentclientprotocol"),
        prefix.join("cache"),
        prefix.join("etc"),
        prefix.join("releases"),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err("App-owned npm prefix contains a link or non-directory".into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Could not inspect app-owned npm prefix".into()),
            _ => {}
        }
    }
    Ok(())
}

fn node_dir(app_data: &Path, spec: Artifact) -> PathBuf {
    app_data
        .join("runtimes/node")
        .join(VERSION)
        .join(spec.platform)
}

fn verified(archive: &[u8], spec: Artifact) -> Result<(), String> {
    if archive.len() as u64 > MAX_ARCHIVE || format!("{:x}", Sha256::digest(archive)) != spec.sha256
    {
        return Err("Managed Node archive checksum or size mismatch".into());
    }
    Ok(())
}

async fn download(spec: Artifact) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not prepare managed Node download")?;
    let url = format!("https://nodejs.org/dist/{VERSION}/{}", spec.filename);
    let mut response =
        client.get(url).send().await.map_err(
            |_| "Could not download Node.js from nodejs.org; check your network or proxy",
        )?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > MAX_ARCHIVE)
    {
        return Err("Managed Node download was unavailable or too large".into());
    }
    let mut archive = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Managed Node download interrupted")?
    {
        if archive.len() as u64 + chunk.len() as u64 > MAX_ARCHIVE {
            return Err("Managed Node download exceeded size limit".into());
        }
        archive.extend_from_slice(&chunk);
    }
    verified(&archive, spec)?;
    Ok(archive)
}

/// A cleared environment: the pinned Node and system tools on PATH, npm's
/// registry, proxy and CA settings, and an app-owned prefix, cache and
/// global config.
fn scrub(
    command: &mut tokio::process::Command,
    home: &Path,
    node_bin: &Path,
    app_data: &Path,
) -> Result<(), String> {
    let path = std::env::join_paths([
        node_bin,
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ])
    .map_err(|_| "Invalid managed Node PATH")?;
    command.env_clear();
    // npm reads registry, auth, CA and proxy settings from ~/.npmrc (real
    // HOME) and from npm_config_* variables. Keep those so a mirror still
    // works, but never let them move the app-owned prefix, cache or global
    // config.
    for (name, value) in std::env::vars_os() {
        let Some(key) = name.to_str().map(str::to_ascii_lowercase) else {
            continue;
        };
        if key.starts_with("npm_config_")
            && !matches!(
                key.as_str(),
                "npm_config_prefix" | "npm_config_cache" | "npm_config_globalconfig"
            )
        {
            command.env(name, value);
        }
    }
    for name in [
        "TMPDIR",
        "USER",
        "LOGNAME",
        "LANG",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "http_proxy",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("HOME", home)
        .env("PATH", path)
        .env("npm_config_cache", app_data.join("node-tools/cache"))
        .env("npm_config_prefix", app_data.join("node-tools"))
        .env(
            "npm_config_globalconfig",
            app_data.join("node-tools/etc/npmrc"),
        )
        .current_dir(home)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    Ok(())
}

async fn run_step(
    setup: &Setup,
    command: &mut tokio::process::Command,
    log: &File,
    failure: &str,
) -> Result<(), String> {
    let start = log
        .metadata()
        .map_err(|_| "Could not inspect install log")?
        .len();
    command
        .stdout(Stdio::from(
            log.try_clone().map_err(|_| "Could not copy install log")?,
        ))
        .stderr(Stdio::from(
            log.try_clone().map_err(|_| "Could not copy install log")?,
        ));
    let mut child = setup
        .spawn(|| command.spawn())
        .map_err(|error| format!("{failure}: {error}"))?;
    let status = tokio::time::timeout(Duration::from_secs(300), child.child.wait())
        .await
        .map_err(|_| format!("{failure}: timed out. See the install log."))?
        .map_err(|_| format!("{failure}: could not finish. See the install log."))?;
    child.reaped = true;
    drop(child);
    if !status.success() {
        let exit = status.code().map_or_else(
            || "process terminated".to_owned(),
            |code| format!("exit code {code}"),
        );
        let detail =
            npm_error(log, start).unwrap_or_else(|| "See the install log for details.".into());
        return Err(format!("{failure} ({exit}).\n{detail}"));
    }
    Ok(())
}

/// The visible diagnostic stays short; the private log keeps everything.
/// Only this step's output is read, so an earlier npm run cannot supply
/// its error.
fn npm_error(log: &File, start: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = log.try_clone().ok()?;
    let end = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(start.max(end.saturating_sub(8192))))
        .ok()?;
    let mut tail = Vec::new();
    file.take(8192).read_to_end(&mut tail).ok()?;
    let output = String::from_utf8_lossy(&tail);
    let lines: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("npm error ") || line.starts_with("npm ERR! "))
        .filter(|line| !line.contains("A complete log of this run"))
        .take(6)
        .map(|line| line.chars().take(200).collect::<String>())
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// buzz-app runs `/usr/bin/tar`; systems without it keep tar in `/bin`.
fn tar() -> &'static Path {
    ["/usr/bin/tar", "/bin/tar"]
        .into_iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .unwrap_or(Path::new("/usr/bin/tar"))
}

async fn install_node(
    setup: &Setup,
    app_data: &Path,
    home: &Path,
    log: &File,
    spec: Artifact,
) -> Result<PathBuf, String> {
    let node_dir = node_dir(app_data, spec);
    let node = node_dir.join("bin/node");
    let npm = node_dir.join("lib/node_modules/npm/bin/npm-cli.js");
    if node.is_file() && npm.is_file() {
        return Ok(node);
    }
    let archive = download(spec).await?;
    let root = app_data.join("runtimes/node");
    std::fs::create_dir_all(&root).map_err(|_| "Could not create managed Node directory")?;
    let stage = root.join(format!("{}.{}.tmp", VERSION, spec.platform));
    if stage.exists() {
        std::fs::remove_dir_all(&stage)
            .map_err(|_| "Could not remove stale Node staging directory")?;
    }
    std::fs::create_dir_all(&stage).map_err(|_| "Could not stage managed Node")?;
    let archive_path = stage.join("node.tar.gz");
    let result = async {
        std::fs::write(&archive_path, archive).map_err(|_| "Could not stage Node archive")?;
        let tar_path = tar();
        let mut tar = tokio::process::Command::new(tar_path);
        scrub(
            &mut tar,
            home,
            tar_path.parent().unwrap_or(Path::new("/usr/bin")),
            app_data,
        )?;
        tar.arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&stage)
            .arg("--strip-components=1");
        run_step(setup, &mut tar, log, "Unpacking Node.js failed").await?;
        std::fs::remove_file(&archive_path).map_err(|_| "Could not remove Node archive")?;
        if !stage.join("bin/node").is_file()
            || !stage.join("lib/node_modules/npm/bin/npm-cli.js").is_file()
        {
            return Err("Managed Node archive was incomplete".into());
        }
        std::fs::create_dir_all(node_dir.parent().ok_or("Invalid Node path")?)
            .map_err(|_| "Could not create Node version directory")?;
        if node_dir.exists() {
            std::fs::remove_dir_all(&node_dir)
                .map_err(|_| "Could not replace incomplete Node runtime")?;
        }
        std::fs::rename(&stage, &node_dir).map_err(|_| "Could not activate managed Node")?;
        Ok(node)
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_dir_all(stage);
    }
    result
}

fn npm_command(
    node: &Path,
    app_data: &Path,
    home: &Path,
    prefix: &Path,
    package: &str,
    install_links: bool,
) -> Result<tokio::process::Command, String> {
    let bin = node.parent().ok_or("Invalid managed Node path")?;
    let npm = bin
        .parent()
        .ok_or("Invalid managed Node path")?
        .join("lib/node_modules/npm/bin/npm-cli.js");
    let mut command = tokio::process::Command::new(node);
    scrub(&mut command, home, bin, app_data)?;
    command
        .arg(npm)
        .args(["install", "--global", "--prefix"])
        .arg(prefix);
    if install_links {
        command.arg("--install-links=true");
    }
    command.arg(package);
    Ok(command)
}

fn adapter_rev() -> &'static str {
    ADAPTER.rsplit_once('#').map_or(ADAPTER, |(_, rev)| rev)
}

/// The release an app-owned shim points at: `../releases/<id>/bin/<name>`.
fn release_id(shim: &Path) -> Option<String> {
    let target = std::fs::read_link(shim).ok()?;
    let rest = target.strip_prefix("../releases").ok()?;
    rest.components()
        .next()?
        .as_os_str()
        .to_str()
        .map(str::to_owned)
}

/// Whether both app-owned Pi shims point at a release of the pinned
/// adapter.
pub fn current(app_data: &Path) -> bool {
    let bin = app_data.join("node-tools/bin");
    ["pi", "buzz-pi-acp"].iter().all(|name| {
        release_id(&bin.join(name))
            .is_some_and(|id| id.split_once('.').map(|(rev, _)| rev) == Some(adapter_rev()))
    })
}

/// Each shim moves by rename, so a starting agent sees an old or a new
/// complete release. Pi moves first because the previous adapter also runs
/// on newer Pi.
#[cfg(unix)]
fn activate(tools: &Path, id: &str, tool: Tool) -> Result<(), String> {
    let bin = tools.join("bin");
    std::fs::create_dir_all(&bin).map_err(|_| "Could not create app-owned tools directory")?;
    for name in tool.binaries() {
        let staged = bin.join(format!(".{name}.{id}"));
        let _ = std::fs::remove_file(&staged);
        std::os::unix::fs::symlink(
            Path::new("../releases").join(id).join("bin").join(name),
            &staged,
        )
        .and_then(|()| std::fs::rename(&staged, bin.join(name)))
        .map_err(|_| "Could not activate the new Harness install")?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn activate(_: &Path, _: &str, _: Tool) -> Result<(), String> {
    Err("Harness installation is supported only on macOS and Linux".into())
}

/// Keeps the previous release for agents still running from it.
fn prune(releases: &Path, keep: &[&str]) {
    let Ok(entries) = std::fs::read_dir(releases) else {
        return;
    };
    for entry in entries.flatten() {
        if !keep.iter().any(|id| entry.file_name() == *id) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Installs Node.js (once) and `tool`'s packages as a new release, then
/// switches the shims to it.
pub async fn install(
    setup: &Setup,
    app_data: &Path,
    log: File,
    tool: Tool,
) -> Result<bool, String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("Harness install requires HOME")?);
    let spec = artifact(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or("Managed Node is unavailable on this platform")?;
    let node = install_node(setup, app_data, &home, &log, spec).await?;
    let tools = app_data.join(tool.prefix());
    refuse_linked_prefix(&tools)?;
    let releases = tools.join("releases");
    std::fs::create_dir_all(&releases).map_err(|_| "Could not create app-owned npm prefix")?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let id = format!("{}.{millis}", tool.revision());
    let prefix = releases.join(&id);
    std::fs::create_dir(&prefix).map_err(|_| "Could not create app-owned npm prefix")?;
    let result = async {
        for (package, install_links, failure) in tool.packages() {
            refuse_linked_prefix(&tools)?;
            refuse_linked_prefix(&prefix)?;
            let mut command = npm_command(&node, app_data, &home, &prefix, package, install_links)?;
            run_step(setup, &mut command, &log, failure).await?;
        }
        // Claude's native package can leave a failing stub despite npm
        // succeeding.
        if tool == Tool::Claude {
            for name in tool.binaries() {
                let mut command = tokio::process::Command::new(prefix.join("bin").join(name));
                scrub(
                    &mut command,
                    &home,
                    node.parent().ok_or("Invalid managed Node path")?,
                    app_data,
                )?;
                command.arg("--version");
                run_step(
                    setup,
                    &mut command,
                    &log,
                    &format!("Verifying {name} failed"),
                )
                .await?;
            }
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&prefix);
        return Err(error);
    }
    let previous = release_id(&tools.join("bin").join(tool.binaries()[1]));
    activate(&tools, &id, tool)?;
    let mut keep = vec![id.as_str()];
    keep.extend(previous.as_deref());
    prune(&releases, &keep);
    Ok(true)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// Creates an executable without holding a write descriptor another
    /// test's fork could inherit (which makes exec fail with ETXTBSY).
    fn write_executable(path: &Path, contents: impl AsRef<[u8]>) {
        use std::os::unix::fs::PermissionsExt;
        let mut source = path.as_os_str().to_owned();
        source.push(".source");
        let source = Path::new(&source);
        std::fs::write(source, contents).unwrap();
        let status = std::process::Command::new("cp")
            .arg(source)
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "could not copy {}", path.display());
        std::fs::remove_file(source).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// The absolute path of a system tool, since installer steps run with
    /// a PATH that may not hold it (NixOS has no `/usr/bin/mkdir`).
    fn which(name: &str) -> String {
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("command -v {name}")])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[tokio::test]
    async fn one_install_at_a_time_and_success_log() {
        let state = std::sync::Arc::new(Setup::default());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-controller/pi-install.log");
        let (started, observed) = tokio::sync::oneshot::channel();
        let (release, done) = tokio::sync::oneshot::channel::<()>();
        let installing = state.clone();
        let output = path.clone();
        let task = tokio::spawn(async move {
            let _guard = installing.claim().unwrap();
            run_install(&output, |mut log| async move {
                started.send(()).unwrap();
                done.await.unwrap();
                writeln!(log, "CLI installed").unwrap();
                Ok(true)
            })
            .await
            .unwrap()
        });
        observed.await.unwrap();
        assert!(state.claim().is_err());
        release.send(()).unwrap();
        let report = task.await.unwrap();
        assert!(state.claim().is_ok());
        assert!(report.ready);
        assert_eq!(report.output.trim(), "CLI installed");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn failure_records_output_and_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pi-install.log");
        let report = run_install(&path, |mut log| async move {
            writeln!(log, "stdout and stderr").unwrap();
            Err("Installer exited unexpectedly".into())
        })
        .await
        .unwrap();
        assert!(!report.ready);
        assert_eq!(
            report.error.as_deref(),
            Some("Installer exited unexpectedly")
        );
        assert!(report.output.contains("stdout and stderr"));
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("Installer exited unexpectedly")
        );
    }

    #[tokio::test]
    async fn shutdown_kills_the_installer_and_fences_later_ones() {
        use tokio::io::AsyncBufReadExt as _;
        let state = Setup::default();
        let _guard = state.claim().unwrap();
        let mut active = state
            .spawn(|| {
                tokio::process::Command::new("/bin/sh")
                    .args(["-c", "printf 'ready\\n'; while :; do read x || :; done"])
                    .process_group(0)
                    .kill_on_drop(true)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
            })
            .unwrap();
        let mut marker = String::new();
        tokio::io::BufReader::new(active.child.stdout.take().unwrap())
            .read_line(&mut marker)
            .await
            .unwrap();
        assert_eq!(marker, "ready\n");
        state.shutdown();
        assert!(!active.child.wait().await.unwrap().success());
        active.reaped = true;
        drop(active);
        assert!(
            state
                .spawn(|| tokio::process::Command::new("/bin/sh").spawn())
                .is_err()
        );
    }

    #[test]
    fn only_agents_that_failed_on_the_missing_cli_wait_for_pi() {
        let missing = Some("Required runtime executable is missing");
        let pi = "/data/node-tools/bin/buzz-pi-acp";
        assert!(waiting(
            true,
            ProcessStatus::Failed,
            pi,
            "buzz-pi-acp",
            missing
        ));
        assert!(!waiting(
            false,
            ProcessStatus::Failed,
            pi,
            "buzz-pi-acp",
            missing
        ));
        assert!(!waiting(
            true,
            ProcessStatus::Stopped,
            pi,
            "buzz-pi-acp",
            missing
        ));
        assert!(!waiting(
            true,
            ProcessStatus::Failed,
            "/usr/bin/goose",
            "buzz-pi-acp",
            missing
        ));
        assert!(!waiting(
            true,
            ProcessStatus::Failed,
            pi,
            "buzz-pi-acp",
            Some("Relay refused the key")
        ));
    }

    #[tokio::test]
    async fn failed_steps_report_their_own_npm_error() {
        let setup = Setup::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("install.log");
        let mut log = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        writeln!(
            log,
            "npm error code EACCES\nstale output from an earlier step"
        )
        .unwrap();
        for (step, output) in [
            (
                "Installing Pi failed",
                "npm error code E404\nnpm error 404 No match found for version >=0.99.0",
            ),
            (
                "Installing the Pi ACP adapter failed",
                "npm error code ENOENT\nnpm error spawn git ENOENT",
            ),
            ("Installing Pi failed", ""),
        ] {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .args(["-c", "printf '%s\\n' \"$1\" >&2; exit 1", "fixture", output])
                .process_group(0)
                .kill_on_drop(true);
            let error = run_step(&setup, &mut command, &log, step)
                .await
                .unwrap_err();
            assert!(
                error.starts_with(&format!("{step} (exit code 1).")),
                "{error}"
            );
            if !output.is_empty() {
                assert!(error.contains(output), "{error}");
            }
            assert!(!error.contains("EACCES"), "{error}");
            if step.contains("adapter") || output.is_empty() {
                assert!(!error.contains("E404"), "{error}");
            }
        }
    }

    #[test]
    fn pinned_artifacts_are_platform_specific() {
        for (os, arch, platform) in [
            ("macos", "aarch64", "darwin-arm64"),
            ("macos", "x86_64", "darwin-x64"),
            ("linux", "x86_64", "linux-x64"),
            ("linux", "aarch64", "linux-arm64"),
        ] {
            let spec = artifact(os, arch).unwrap();
            assert_eq!(spec.platform, platform);
            assert_eq!(spec.sha256.len(), 64);
            assert!(spec.filename.ends_with(&format!("{platform}.tar.gz")));
        }
        assert!(artifact("windows", "x86_64").is_none());
        assert!(artifact("linux", "riscv64").is_none());
        assert!(verified(b"untrusted bytes", artifact("linux", "x86_64").unwrap()).is_err());
    }

    #[test]
    fn scrubbed_environment_has_only_managed_node_and_system_tools() {
        let mut cmd = tokio::process::Command::new("/usr/bin/env");
        scrub(
            &mut cmd,
            Path::new("/temporary/home"),
            Path::new("/managed/node/bin"),
            Path::new("/app/data"),
        )
        .unwrap();
        let env: Vec<_> = cmd.as_std().get_envs().collect();
        let get = |name: &str| {
            env.iter()
                .find(|(key, _)| *key == name)
                .and_then(|(_, value)| *value)
        };
        assert_eq!(
            get("PATH"),
            Some(std::ffi::OsStr::new(
                "/managed/node/bin:/usr/bin:/bin:/usr/sbin:/sbin"
            ))
        );
        assert_eq!(get("HOME"), Some(std::ffi::OsStr::new("/temporary/home")));
        assert_eq!(
            get("npm_config_prefix"),
            Some(std::ffi::OsStr::new("/app/data/node-tools"))
        );
        assert!(env.iter().all(|(name, _)| *name != "BUZZ_PRIVATE_KEY"));
    }

    #[test]
    fn linked_prefix_cannot_redirect_the_install() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("node-tools");
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&other, &prefix).unwrap();
        assert!(refuse_linked_prefix(&prefix).is_err());
        std::fs::remove_file(&prefix).unwrap();
        std::fs::create_dir_all(&prefix).unwrap();
        for destination in ["lib", "lib/node_modules", "etc", "releases"] {
            let path = prefix.join(destination);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&other, &path).unwrap();
            assert!(refuse_linked_prefix(&prefix).is_err(), "{destination}");
            std::fs::remove_file(path).unwrap();
        }
        assert!(refuse_linked_prefix(&prefix).is_ok());
    }

    #[test]
    fn activation_swaps_both_shims_and_prunes_older_releases() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("node-tools");
        let release = |id: &str| {
            let bin = tools.join("releases").join(id).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            for name in ["pi", "buzz-pi-acp"] {
                write_executable(&bin.join(name), id);
            }
        };
        let adapter = || std::fs::read_to_string(tools.join("bin/buzz-pi-acp")).unwrap();
        let old = "86b201e.1";
        let pinned = format!("{}.2", adapter_rev());
        let newer = format!("{}.3", adapter_rev());
        release(old);
        activate(&tools, old, Tool::Pi).unwrap();
        assert_eq!(adapter(), old);
        assert!(!current(dir.path()));

        release(&pinned);
        activate(&tools, &pinned, Tool::Pi).unwrap();
        assert!(current(dir.path()));
        assert_eq!(adapter(), pinned);
        assert!(buzz_agent_controller::managed_tool(dir.path(), "buzz-pi-acp").is_some());

        release(&newer);
        std::fs::create_dir_all(tools.join("releases/failed.4")).unwrap();
        prune(&tools.join("releases"), &[newer.as_str(), pinned.as_str()]);
        let mut left: Vec<_> = std::fs::read_dir(tools.join("releases"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [pinned, newer]);
    }

    #[test]
    fn npm_installs_only_the_approved_packages_into_the_prefix() {
        let node = Path::new("/app/data/runtimes/node/v24.18.0/linux-x64/bin/node");
        let prefix = Path::new("/app/data/node-tools/releases/72015de.1");
        let args = |package, links| {
            npm_command(
                node,
                Path::new("/app/data"),
                Path::new("/temporary/home"),
                prefix,
                package,
                links,
            )
            .unwrap()
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
        };
        let npm = "/app/data/runtimes/node/v24.18.0/linux-x64/lib/node_modules/npm/bin/npm-cli.js";
        let release = "/app/data/node-tools/releases/72015de.1";
        assert_eq!(
            args(PI, false),
            [npm, "install", "--global", "--prefix", release, PI]
        );
        assert_eq!(
            args(ADAPTER, true),
            [
                npm,
                "install",
                "--global",
                "--prefix",
                release,
                "--install-links=true",
                ADAPTER
            ]
        );
    }

    /// The whole install against a fake Node whose "npm" writes the
    /// requested package's launcher: nothing is downloaded or installed.
    #[tokio::test]
    async fn claude_install_rejects_a_failing_stub_and_keeps_the_previous_release() {
        let dir = tempfile::tempdir().unwrap();
        let app_data = dir.path();
        let setup = Setup::default();
        let spec = artifact(std::env::consts::OS, std::env::consts::ARCH).unwrap();
        let node_root = node_dir(app_data, spec);
        std::fs::create_dir_all(node_root.join("bin")).unwrap();
        std::fs::create_dir_all(node_root.join("lib/node_modules/npm/bin")).unwrap();
        std::fs::write(node_root.join("lib/node_modules/npm/bin/npm-cli.js"), "").unwrap();
        let node = node_root.join("bin/node");
        let (mkdir, chmod) = (which("mkdir"), which("chmod"));
        let npm = |cli_exit: u8| {
            format!(
                r#"#!/bin/sh
set -eu
prefix="$5"
case "$6" in
  @anthropic-ai/claude-code@*) name=claude; code={cli_exit} ;;
  @agentclientprotocol/claude-agent-acp@*) name=claude-agent-acp; code=0 ;;
  *) exit 2 ;;
esac
{mkdir} -p "$prefix/bin"
printf '#!/bin/sh\nexit %s\n' "$code" > "$prefix/bin/$name"
{chmod} 755 "$prefix/bin/$name"
"#
            )
        };
        let tools = app_data.join("claude-tools");
        let old = tools.join("releases/previous/bin");
        std::fs::create_dir_all(&old).unwrap();
        for name in ["claude", "claude-agent-acp"] {
            write_executable(&old.join(name), "#!/bin/sh\nexit 0\n");
        }
        activate(&tools, "previous", Tool::Claude).unwrap();
        let log_path = app_data.join("install.log");
        write_executable(&node, npm(1));
        let error = install(
            &setup,
            app_data,
            File::create(&log_path).unwrap(),
            Tool::Claude,
        )
        .await
        .unwrap_err();
        assert!(error.contains("Verifying claude failed"), "{error}");
        assert_eq!(
            release_id(&tools.join("bin/claude")),
            Some("previous".into())
        );
        assert_eq!(
            std::fs::read_dir(tools.join("releases")).unwrap().count(),
            1
        );
        write_executable(&node, npm(0));
        assert!(
            install(
                &setup,
                app_data,
                File::create(&log_path).unwrap(),
                Tool::Claude
            )
            .await
            .unwrap()
        );
        assert_ne!(
            release_id(&tools.join("bin/claude")),
            Some("previous".into())
        );
        assert!(buzz_agent_controller::managed_tool(app_data, "claude-agent-acp").is_some());
        // The previous release stays for agents still running from it.
        assert!(old.join("claude").is_file());
    }
}
