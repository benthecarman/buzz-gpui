//! Finds the agent harnesses installed on this machine.
//!
//! Discovery uses the controller's tool PATH (the user's login-shell PATH
//! plus common install locations), the same PATH agents launch with.

use crate::api::{Harness, HarnessKind};
use buzz_agent_controller::{harness_preset, installed};

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

/// Blocks until login-shell PATH discovery finishes (at most ~15 s, once per
/// process).
pub fn detect() -> Vec<Harness> {
    buzz_agent_controller::prepare_tools_path();
    let listener_missing = installed(LISTENER).is_none();
    SPECS
        .iter()
        .map(|spec| {
            let mut missing: Vec<String> = Vec::new();
            if listener_missing {
                missing.push(LISTENER.into());
            }
            let worker = installed(spec.requires[0]);
            missing.extend(
                spec.requires
                    .iter()
                    .filter(|name| installed(name).is_none())
                    .map(|name| name.to_string()),
            );
            let preset = spec.preset.and_then(harness_preset);
            let command = match (spec.named_command, &worker) {
                (Some(name), Some(_)) => Some(name.to_string()),
                (None, Some(path)) => Some(path.to_string_lossy().into_owned()),
                (_, None) => None,
            };
            Harness {
                kind: spec.kind,
                label: preset.map_or(spec.label, |p| p.label.as_str()).to_string(),
                command: missing.is_empty().then_some(command).flatten(),
                missing,
                setup_hint: preset
                    .map_or(spec.setup_hint, |p| p.setup_hint.as_str())
                    .to_string(),
                setup_url: preset.map(|p| p.setup_url.clone()),
            }
        })
        .collect()
}
