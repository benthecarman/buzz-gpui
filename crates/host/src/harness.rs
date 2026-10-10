//! Finds the agent harnesses installed on this machine.
//!
//! Discovery uses the controller's tool PATH (the user's login-shell PATH
//! plus common install locations), the same PATH agents launch with.

use crate::api::{Harness, HarnessKind, HarnessStatus};
/// The preset catalog (Hermes, Claude Code): labels, setup guides and hints
/// for harnesses Buzz does not configure itself.
pub use buzz_agent_controller::{HarnessPreset, harness_presets};
use buzz_agent_controller::{harness_preset, installed, installed_npm_tool, managed_tool};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

struct Spec {
    kind: HarnessKind,
    label: &'static str,
    /// Executables the harness needs. The first one is the ACP worker.
    requires: &'static [&'static str],
    /// The controller resolves these commands itself; others are stored as
    /// the absolute path of the worker.
    named_command: Option<&'static str>,
    preset: Option<&'static str>,
    setup_hint: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        kind: HarnessKind::BuzzAgent,
        label: "Buzz Agent",
        requires: &["buzz-agent", "buzz-dev-mcp"],
        named_command: Some("buzz-agent"),
        preset: None,
        setup_hint: "Install buzz-agent and buzz-dev-mcp from block/buzz.",
    },
    Spec {
        kind: HarnessKind::Goose,
        label: "Goose",
        requires: &["goose-acp"],
        named_command: Some("goose"),
        preset: None,
        setup_hint: "Install goose-acp and configure Goose with `goose configure`.",
    },
    Spec {
        kind: HarnessKind::Claude,
        label: "Claude Code",
        requires: &["claude-agent-acp", "claude", "node"],
        named_command: None,
        preset: Some("claude-agent-acp"),
        setup_hint: "Install Claude Code and `@agentclientprotocol/claude-agent-acp` with npm, then sign in with `claude`.",
    },
    Spec {
        kind: HarnessKind::Pi,
        label: "Pi",
        requires: &["buzz-pi-acp", "pi", "node"],
        named_command: None,
        preset: None,
        setup_hint: "Install the Pi CLI and buzz-pi-acp with npm.",
    },
    Spec {
        kind: HarnessKind::Hermes,
        label: "Hermes Agent",
        requires: &["hermes-acp"],
        named_command: None,
        preset: Some("hermes-acp"),
        setup_hint: "Install Hermes with ACP support and configure it with `hermes model`.",
    },
];

/// The listener every harness runs under.
pub const LISTENER: &str = "buzz-acp";

/// A provider choice in the agent form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    /// The environment variable holding its API key, when it takes one.
    pub key: Option<&'static str>,
}

const fn provider(id: &'static str, label: &'static str, key: Option<&'static str>) -> Provider {
    Provider { id, label, key }
}

/// buzz-app's `harnessOptions` providers (`src-tauri/src/agents.rs`) and
/// API key variables (`src/bundled/agents/agent-edit.ts`).
const BUZZ_AGENT_PROVIDERS: &[Provider] = &[
    provider("databricks_v2", "Databricks v2", None),
    provider("openai", "OpenAI", Some("OPENAI_COMPAT_API_KEY")),
];
const GOOSE_PROVIDERS: &[Provider] = &[
    provider("anthropic", "Anthropic", Some("ANTHROPIC_API_KEY")),
    provider("openai", "OpenAI", Some("OPENAI_API_KEY")),
    provider("openrouter", "OpenRouter", Some("OPENROUTER_API_KEY")),
    provider("google", "Google Gemini", Some("GOOGLE_API_KEY")),
    provider("github_copilot", "GitHub Copilot", None),
    provider("databricks", "Databricks", None),
    provider("databricks_v2", "Databricks v2", None),
    provider("ollama", "Ollama", None),
    provider("groq", "Groq", Some("GROQ_API_KEY")),
    provider("mistral", "Mistral AI", Some("MISTRAL_API_KEY")),
    provider("together", "Together AI", Some("TOGETHER_API_KEY")),
    provider("perplexity", "Perplexity", Some("PERPLEXITY_API_KEY")),
    provider("cerebras", "Cerebras", Some("CEREBRAS_API_KEY")),
    provider("custom_deepseek", "DeepSeek", Some("DEEPSEEK_API_KEY")),
];
/// Pi lists signed-in providers through its model catalog; these are the
/// ones a key can sign in to from the form.
const PI_PROVIDERS: &[Provider] = &[
    provider("openai", "OpenAI", Some("OPENAI_API_KEY")),
    provider("anthropic", "Anthropic", Some("ANTHROPIC_API_KEY")),
    provider("google", "Google Gemini", Some("GEMINI_API_KEY")),
    provider("openrouter", "OpenRouter", Some("OPENROUTER_API_KEY")),
];

/// Providers the harness can be pointed at. Presets choose their own.
pub fn providers(kind: HarnessKind) -> &'static [Provider] {
    match kind {
        HarnessKind::BuzzAgent => BUZZ_AGENT_PROVIDERS,
        HarnessKind::Goose => GOOSE_PROVIDERS,
        HarnessKind::Pi => PI_PROVIDERS,
        HarnessKind::Claude | HarnessKind::Hermes => &[],
    }
}

/// Suggested effort levels (buzz-app's `effortChoices`); not evidence of
/// what a model supports.
pub fn efforts(kind: HarnessKind) -> &'static [&'static str] {
    match kind {
        HarnessKind::BuzzAgent => &["none", "minimal", "low", "medium", "high", "xhigh", "max"],
        HarnessKind::Goose => &["off", "low", "medium", "high", "max"],
        HarnessKind::Pi => &["off", "minimal", "low", "medium", "high", "xhigh"],
        HarnessKind::Claude | HarnessKind::Hermes => &[],
    }
}

/// The name agent defaults use for a harness; only these three can be a
/// default.
pub fn defaults_name(kind: HarnessKind) -> Option<&'static str> {
    match kind {
        HarnessKind::BuzzAgent => Some("buzz-agent"),
        HarnessKind::Goose => Some("goose"),
        HarnessKind::Pi => Some("pi"),
        HarnessKind::Claude | HarnessKind::Hermes => None,
    }
}

pub fn from_defaults_name(name: &str) -> Option<HarnessKind> {
    match name {
        "buzz-agent" => Some(HarnessKind::BuzzAgent),
        "goose" => Some(HarnessKind::Goose),
        "pi" => Some(HarnessKind::Pi),
        _ => None,
    }
}

/// One npm-installed harness's tools: its CLI, its ACP adapter and the
/// Node.js they run on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NpmTools {
    pub cli: Option<PathBuf>,
    pub adapter: Option<PathBuf>,
    pub node: Option<PathBuf>,
}

impl NpmTools {
    fn status(&self) -> HarnessStatus {
        if self.cli.is_none() || self.node.is_none() {
            HarnessStatus::CliNeeded
        } else if self.adapter.is_none() {
            HarnessStatus::AdapterNeeded
        } else {
            HarnessStatus::Ready
        }
    }
}

/// buzz-app's `npm_choice`: a complete user install always wins. Otherwise
/// the host's own install is used when its pinned Node can run its npm
/// shims. Returns the tools in use, their status, and whether they are the
/// host's own.
pub fn npm_choice(user: NpmTools, managed: NpmTools) -> (NpmTools, HarnessStatus, bool) {
    let user_ready = user.cli.is_some() && user.adapter.is_some() && user.node.is_some();
    let managed_selected = !user_ready && managed.adapter.is_some() && managed.node.is_some();
    let selected = if managed_selected {
        NpmTools {
            cli: managed.cli.or(user.cli),
            ..managed
        }
    } else {
        user
    };
    let status = selected.status();
    (selected, status, managed_selected)
}

/// Pi's tools: on PATH, or the host's own install under `app_data`.
fn pi_tools(app_data: &Path) -> (NpmTools, HarnessStatus, bool) {
    npm_choice(
        NpmTools {
            cli: installed("pi"),
            adapter: installed("buzz-pi-acp"),
            node: installed("node"),
        },
        NpmTools {
            cli: managed_tool(app_data, "pi"),
            adapter: managed_tool(app_data, "buzz-pi-acp"),
            node: managed_tool(app_data, "node"),
        },
    )
}

/// Claude Code's tools (buzz-app's `claude_setup`).
pub struct ClaudeSetup {
    pub tools: NpmTools,
    pub status: HarnessStatus,
    /// The host's own Node.js directory, put first on PATH when the host
    /// installed Claude Code itself.
    pub node_bin: Option<PathBuf>,
}

pub fn claude_setup(app_data: &Path) -> ClaudeSetup {
    let user_cli = installed_npm_tool("claude");
    let (mut tools, status, managed) = npm_choice(
        NpmTools {
            cli: user_cli.clone(),
            adapter: installed_npm_tool("claude-agent-acp"),
            node: installed_npm_tool("node"),
        },
        NpmTools {
            cli: managed_tool(app_data, "claude"),
            adapter: managed_tool(app_data, "claude-agent-acp"),
            node: managed_tool(app_data, "node"),
        },
    );
    if !managed {
        tools.cli = user_cli;
    }
    let node_bin = if managed {
        tools
            .node
            .as_ref()
            .and_then(|node| node.parent())
            .map(Path::to_path_buf)
    } else {
        None
    };
    ClaudeSetup {
        tools,
        status,
        node_bin,
    }
}

/// The command that signs in to Claude Code, quoted for a POSIX shell.
pub fn login_command(cli: &Path, node_bin: Option<&Path>) -> String {
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    match node_bin {
        Some(bin) => format!("PATH={}:\"$PATH\" {} auth login", quote(bin), quote(cli)),
        None => format!("{} auth login", quote(cli)),
    }
}

/// Asks Claude Code whether it is signed in: `claude auth status` within
/// five seconds. `Some(false)` only when it says so; `None` when unsure.
pub async fn claude_signed_in(cli: &Path, path: &std::ffi::OsStr) -> Option<bool> {
    let (output, status) =
        run_output(cli, &["auth", "status"], Duration::from_secs(5), path, 4096).await?;
    let logged_in = serde_json::from_str::<serde_json::Value>(&output)
        .ok()?
        .get("loggedIn")?
        .as_bool()?;
    match (status.code(), logged_in) {
        (Some(0), true) => Some(true),
        (Some(1), false) => Some(false),
        _ => None,
    }
}

/// Runs `executable` with `PATH` set and stdin closed, reading at most
/// `max_output` bytes of stdout before `deadline`; its whole process group
/// is killed otherwise. buzz-app's `host_command::run_output`.
async fn run_output(
    executable: &Path,
    args: &[&str],
    deadline: Duration,
    path: &std::ffi::OsStr,
    max_output: u64,
) -> Option<(String, std::process::ExitStatus)> {
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if !path.is_empty() {
        command.env("PATH", path);
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().ok()?;
    let group = child.id();
    let stdout = child.stdout.take()?;
    let output = tokio::time::timeout(deadline, async {
        let mut bytes = Vec::new();
        stdout
            .take(max_output + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() as u64 > max_output {
            return None;
        }
        let status = child.wait().await.ok()?;
        Some((bytes, status))
    })
    .await;
    if let Ok(Some((bytes, status))) = output {
        return Some((String::from_utf8(bytes).ok()?, status));
    }
    #[cfg(unix)]
    if let Some(group) = group {
        // SAFETY: signals the process group this probe created.
        unsafe {
            libc::kill(-(group as i32), libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
    None
}

/// What an npm harness still needs, by executable name.
fn npm_missing(tools: &NpmTools, cli: &str, adapter: &str) -> Vec<String> {
    [
        (tools.adapter.is_none(), adapter),
        (tools.cli.is_none(), cli),
        (tools.node.is_none(), "node"),
    ]
    .into_iter()
    .filter(|(missing, _)| *missing)
    .map(|(_, name)| name.to_string())
    .collect()
}

/// Blocks until login-shell PATH discovery finishes (at most ~15 s, once per
/// process). `app_data` holds the harnesses this host installed itself.
pub fn detect(app_data: &Path) -> Vec<Harness> {
    buzz_agent_controller::prepare_tools_path();
    let listener_missing = installed(LISTENER).is_none();
    SPECS
        .iter()
        .map(|spec| {
            let preset = spec.preset.and_then(harness_preset);
            let mut harness = Harness {
                kind: spec.kind,
                label: preset.map_or(spec.label, |p| p.label.as_str()).to_string(),
                command: None,
                missing: Vec::new(),
                setup_hint: preset
                    .map_or(spec.setup_hint, |p| p.setup_hint.as_str())
                    .to_string(),
                setup_url: preset.map(|p| p.setup_url.clone()),
                status: None,
                install_supported: false,
                update_supported: false,
                login_command: None,
            };
            let (worker, status) = match spec.kind {
                HarnessKind::Pi => {
                    let (tools, status, managed) = pi_tools(app_data);
                    harness.missing = npm_missing(&tools, "pi", "buzz-pi-acp");
                    harness.install_supported = crate::install::supported();
                    harness.update_supported = managed
                        && status == HarnessStatus::Ready
                        && !crate::install::current(app_data);
                    (tools.adapter, status)
                }
                HarnessKind::Claude => {
                    let setup = claude_setup(app_data);
                    harness.missing = npm_missing(&setup.tools, "claude", "claude-agent-acp");
                    harness.install_supported = crate::install::supported();
                    harness.login_command = setup
                        .tools
                        .cli
                        .as_deref()
                        .map(|cli| login_command(cli, setup.node_bin.as_deref()));
                    (setup.tools.adapter, setup.status)
                }
                _ => {
                    harness.missing = spec
                        .requires
                        .iter()
                        .filter(|name| installed(name).is_none())
                        .map(|name| name.to_string())
                        .collect();
                    let status = if harness.missing.is_empty() {
                        HarnessStatus::Ready
                    } else {
                        HarnessStatus::CliNeeded
                    };
                    (installed(spec.requires[0]), status)
                }
            };
            // Without the listener nothing runs, whatever the harness has.
            if listener_missing {
                harness.missing.insert(0, LISTENER.into());
            }
            harness.status = Some(if listener_missing {
                HarnessStatus::CliNeeded
            } else {
                status
            });
            let command = match (spec.named_command, &worker) {
                (Some(name), Some(_)) => Some(name.to_string()),
                (None, Some(path)) => Some(path.to_string_lossy().into_owned()),
                (_, None) => None,
            };
            harness.command = harness.missing.is_empty().then_some(command).flatten();
            harness
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(cli: bool, adapter: bool, node: bool, root: &str) -> NpmTools {
        let path = |name: &str, on: bool| on.then(|| PathBuf::from(format!("{root}/{name}")));
        NpmTools {
            cli: path("cli", cli),
            adapter: path("adapter", adapter),
            node: path("node", node),
        }
    }

    #[test]
    fn a_complete_user_install_wins_over_the_hosts_own() {
        let (chosen, status, managed) = npm_choice(
            tools(true, true, true, "/usr"),
            tools(true, true, true, "/data"),
        );
        assert_eq!(status, HarnessStatus::Ready);
        assert!(!managed);
        assert_eq!(chosen.adapter, Some(PathBuf::from("/usr/adapter")));
    }

    #[test]
    fn the_hosts_own_install_fills_in_for_an_incomplete_user_one() {
        let (chosen, status, managed) = npm_choice(
            tools(true, false, true, "/usr"),
            tools(true, true, true, "/data"),
        );
        assert_eq!(status, HarnessStatus::Ready);
        assert!(managed);
        assert_eq!(chosen.node, Some(PathBuf::from("/data/node")));
        // Its CLI falls back to the user's.
        let (chosen, _, managed) = npm_choice(
            tools(true, false, false, "/usr"),
            tools(false, true, true, "/data"),
        );
        assert!(managed);
        assert_eq!(chosen.cli, Some(PathBuf::from("/usr/cli")));
    }

    #[test]
    fn status_names_what_is_missing() {
        let none = NpmTools::default;
        let status = |user| npm_choice(user, none()).1;
        assert_eq!(
            status(tools(false, true, true, "/usr")),
            HarnessStatus::CliNeeded
        );
        assert_eq!(
            status(tools(true, true, false, "/usr")),
            HarnessStatus::CliNeeded
        );
        assert_eq!(
            status(tools(true, false, true, "/usr")),
            HarnessStatus::AdapterNeeded
        );
        assert_eq!(
            npm_missing(&tools(true, false, false, "/usr"), "pi", "buzz-pi-acp"),
            ["buzz-pi-acp", "node"]
        );
    }

    /// A fake `claude` answering `auth status` with `json` and `code`.
    #[cfg(unix)]
    async fn auth(json: &str, code: u8) -> Option<bool> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("claude");
        let source = dir.path().join("claude.source");
        std::fs::write(
            &source,
            format!("#!/bin/sh\nprintf '%s' '{json}'\nexit {code}\n"),
        )
        .unwrap();
        // Copied by a child process, so no test thread's fork holds the
        // executable open for writing (ETXTBSY).
        assert!(
            std::process::Command::new("cp")
                .arg(&source)
                .arg(&cli)
                .status()
                .unwrap()
                .success()
        );
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        claude_signed_in(&cli, std::ffi::OsStr::new("/usr/bin:/bin")).await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_sign_in_is_confirmed_only_by_a_matching_exit_code() {
        assert_eq!(auth(r#"{"loggedIn":true}"#, 0).await, Some(true));
        assert_eq!(auth(r#"{"loggedIn":false}"#, 1).await, Some(false));
        assert_eq!(auth(r#"{"loggedIn":true}"#, 1).await, None);
        assert_eq!(auth("not json", 0).await, None);
        assert_eq!(
            claude_signed_in(Path::new("/nonexistent/claude"), std::ffi::OsStr::new("")).await,
            None
        );
    }

    #[test]
    fn login_commands_quote_paths_and_put_the_hosts_node_first() {
        assert_eq!(
            login_command(Path::new("/usr/bin/claude"), None),
            "'/usr/bin/claude' auth login"
        );
        assert_eq!(
            login_command(
                Path::new("/data/claude-tools/bin/claude"),
                Some(Path::new("/data/it's/node/bin"))
            ),
            "PATH='/data/it'\\''s/node/bin':\"$PATH\" '/data/claude-tools/bin/claude' auth login"
        );
    }
}
