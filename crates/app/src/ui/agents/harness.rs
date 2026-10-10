//! Harness logos and how a saved command maps back to a harness.

use buzz_host::api::{Harness, HarnessKind, HarnessStatus};
use buzz_ui::{Brand, BrandMark, Icon, IconName};
use gpui_kit::{AnyElement, IntoElement};

/// The harness logo (Settings → Agents uses 32px; tiles use 14).
pub fn mark(kind: HarnessKind, size: f32) -> AnyElement {
    let brand = match kind {
        HarnessKind::BuzzAgent => return Icon::new(IconName::Robot).size(size).into_any_element(),
        HarnessKind::Goose => BrandMark::Goose,
        HarnessKind::Pi => BrandMark::Pi,
        HarnessKind::Claude => BrandMark::Claude,
        HarnessKind::Hermes => BrandMark::Hermes,
    };
    Brand::new(brand).size(size).into_any_element()
}

/// buzz-app's harness status labels; "Unknown" when the host did not say.
pub fn status_label(status: Option<HarnessStatus>) -> &'static str {
    match status {
        Some(HarnessStatus::Ready) => "Ready",
        Some(HarnessStatus::CliNeeded) => "CLI needed",
        Some(HarnessStatus::AdapterNeeded) => "Adapter needed",
        None => "Unknown",
    }
}

/// Preset harnesses (Claude Code, Hermes) use their own model and sign-in;
/// the controller does not pass a model or provider to them.
pub fn is_preset(kind: HarnessKind) -> bool {
    matches!(kind, HarnessKind::Claude | HarnessKind::Hermes)
}

/// The harness a saved command belongs to: an exact match with what the
/// host detected, else by the executable's name.
pub fn kind_of(command: &str, harnesses: &[Harness]) -> Option<HarnessKind> {
    if let Some(harness) = harnesses
        .iter()
        .find(|harness| harness.command.as_deref() == Some(command))
    {
        return Some(harness.kind);
    }
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    match name {
        "buzz-agent" => Some(HarnessKind::BuzzAgent),
        "goose" | "goose-acp" => Some(HarnessKind::Goose),
        "claude-agent-acp" => Some(HarnessKind::Claude),
        "buzz-pi-acp" => Some(HarnessKind::Pi),
        "hermes-acp" => Some(HarnessKind::Hermes),
        _ => None,
    }
}

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

/// buzz-app's `harnessOptions` providers and API key variables
/// (`src/bundled/agents/agent-edit.ts`).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_map_to_harnesses() {
        let detected = vec![Harness {
            kind: HarnessKind::Pi,
            label: "Pi".into(),
            command: Some("/opt/pi/bin/custom-pi".into()),
            missing: Vec::new(),
            setup_hint: String::new(),
            setup_url: None,
            status: None,
            install_supported: false,
            update_supported: false,
            login_command: None,
        }];
        assert_eq!(
            kind_of("/opt/pi/bin/custom-pi", &detected),
            Some(HarnessKind::Pi)
        );
        assert_eq!(kind_of("buzz-agent", &[]), Some(HarnessKind::BuzzAgent));
        assert_eq!(
            kind_of("/home/me/.npm/bin/claude-agent-acp", &[]),
            Some(HarnessKind::Claude)
        );
        assert_eq!(kind_of("/usr/bin/fake-acp", &[]), None);
    }
}
