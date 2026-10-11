//! The harnesses agents on this host can use, as buzz-app lists them
//! (`src-tauri/src/agents.rs`, `harness_options`).
//!
//! Discovery uses the controller's tool PATH (the user's login-shell PATH
//! plus common install locations), the same PATH agents launch with. Pi and
//! Claude Code may also be installed into the host's own tools, under its
//! state directory; a complete user install always wins over those. Codex
//! runs the user's own `codex` CLI through an ACP adapter, which may be the
//! host's own.

use crate::api::{
    ClaudeSetup, CodexSetup, ConfigurationPolicy, Harness, HarnessKind, HarnessOption,
    HarnessStatus, ProviderOption, SelectorEnvironment,
};
use buzz_agent_controller::{
    HarnessConfigurationPolicy, HarnessIntegration, installed, installed_npm_tool, managed_tool,
};
/// The preset catalog (Hermes, Claude Code): labels, setup guides and hints
/// for harnesses Buzz does not configure itself.
pub use buzz_agent_controller::{HarnessPreset, harness_presets};
use std::path::{Path, PathBuf};

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
/// Common IDs checked against Goose's provider registry; custom IDs remain
/// editable.
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
        HarnessKind::Claude | HarnessKind::Hermes | HarnessKind::Codex => &[],
    }
}

/// Where the host installs Pi, Claude Code and their Node.js: buzz-app's
/// app data directory.
pub fn tools_root(state_dir: &Path) -> &Path {
    state_dir
}

/// Pi, Claude Code and Codex adapter installs are supported here.
pub const INSTALL_SUPPORTED: bool = cfg!(all(
    any(target_os = "macos", target_os = "linux"),
    any(target_arch = "x86_64", target_arch = "aarch64")
));

fn provider_options(list: &[Provider]) -> Vec<ProviderOption> {
    list.iter()
        .map(|provider| ProviderOption {
            value: provider.id.to_string(),
            label: provider.label.to_string(),
        })
        .collect()
}

/// Converts through JSON names so policy enums added upstream stay strings.
fn convert_policy(policy: HarnessConfigurationPolicy) -> Option<ConfigurationPolicy> {
    let json = serde_json::to_value(policy).ok()?;
    let text = |key: &str| json.get(key)?.as_str().map(str::to_string);
    Some(ConfigurationPolicy {
        authentication: text("authentication")?,
        provider: text("provider")?,
        model: text("model")?,
        selector_environment: policy.selector_environment.map(|selector| {
            let json = serde_json::to_value(selector).unwrap_or_default();
            SelectorEnvironment {
                model: json["model"].as_str().unwrap_or_default().to_string(),
                provider: json["provider"].as_str().unwrap_or_default().to_string(),
            }
        }),
        supported_modes: json["supportedModes"]
            .as_array()
            .map(|modes| {
                modes
                    .iter()
                    .filter_map(|mode| mode.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        effort_discovery: text("effortDiscovery").unwrap_or_default(),
    })
}

fn policy(command: &str) -> Option<ConfigurationPolicy> {
    convert_policy(HarnessConfigurationPolicy::for_command(command))
}

/// The policy of a native-owned integration, whatever its command.
fn integration_policy(integration: HarnessIntegration) -> Option<ConfigurationPolicy> {
    convert_policy(HarnessConfigurationPolicy::for_integration(integration))
}

/// The integration's wire name.
fn integration_id(integration: HarnessIntegration) -> Option<String> {
    serde_json::to_value(integration)
        .ok()?
        .as_str()
        .map(str::to_string)
}

/// The id of Codex's harness option.
pub const CODEX: &str = buzz_agent_controller::codex::INTEGRATION_ID;

fn npm_status(cli: bool, adapter: bool, node: bool) -> &'static str {
    if !cli || !node {
        "cli-needed"
    } else if !adapter {
        "adapter-needed"
    } else {
        "ready"
    }
}

struct NpmTools {
    cli: Option<PathBuf>,
    adapter: Option<PathBuf>,
    node: Option<PathBuf>,
}

/// An existing, complete user install always wins. Otherwise the host's
/// own pair is used when its pinned Node can run its npm shims.
fn npm_choice(user: NpmTools, managed: NpmTools) -> (Option<PathBuf>, &'static str, bool) {
    let user_ready = user.cli.is_some() && user.adapter.is_some() && user.node.is_some();
    let managed_selected = !user_ready && managed.adapter.is_some() && managed.node.is_some();
    let selected = if user_ready {
        user
    } else if managed_selected {
        NpmTools {
            cli: managed.cli.or(user.cli),
            ..managed
        }
    } else {
        user
    };
    let status = npm_status(
        selected.cli.is_some(),
        selected.adapter.is_some(),
        selected.node.is_some(),
    );
    (selected.adapter, status, managed_selected)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn pi_current(app_data: &Path) -> bool {
    buzz_desktop::managed_npm::current(app_data)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn pi_current(_: &Path) -> bool {
    true
}

/// Claude Code's tools and how to sign in to it.
pub struct Claude {
    pub setup: ClaudeSetup,
    pub cli: Option<PathBuf>,
    pub node: Option<PathBuf>,
    pub adapter: Option<PathBuf>,
}

pub fn claude(app_data: &Path) -> Claude {
    let cli = installed_npm_tool("claude");
    let managed_cli = managed_tool(app_data, "claude");
    let managed_node = managed_tool(app_data, "node");
    let (adapter, status, managed) = npm_choice(
        NpmTools {
            cli: cli.clone(),
            adapter: installed_npm_tool("claude-agent-acp"),
            node: installed_npm_tool("node"),
        },
        NpmTools {
            cli: managed_cli.clone(),
            adapter: managed_tool(app_data, "claude-agent-acp"),
            node: managed_node.clone(),
        },
    );
    let cli = if managed { managed_cli.or(cli) } else { cli };
    let node = managed.then_some(managed_node).flatten();
    let login_command =
        cli.as_ref()
            .map(|cli| match node.as_ref().and_then(|node| node.parent()) {
                Some(node_bin) => format!(
                    "PATH={}:\"$PATH\" {} auth login",
                    quote(node_bin),
                    quote(cli)
                ),
                None => format!("{} auth login", quote(cli)),
            });
    Claude {
        setup: ClaudeSetup {
            status: status.into(),
            install_supported: INSTALL_SUPPORTED,
            login_command,
        },
        cli,
        node,
        adapter,
    }
}

/// A path as one single-quoted shell word.
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Codex's tools: the user's CLI, and its ACP adapter (theirs, else the
/// host's own), and how to sign in.
pub struct Codex {
    pub setup: CodexSetup,
    pub cli: Option<PathBuf>,
    pub adapter: Option<PathBuf>,
}

pub fn codex(app_data: &Path) -> Codex {
    let cli = installed("codex");
    Codex {
        setup: CodexSetup {
            login_command: cli.as_deref().map(|cli| format!("{} login", quote(cli))),
        },
        adapter: buzz_agent_controller::codex::installed_adapter(Some(app_data)),
        cli,
    }
}

/// What Codex still needs. Codex agents run on Unix only for now.
fn codex_status(cli: bool, adapter: bool) -> &'static str {
    if !cfg!(unix) {
        "not-enabled"
    } else if !cli {
        "cli-needed"
    } else if !adapter {
        "adapter-needed"
    } else {
        "ready"
    }
}

/// Editing suggestions and executable presence only; availability does not
/// establish provider credentials or an ACP session.
pub fn options(app_data: &Path) -> Vec<HarnessOption> {
    let (pi, pi_status, pi_managed) = npm_choice(
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
    );
    let mut options = vec![
        HarnessOption {
            id: integration_id(HarnessIntegration::BuzzAgent),
            command: "buzz-agent".into(),
            label: "Buzz Agent".into(),
            available: true,
            status: "ready".into(),
            providers: provider_options(BUZZ_AGENT_PROVIDERS),
            configuration_policy: policy("buzz-agent"),
            ..Default::default()
        },
        HarnessOption {
            id: integration_id(HarnessIntegration::Goose),
            command: "goose".into(),
            label: "Goose".into(),
            available: true,
            status: "ready".into(),
            providers: provider_options(GOOSE_PROVIDERS),
            configuration_policy: policy("goose"),
            ..Default::default()
        },
        HarnessOption {
            id: integration_id(HarnessIntegration::Pi),
            command: pi.map_or_else(
                || "buzz-pi-acp".into(),
                |path| path.to_string_lossy().into_owned(),
            ),
            label: "Pi".into(),
            available: pi_status == "ready",
            status: pi_status.into(),
            install_supported: Some(INSTALL_SUPPORTED),
            update_supported: Some(pi_managed && pi_status == "ready" && !pi_current(app_data)),
            // Pi reports signed-in providers through its model catalog.
            providers: Vec::new(),
            configuration_policy: policy("buzz-pi-acp"),
            ..Default::default()
        },
    ];
    options.extend(
        harness_presets()
            .iter()
            .filter(|preset| preset.id != "claude")
            .map(|preset| {
                let command = installed(&preset.command);
                HarnessOption {
                    id: integration_id(HarnessIntegration::External),
                    configuration_policy: integration_policy(HarnessIntegration::External),
                    available: command.is_some(),
                    status: if command.is_some() {
                        "ready"
                    } else {
                        "cli-needed"
                    }
                    .into(),
                    command: command.map_or_else(
                        || preset.command.clone(),
                        |path| path.to_string_lossy().into_owned(),
                    ),
                    label: preset.label.clone(),
                    install_supported: Some(false),
                    update_supported: Some(false),
                    default_args: preset.args.clone(),
                    providers: Vec::new(),
                }
            }),
    );
    let claude = claude(app_data);
    options.push(HarnessOption {
        id: integration_id(HarnessIntegration::External),
        configuration_policy: integration_policy(HarnessIntegration::External),
        command: claude.adapter.map_or_else(
            || "claude-agent-acp".into(),
            |path| path.to_string_lossy().into_owned(),
        ),
        label: "Claude Code".into(),
        available: claude.setup.status == "ready",
        status: claude.setup.status,
        install_supported: Some(claude.setup.install_supported),
        update_supported: Some(false),
        default_args: Vec::new(),
        providers: Vec::new(),
    });
    let codex = codex(app_data);
    let codex_status = codex_status(codex.cli.is_some(), codex.adapter.is_some());
    options.push(HarnessOption {
        id: Some(CODEX.into()),
        command: codex.adapter.map_or_else(
            || "codex-acp".into(),
            |path| path.to_string_lossy().into_owned(),
        ),
        label: "Codex".into(),
        // As for Claude Code: executable presence, not sign-in.
        available: codex_status == "ready",
        status: codex_status.into(),
        install_supported: Some(INSTALL_SUPPORTED),
        update_supported: None,
        default_args: Vec::new(),
        providers: Vec::new(),
        configuration_policy: integration_policy(HarnessIntegration::Codex),
    });
    options
}

/// The harness kind a command runs, by its executable name. `None` for a
/// custom executable.
pub fn kind_of(command: &str) -> Option<HarnessKind> {
    let name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
        .trim_end_matches(".exe");
    match name {
        "buzz-agent" => Some(HarnessKind::BuzzAgent),
        "goose" | "goose-acp" => Some(HarnessKind::Goose),
        "buzz-pi-acp" => Some(HarnessKind::Pi),
        "claude-agent-acp" => Some(HarnessKind::Claude),
        "hermes-acp" => Some(HarnessKind::Hermes),
        _ => None,
    }
}

/// A harness option's status.
pub fn status(option: &HarnessOption) -> Option<HarnessStatus> {
    match option.status.as_str() {
        "ready" => Some(HarnessStatus::Ready),
        "cli-needed" => Some(HarnessStatus::CliNeeded),
        "adapter-needed" => Some(HarnessStatus::AdapterNeeded),
        "check-needed" => Some(HarnessStatus::CheckNeeded),
        "not-enabled" => Some(HarnessStatus::NotEnabled),
        _ => None,
    }
}

/// An option's harness kind: Codex by its id, never by its command.
fn option_kind(option: &HarnessOption) -> Option<HarnessKind> {
    if option.id.as_deref() == Some(CODEX) {
        return Some(HarnessKind::Codex);
    }
    kind_of(&option.command)
}

/// A preset harness (Hermes, Claude Code): its own model and sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    pub id: String,
    pub label: String,
    pub setup_hint: String,
    pub setup_url: String,
}

/// The preset `command` runs, by executable name (buzz-app's
/// `harnessPreset`).
pub fn preset(command: &str) -> Option<Preset> {
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let name = [".exe", ".cmd", ".bat"]
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .unwrap_or(name);
    harness_presets()
        .iter()
        .find(|preset| preset.command == name)
        .map(|preset| Preset {
            id: preset.id.clone(),
            label: preset.label.clone(),
            setup_hint: preset.setup_hint.clone(),
            setup_url: preset.setup_url.clone(),
        })
}

/// Every preset, for setup guides.
pub fn presets() -> Vec<Preset> {
    harness_presets()
        .iter()
        .map(|preset| Preset {
            id: preset.id.clone(),
            label: preset.label.clone(),
            setup_hint: preset.setup_hint.clone(),
            setup_url: preset.setup_url.clone(),
        })
        .collect()
}

/// The options as a harness list, which Settings shows. `claude` and
/// `codex` add their sign-in commands.
pub fn legacy(
    options: &[HarnessOption],
    claude: Option<&ClaudeSetup>,
    codex: Option<&CodexSetup>,
) -> Vec<Harness> {
    options
        .iter()
        .filter_map(|option| {
            let kind = option_kind(option)?;
            let preset = buzz_agent_controller::harness_preset(&option.command);
            let missing = match option.status.as_str() {
                "ready" => Vec::new(),
                "adapter-needed" => vec!["ACP adapter".into()],
                _ => vec!["CLI".into()],
            };
            Some(Harness {
                kind,
                label: option.label.clone(),
                command: option.available.then(|| option.command.clone()),
                missing,
                setup_hint: preset.map(|p| p.setup_hint.clone()).unwrap_or_default(),
                setup_url: preset.map(|p| p.setup_url.clone()),
                status: status(option),
                install_supported: option.install_supported == Some(true),
                update_supported: option.update_supported == Some(true),
                login_command: match kind {
                    HarnessKind::Claude => claude.and_then(|setup| setup.login_command.clone()),
                    HarnessKind::Codex => codex.and_then(|setup| setup.login_command.clone()),
                    _ => None,
                },
            })
        })
        .collect()
}

/// The list as [`Request::Harnesses`](crate::api::Request::Harnesses)
/// answers it, for GUIs from before harness options: without Codex or the
/// statuses those GUIs cannot read.
pub fn for_older_guis(mut list: Vec<Harness>) -> Vec<Harness> {
    list.retain(|harness| harness.kind != HarnessKind::Codex);
    for harness in &mut list {
        if matches!(
            harness.status,
            Some(HarnessStatus::CheckNeeded | HarnessStatus::NotEnabled)
        ) {
            harness.status = None;
        }
    }
    list
}

/// Suggested effort levels (buzz-app's `effortChoices`); not evidence of
/// what a model supports.
pub fn efforts(kind: HarnessKind) -> &'static [&'static str] {
    match kind {
        HarnessKind::BuzzAgent => &["none", "minimal", "low", "medium", "high", "xhigh", "max"],
        HarnessKind::Goose => &["off", "low", "medium", "high", "max"],
        HarnessKind::Pi => &["off", "minimal", "low", "medium", "high", "xhigh"],
        HarnessKind::Claude | HarnessKind::Hermes | HarnessKind::Codex => &[],
    }
}

/// The name agent defaults use for a harness; only these three can be a
/// default.
pub fn defaults_name(kind: HarnessKind) -> Option<&'static str> {
    match kind {
        HarnessKind::BuzzAgent => Some("buzz-agent"),
        HarnessKind::Goose => Some("goose"),
        HarnessKind::Pi => Some("pi"),
        HarnessKind::Claude | HarnessKind::Hermes | HarnessKind::Codex => None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(cli: bool, adapter: bool, node: bool, root: &str) -> NpmTools {
        let path = |name: &str| Some(PathBuf::from(format!("{root}/{name}")));
        NpmTools {
            cli: cli.then(|| path("cli")).flatten(),
            adapter: adapter.then(|| path("adapter")).flatten(),
            node: node.then(|| path("node")).flatten(),
        }
    }

    #[test]
    fn a_complete_user_install_wins_over_the_host_tools() {
        let (adapter, status, managed) = npm_choice(
            tools(true, true, true, "/user"),
            tools(true, true, true, "/host"),
        );
        assert_eq!(adapter, Some("/user/adapter".into()));
        assert_eq!(status, "ready");
        assert!(!managed);
    }

    #[test]
    fn host_tools_fill_in_an_incomplete_user_install() {
        let (adapter, status, managed) = npm_choice(
            tools(true, false, true, "/user"),
            tools(false, true, true, "/host"),
        );
        assert_eq!(adapter, Some("/host/adapter".into()));
        // The user's CLI runs with the host's adapter and Node.
        assert_eq!(status, "ready");
        assert!(managed);
    }

    #[test]
    fn statuses_name_what_is_missing() {
        assert_eq!(npm_status(false, true, true), "cli-needed");
        assert_eq!(npm_status(true, true, false), "cli-needed");
        assert_eq!(npm_status(true, false, true), "adapter-needed");
        assert_eq!(npm_status(true, true, true), "ready");
    }

    #[test]
    fn options_list_buzz_apps_harnesses_in_order() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path());
        let labels: Vec<_> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Buzz Agent",
                "Goose",
                "Pi",
                "Hermes Agent",
                "Claude Code",
                "Codex"
            ]
        );
        let ids: Vec<_> = options.iter().map(|o| o.id.as_deref().unwrap()).collect();
        assert_eq!(
            ids,
            ["buzz-agent", "goose", "pi", "external", "external", "codex"]
        );
        assert_eq!(options[0].providers[0].value, "databricks_v2");
        assert!(options[0].configuration_policy.is_some());
        assert_eq!(
            options[1]
                .configuration_policy
                .as_ref()
                .unwrap()
                .authentication,
            "harnessWithOverrides"
        );
        assert_eq!(kind_of(&options[2].command), Some(HarnessKind::Pi));
    }

    #[test]
    fn codex_needs_its_cli_then_its_adapter() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path());
        let codex = options.iter().find(|o| o.label == "Codex").unwrap();
        let tools = self::codex(root.path());
        let status = codex_status(tools.cli.is_some(), tools.adapter.is_some());
        assert_eq!(codex.status, status);
        assert_eq!(codex.available, status == "ready");
        assert_eq!(codex.install_supported, Some(INSTALL_SUPPORTED));
        assert_eq!(codex.update_supported, None);
        if tools.adapter.is_none() {
            assert_eq!(codex.command, "codex-acp");
        }
        let policy = codex.configuration_policy.as_ref().unwrap();
        assert_eq!(
            (
                policy.authentication.as_str(),
                policy.provider.as_str(),
                policy.model.as_str(),
                policy.effort_discovery.as_str(),
            ),
            ("external", "external", "optional", "modelSpecific")
        );
        assert_eq!(policy.supported_modes, ["default", "advanced"]);
        assert!(policy.selector_environment.is_none());
        // Other integrations admit no saved modes.
        assert!(
            options[0]
                .configuration_policy
                .as_ref()
                .unwrap()
                .supported_modes
                .is_empty()
        );
        if cfg!(unix) {
            assert_eq!(codex_status(false, true), "cli-needed");
            assert_eq!(codex_status(true, false), "adapter-needed");
            assert_eq!(codex_status(true, true), "ready");
        } else {
            assert_eq!(codex_status(true, true), "not-enabled");
        }
    }

    #[test]
    fn codex_signs_in_with_its_quoted_cli() {
        let root = tempfile::tempdir().unwrap();
        let tools = codex(root.path());
        assert_eq!(
            tools.setup.login_command,
            tools
                .cli
                .as_deref()
                .map(|cli| format!("{} login", quote(cli)))
        );
        assert_eq!(quote(Path::new("/opt/it's/codex")), "'/opt/it'\\''s/codex'");
    }

    #[test]
    fn codex_is_known_by_its_id_not_its_command() {
        let option = |id: Option<&str>| HarnessOption {
            id: id.map(str::to_string),
            command: "/tools/codex-acp".into(),
            label: "Codex".into(),
            status: "not-enabled".into(),
            ..Default::default()
        };
        assert_eq!(option_kind(&option(Some(CODEX))), Some(HarnessKind::Codex));
        assert_eq!(option_kind(&option(Some("external"))), None);
        assert_eq!(option_kind(&option(None)), None);
        let setup = CodexSetup {
            login_command: Some("'codex' login".into()),
        };
        let list = legacy(&[option(Some(CODEX))], None, Some(&setup));
        assert_eq!(list[0].kind, HarnessKind::Codex);
        assert_eq!(list[0].status, Some(HarnessStatus::NotEnabled));
        assert_eq!(list[0].login_command.as_deref(), Some("'codex' login"));
    }

    #[test]
    fn older_guis_never_get_codex_or_new_statuses() {
        let root = tempfile::tempdir().unwrap();
        let mut options = options(root.path());
        options[1].status = "check-needed".into();
        let list = for_older_guis(legacy(&options, None, None));
        assert!(list.iter().all(|h| h.kind != HarnessKind::Codex));
        let goose = list.iter().find(|h| h.kind == HarnessKind::Goose).unwrap();
        assert_eq!(goose.status, None);
        // What an older GUI decodes: only the kinds and statuses it knew.
        let wire = rmp_serde::to_vec_named(&list).unwrap();
        let value: serde_json::Value = rmp_serde::from_slice(&wire).unwrap();
        for harness in value.as_array().unwrap() {
            assert!(
                ["buzzagent", "goose", "claude", "pi", "hermes"]
                    .contains(&harness["kind"].as_str().unwrap())
            );
            assert!(matches!(
                harness["status"].as_str(),
                None | Some("ready" | "cli-needed" | "adapter-needed")
            ));
        }
    }

    #[test]
    fn legacy_harnesses_carry_status_and_sign_in() {
        let root = tempfile::tempdir().unwrap();
        let options = options(root.path());
        let setup = ClaudeSetup {
            status: "ready".into(),
            install_supported: true,
            login_command: Some("'claude' auth login".into()),
        };
        let list = legacy(&options, Some(&setup), None);
        let find = |kind| list.iter().find(|h| h.kind == kind).unwrap();
        assert_eq!(
            find(HarnessKind::BuzzAgent).status,
            Some(HarnessStatus::Ready)
        );
        assert_eq!(
            find(HarnessKind::Claude).login_command.as_deref(),
            Some("'claude' auth login")
        );
        assert_eq!(find(HarnessKind::Goose).login_command, None);
        assert_eq!(find(HarnessKind::Pi).install_supported, INSTALL_SUPPORTED);
    }
}
