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
