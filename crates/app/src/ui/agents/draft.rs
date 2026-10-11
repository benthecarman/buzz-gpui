//! An agent's settings as the Add and Edit dialogs hold them: buzz-app's
//! `AgentDraft` (`src/bundled/agents/agent-edit.ts`), the harness policy
//! helpers (`harness-policy.ts`) and the provider key rules of
//! `AgentSettingsFields.tsx`.

use buzz_host::api::{
    Agent, AgentEdit, AgentsSnapshot, AiConfiguration, ConfigurationPolicy, DatabricksSettings,
    HarnessEdit, HarnessOption, SessionPolicy,
};
use buzz_host::harness::CODEX;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draft {
    pub revision: u64,
    pub name: String,
    /// `None` keeps the saved artwork; empty removes it.
    pub picture: Option<String>,
    pub system_prompt: String,
    pub session_policy: Option<SessionPolicy>,
    pub workspace: String,
    /// The harness's native integration (`codex`); `None` for others.
    pub integration: Option<String>,
    pub command: String,
    /// A JSON array of strings, as typed.
    pub args: String,
    pub model: String,
    pub provider: String,
    /// Codex's Default or Advanced choice, as the host sent it.
    pub configuration: Option<AiConfiguration>,
    /// `None` removes a saved variable; values are write-only.
    pub environment: BTreeMap<String, Option<String>>,
    pub databricks: Option<DatabricksSettings>,
}

/// `agentDraft`: the saved settings, with no environment changes.
pub fn from_agent(agent: &Agent) -> Draft {
    Draft {
        revision: agent.revision,
        name: agent.name.clone(),
        picture: None,
        system_prompt: agent.system_prompt.clone(),
        session_policy: agent.session_policy,
        workspace: agent.workspace.clone(),
        integration: agent.harness.integration.clone(),
        command: agent.harness.command.clone(),
        args: serde_json::to_string_pretty(&agent.harness.args).unwrap_or_else(|_| "[]".into()),
        model: agent.harness.model.clone(),
        provider: agent.harness.provider.clone(),
        configuration: agent.harness.configuration.clone(),
        environment: BTreeMap::new(),
        databricks: agent.harness.databricks.clone(),
    }
}

impl Draft {
    /// A native Codex agent. Only the saved integration says so, never the
    /// executable's name.
    pub fn is_codex(&self) -> bool {
        self.integration.as_deref() == Some(CODEX)
    }

    pub fn codex_configuration(&self) -> Option<Configuration> {
        self.configuration.as_ref().and_then(Configuration::parse)
    }

    pub fn set_codex_configuration(&mut self, configuration: Configuration) {
        self.configuration = Some(configuration.to_value());
    }
}

/// Codex's saved choice: the controller's `AiConfiguration`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Configuration {
    /// Codex chooses the model and effort each start.
    Default,
    Advanced(Effort),
}

/// An Advanced effort: one of the model's levels (empty until chosen), or
/// the model reports none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effort {
    Value(String),
    Unsupported,
}

impl Configuration {
    /// `None` for a shape this build does not know.
    pub fn parse(value: &AiConfiguration) -> Option<Self> {
        match value.get("mode")?.as_str()? {
            "default" => Some(Self::Default),
            "advanced" => {
                let effort = value.get("effort")?;
                Some(Self::Advanced(match effort.get("kind")?.as_str()? {
                    "value" => Effort::Value(effort.get("value")?.as_str()?.to_string()),
                    "unsupported" => Effort::Unsupported,
                    _ => return None,
                }))
            }
            _ => None,
        }
    }

    pub fn to_value(&self) -> AiConfiguration {
        match self {
            Self::Default => serde_json::json!({ "mode": "default" }),
            Self::Advanced(Effort::Value(value)) => serde_json::json!({
                "mode": "advanced",
                "effort": { "kind": "value", "value": value },
            }),
            Self::Advanced(Effort::Unsupported) => serde_json::json!({
                "mode": "advanced",
                "effort": { "kind": "unsupported" },
            }),
        }
    }
}

fn executable(command: &str) -> &str {
    command.rsplit(['/', '\\']).next().unwrap_or(command)
}

pub fn is_goose(command: &str) -> bool {
    let name = executable(command);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    matches!(name, "goose" | "goose-acp")
}

pub fn is_pi(command: &str) -> bool {
    executable(command) == "buzz-pi-acp"
}

/// `harnessKind`: `buzz-agent`, `goose`, `pi`, or a preset's id.
pub fn kind(command: &str) -> Option<String> {
    if executable(command) == "buzz-agent" {
        return Some("buzz-agent".into());
    }
    if is_goose(command) {
        return Some("goose".into());
    }
    if is_pi(command) {
        return Some("pi".into());
    }
    buzz_host::harness::preset(command).map(|preset| preset.id)
}

/// `harnessOption`: the integration's option, else the exact command, else
/// the Goose or Pi option.
pub fn option<'a>(
    options: &'a [HarnessOption],
    command: &str,
    integration: Option<&str>,
) -> Option<&'a HarnessOption> {
    integration
        .and_then(|id| {
            options
                .iter()
                .find(|option| option.id.as_deref() == Some(id))
        })
        .or_else(|| options.iter().find(|option| option.command == command))
        .or_else(|| {
            let wanted = kind(command).filter(|kind| kind == "goose" || kind == "pi")?;
            options
                .iter()
                .find(|option| kind(&option.command).as_deref() == Some(&wanted))
        })
}

/// `harnessPolicy`: the host's policy for the integration, else for the
/// command.
pub fn policy<'a>(
    options: &'a [HarnessOption],
    command: &str,
    integration: Option<&str>,
) -> Option<&'a ConfigurationPolicy> {
    if let Some(owned) = integration.and_then(|id| {
        options
            .iter()
            .find(|option| option.id.as_deref() == Some(id))
    }) {
        return owned.configuration_policy.as_ref();
    }
    if let Some(exact) = options.iter().find(|option| option.command == command) {
        return exact.configuration_policy.as_ref();
    }
    let wanted = kind(command)?;
    options
        .iter()
        .find(|option| kind(&option.command).as_deref() == Some(&wanted))
        .and_then(|option| option.configuration_policy.as_ref())
}

/// A provider's API key: its label and environment variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApiKey {
    pub label: &'static str,
    pub env: &'static str,
}

const fn key(label: &'static str, env: &'static str) -> ApiKey {
    ApiKey { label, env }
}

/// Goose provider config keys; OAuth and local providers have none.
pub fn goose_api_key(provider: &str) -> Option<ApiKey> {
    Some(match provider {
        "openai" => key("OpenAI", "OPENAI_API_KEY"),
        "anthropic" => key("Anthropic", "ANTHROPIC_API_KEY"),
        "openrouter" => key("OpenRouter", "OPENROUTER_API_KEY"),
        "google" => key("Google Gemini", "GOOGLE_API_KEY"),
        "groq" => key("Groq", "GROQ_API_KEY"),
        "mistral" => key("Mistral AI", "MISTRAL_API_KEY"),
        "together" => key("Together AI", "TOGETHER_API_KEY"),
        "perplexity" => key("Perplexity", "PERPLEXITY_API_KEY"),
        "cerebras" => key("Cerebras", "CEREBRAS_API_KEY"),
        "custom_deepseek" => key("DeepSeek", "DEEPSEEK_API_KEY"),
        _ => return None,
    })
}

/// Pi providers someone can sign in to from the form; Buzz never inherits
/// keys exported in a shell.
pub const PI_API_KEYS: &[(&str, ApiKey)] = &[
    ("openai", key("OpenAI", "OPENAI_API_KEY")),
    ("anthropic", key("Anthropic", "ANTHROPIC_API_KEY")),
    ("google", key("Google Gemini", "GEMINI_API_KEY")),
    ("openrouter", key("OpenRouter", "OPENROUTER_API_KEY")),
];

pub fn pi_api_key(provider: &str) -> Option<ApiKey> {
    PI_API_KEYS
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, key)| *key)
}

/// The Goose provider in effect: a `GOOSE_PROVIDER` override wins; a saved
/// one hides it (`None`).
pub fn effective_goose_provider(
    provider: &str,
    environment: &BTreeMap<String, Option<String>>,
    saved_keys: &[String],
) -> Option<String> {
    match environment.get("GOOSE_PROVIDER") {
        Some(Some(value)) => Some(value.clone()),
        None if saved_keys.iter().any(|key| key == "GOOSE_PROVIDER") => None,
        _ => Some(provider.to_string()),
    }
}

/// Draft, then agent defaults, then the build floor, as the host resolves
/// Buzz Agent's provider; `None` when a hidden override decides it.
pub fn effective_buzz_provider(
    draft: &Draft,
    saved_keys: &[String],
    data: &AgentsSnapshot,
) -> Option<String> {
    const KEY: &str = "BUZZ_AGENT_PROVIDER";
    match draft.environment.get(KEY) {
        Some(Some(value)) => return Some(value.clone()),
        Some(None) => {}
        None if saved_keys.iter().any(|key| key == KEY) => return None,
        None => {}
    }
    if data
        .default_settings
        .as_ref()
        .is_some_and(|defaults| defaults.environment_keys.iter().any(|key| key == KEY))
    {
        return None;
    }
    let inherited = data
        .default_settings
        .as_ref()
        .filter(|defaults| defaults.harness == "buzz-agent")
        .map(|defaults| defaults.provider.clone())
        .unwrap_or_default();
    [
        draft.provider.clone(),
        inherited,
        data.agent_defaults
            .as_ref()
            .map(|defaults| defaults.provider.clone())
            .unwrap_or_default(),
    ]
    .into_iter()
    .find(|provider| !provider.is_empty())
    .or(Some(String::new()))
}

/// The API key the form asks for, if the draft's provider takes one.
pub fn provider_api_key(
    draft: &Draft,
    saved_keys: &[String],
    data: &AgentsSnapshot,
) -> Option<ApiKey> {
    let options = data.harness_options.as_deref().unwrap_or_default();
    if policy(options, &draft.command, draft.integration.as_deref())
        .is_some_and(|policy| policy.authentication == "external")
    {
        return None;
    }
    if is_pi(&draft.command) {
        return pi_api_key(&draft.provider);
    }
    if kind(&draft.command).as_deref() == Some("buzz-agent") {
        let provider = effective_buzz_provider(draft, saved_keys, data)?;
        return matches!(provider.as_str(), "openai" | "openai-compat")
            .then_some(key("OpenAI", "OPENAI_COMPAT_API_KEY"));
    }
    if !is_goose(&draft.command) {
        return None;
    }
    effective_goose_provider(&draft.provider, &draft.environment, saved_keys)
        .and_then(|provider| goose_api_key(&provider))
}

/// `agentEdit`: the draft as a save, or why it cannot be saved.
/// `model_discovery` builds the draft a model lookup needs, which may lack
/// a name or workspace.
pub fn edit(draft: &Draft, model_discovery: bool) -> Result<AgentEdit, String> {
    if !model_discovery && draft.name.trim().is_empty() {
        return Err("Enter an agent name.".into());
    }
    if draft.command.trim().is_empty() {
        return Err("Enter a harness executable.".into());
    }
    if !model_discovery && draft.workspace.trim().is_empty() {
        return Err("Enter a workspace path.".into());
    }
    if let Some(preset) = buzz_host::harness::preset(&draft.command)
        && (!draft.model.is_empty() || !draft.provider.is_empty())
    {
        return Err(format!(
            "Use {} defaults before saving this agent.",
            preset.label
        ));
    }
    let args: Vec<String> = serde_json::from_str(&draft.args)
        .map_err(|_| "Arguments must be a JSON array of strings.".to_string())?;
    if args.iter().any(|arg| arg.is_empty() || arg.contains(',')) {
        return Err("ACP arguments must be nonempty and cannot contain commas.".into());
    }
    if draft.is_codex() {
        let Some(configuration) = &draft.configuration else {
            return Err("Choose Default or Advanced Codex configuration.".into());
        };
        if !draft.provider.is_empty() {
            return Err("Codex uses the provider configured by its CLI.".into());
        }
        if !args.is_empty() {
            return Err("Codex does not accept custom adapter arguments.".into());
        }
        match Configuration::parse(configuration) {
            Some(Configuration::Default) if !draft.model.is_empty() => {
                return Err("Default Codex configuration cannot select a model.".into());
            }
            Some(Configuration::Advanced(_))
                if !model_discovery && draft.model.trim().is_empty() =>
            {
                return Err("Choose a model for Advanced Codex configuration.".into());
            }
            Some(Configuration::Advanced(Effort::Value(value)))
                if !model_discovery && value.trim().is_empty() =>
            {
                return Err("Choose an effort level for Advanced Codex configuration.".into());
            }
            _ => {}
        }
    }
    Ok(AgentEdit {
        name: draft.name.clone(),
        picture: draft.picture.clone(),
        system_prompt: draft.system_prompt.clone(),
        session_policy: Some(draft.session_policy),
        workspace: draft.workspace.clone(),
        harness: HarnessEdit {
            integration: draft.integration.clone(),
            command: draft.command.clone(),
            args,
            model: draft.model.clone(),
            configuration: draft.configuration.clone(),
            provider: draft.provider.clone(),
            // Codex has no Databricks settings; a hidden stale pair would
            // only fail validation.
            databricks: draft.databricks.clone().filter(|_| !draft.is_codex()),
        },
        environment: draft.environment.clone(),
        effort: None,
    })
}

/// `newAgentDraft`: the agent defaults harness is copied at creation;
/// everything else is inherited at start.
pub fn new_draft(data: &AgentsSnapshot) -> Draft {
    let options = data.harness_options.as_deref().unwrap_or_default();
    let defaults = data.default_settings.as_ref();
    let wanted = defaults
        .map(|defaults| defaults.harness.clone())
        .filter(|harness| !harness.is_empty())
        .unwrap_or_else(|| "buzz-agent".into());
    let chosen = options
        .iter()
        .find(|option| option.available && kind(&option.command).as_deref() == Some(&wanted));
    let command = chosen
        .map(|option| option.command.clone())
        .unwrap_or_else(|| "buzz-agent".into());
    let inherits = defaults.is_some_and(|d| d.harness == "buzz-agent" && !d.provider.is_empty());
    let build_provider = data
        .agent_defaults
        .as_ref()
        .is_some_and(|defaults| !defaults.provider.is_empty());
    let provider = if command != "buzz-agent" || inherits || build_provider {
        String::new()
    } else {
        chosen
            .and_then(|option| option.providers.first())
            .map(|provider| provider.value.clone())
            .unwrap_or_else(|| "databricks_v2".into())
    };
    Draft {
        workspace: data.default_workspace.clone(),
        args: serde_json::to_string(&chosen.map(|o| o.default_args.clone()).unwrap_or_default())
            .unwrap_or_else(|_| "[]".into()),
        command,
        provider,
        ..Default::default()
    }
}

/// The name and instructions a clone copies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seed {
    pub name: String,
    pub system_prompt: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_host::api::ProviderOption;

    fn options() -> Vec<HarnessOption> {
        vec![
            HarnessOption {
                command: "buzz-agent".into(),
                available: true,
                providers: vec![ProviderOption {
                    value: "databricks_v2".into(),
                    label: "Databricks v2".into(),
                }],
                ..Default::default()
            },
            HarnessOption {
                command: "/opt/bin/buzz-pi-acp".into(),
                available: true,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn validation_follows_agent_edit() {
        let draft = Draft {
            name: "Scout".into(),
            workspace: "/w".into(),
            command: "buzz-agent".into(),
            args: "[]".into(),
            ..Default::default()
        };
        assert!(edit(&draft, false).is_ok());
        let missing = |change: fn(&mut Draft)| {
            let mut draft = draft.clone();
            change(&mut draft);
            edit(&draft, false).unwrap_err()
        };
        assert_eq!(missing(|d| d.name.clear()), "Enter an agent name.");
        assert_eq!(
            missing(|d| d.command.clear()),
            "Enter a harness executable."
        );
        assert_eq!(missing(|d| d.workspace.clear()), "Enter a workspace path.");
        assert_eq!(
            missing(|d| d.args = "{".into()),
            "Arguments must be a JSON array of strings."
        );
        assert_eq!(
            missing(|d| d.args = r#"["a,b"]"#.into()),
            "ACP arguments must be nonempty and cannot contain commas."
        );
        assert_eq!(
            missing(|d| {
                d.command = "hermes-acp".into();
                d.model = "x".into();
            }),
            "Use Hermes Agent defaults before saving this agent."
        );
    }

    #[test]
    fn new_drafts_default_to_buzz_agent_with_its_first_provider() {
        let data = AgentsSnapshot {
            harness_options: Some(options()),
            default_workspace: "/w".into(),
            ..Default::default()
        };
        let draft = new_draft(&data);
        assert_eq!(draft.command, "buzz-agent");
        assert_eq!(draft.provider, "databricks_v2");
        assert_eq!(draft.workspace, "/w");
        assert_eq!(draft.args, "[]");
    }

    #[test]
    fn kinds_and_options_match_by_executable() {
        assert_eq!(kind("/x/goose").as_deref(), Some("goose"));
        assert_eq!(kind("claude-agent-acp").as_deref(), Some("claude"));
        assert_eq!(kind("/x/custom"), None);
        let options = options();
        assert_eq!(
            option(&options, "buzz-pi-acp", None).map(|o| o.command.as_str()),
            Some("/opt/bin/buzz-pi-acp")
        );
    }

    fn codex_options() -> Vec<HarnessOption> {
        let mut options = options();
        options.push(HarnessOption {
            id: Some("codex".into()),
            command: "/tools/codex-acp".into(),
            available: true,
            configuration_policy: Some(ConfigurationPolicy {
                authentication: "external".into(),
                provider: "external".into(),
                ..Default::default()
            }),
            ..Default::default()
        });
        options
    }

    #[test]
    fn codex_is_found_by_its_integration_not_its_command() {
        let options = codex_options();
        assert_eq!(
            option(&options, "/saved/codex-acp", Some("codex")).map(|o| o.command.as_str()),
            Some("/tools/codex-acp")
        );
        assert_eq!(option(&options, "/saved/codex-acp", None), None);
        assert_eq!(
            policy(&options, "/saved/codex-acp", Some("codex")).map(|p| p.provider.as_str()),
            Some("external")
        );
        assert_eq!(policy(&options, "/saved/codex-acp", None), None);
    }

    fn codex_draft() -> Draft {
        Draft {
            name: "Coder".into(),
            workspace: "/w".into(),
            integration: Some("codex".into()),
            command: "/tools/codex-acp".into(),
            args: "[]".into(),
            databricks: Some(DatabricksSettings {
                host: "not-an-origin".into(),
                filter: "stale".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn codex_needs_a_complete_mode_without_default_overrides() {
        let mut draft = codex_draft();
        let error = |draft: &Draft| edit(draft, false).unwrap_err();
        assert_eq!(
            error(&draft),
            "Choose Default or Advanced Codex configuration."
        );
        draft.set_codex_configuration(Configuration::Default);
        let saved = edit(&draft, false).unwrap().harness;
        assert_eq!(saved.integration.as_deref(), Some("codex"));
        assert_eq!(
            saved.configuration,
            Some(serde_json::json!({ "mode": "default" }))
        );
        // Hidden Databricks settings are not sent, but stay in the draft.
        assert_eq!(saved.databricks, None);
        assert!(draft.databricks.is_some());

        draft.model = "gpt-5.5".into();
        assert_eq!(
            error(&draft),
            "Default Codex configuration cannot select a model."
        );
        draft.model.clear();
        draft.provider = "openai".into();
        assert_eq!(
            error(&draft),
            "Codex uses the provider configured by its CLI."
        );
        draft.provider.clear();
        draft.args = r#"["--flag"]"#.into();
        assert_eq!(
            error(&draft),
            "Codex does not accept custom adapter arguments."
        );
        draft.args = "[]".into();

        draft.set_codex_configuration(Configuration::Advanced(Effort::Value(String::new())));
        assert_eq!(
            error(&draft),
            "Choose a model for Advanced Codex configuration."
        );
        // A model lookup needs neither yet.
        assert!(edit(&draft, true).is_ok());
        draft.model = "gpt-5.5".into();
        assert_eq!(
            error(&draft),
            "Choose an effort level for Advanced Codex configuration."
        );
        draft.set_codex_configuration(Configuration::Advanced(Effort::Unsupported));
        assert_eq!(
            edit(&draft, false).unwrap().harness.configuration,
            Some(serde_json::json!({
                "mode": "advanced",
                "effort": { "kind": "unsupported" },
            }))
        );
    }

    #[test]
    fn codex_settings_round_trip_through_the_draft() {
        let configuration = serde_json::json!({
            "mode": "advanced",
            "effort": { "kind": "value", "value": "high" },
        });
        let agent = Agent {
            name: "Coder".into(),
            workspace: "/w".into(),
            harness: buzz_host::api::HarnessSettings {
                integration: Some("codex".into()),
                command: "/tools/codex-acp".into(),
                model: "gpt-5.5".into(),
                configuration: Some(configuration.clone()),
                ..Default::default()
            },
            ..Default::default()
        };
        let draft = from_agent(&agent);
        assert!(draft.is_codex());
        assert_eq!(
            draft.codex_configuration(),
            Some(Configuration::Advanced(Effort::Value("high".into())))
        );
        let saved = edit(&draft, false).unwrap().harness;
        assert_eq!(saved.integration.as_deref(), Some("codex"));
        assert_eq!(saved.configuration, Some(configuration));
        // A mode this build does not know is sent back as it came.
        let unknown = serde_json::json!({ "mode": "later" });
        let draft = Draft {
            configuration: Some(unknown.clone()),
            ..draft
        };
        assert_eq!(draft.codex_configuration(), None);
        assert_eq!(
            edit(&draft, false).unwrap().harness.configuration,
            Some(unknown)
        );
    }

    #[test]
    fn provider_keys_follow_the_harness() {
        let data = AgentsSnapshot::default();
        let draft = |command: &str, provider: &str| Draft {
            command: command.into(),
            provider: provider.into(),
            ..Default::default()
        };
        assert_eq!(
            provider_api_key(&draft("buzz-agent", "openai"), &[], &data).map(|k| k.env),
            Some("OPENAI_COMPAT_API_KEY")
        );
        assert_eq!(
            provider_api_key(&draft("buzz-agent", "databricks_v2"), &[], &data),
            None
        );
        assert_eq!(
            provider_api_key(&draft("goose", "anthropic"), &[], &data).map(|k| k.env),
            Some("ANTHROPIC_API_KEY")
        );
        assert_eq!(
            provider_api_key(&draft("buzz-pi-acp", "google"), &[], &data).map(|k| k.env),
            Some("GEMINI_API_KEY")
        );
        // A saved GOOSE_PROVIDER override hides the provider.
        assert_eq!(
            provider_api_key(
                &draft("goose", "anthropic"),
                &["GOOSE_PROVIDER".into()],
                &data
            ),
            None
        );
    }
}
