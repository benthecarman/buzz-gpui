//! Model catalogs for the agent form, ported from buzz-app's
//! `src-tauri/src/{goose_models,pi_models}.rs`.
//!
//! Each lookup runs the harness once, with a cleared environment holding
//! only the agent's own variables, and kills its whole process group when
//! done or timed out. Environment values never leave the host: only model
//! IDs come back.

use crate::api::{ModelCatalog, ModelOption};
use buzz_agent_controller::GooseModelContext;
use buzz_agent_controller::pi::PiContext;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Which catalog a harness command uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Goose,
    Pi,
    /// Buzz Agent lists models only for Databricks, through an OAuth
    /// sign-in this host does not run.
    Databricks,
    None,
}

pub fn lookup(command: &str) -> Lookup {
    let name = std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim_end_matches(".exe");
    match name {
        "goose" | "goose-acp" => Lookup::Goose,
        "buzz-pi-acp" => Lookup::Pi,
        "buzz-agent" => Lookup::Databricks,
        _ => Lookup::None,
    }
}

const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MODELS: usize = 10_000;
const PI_FAILURE: &str = "Pi models unavailable. Check Pi sign-in and extension configuration, then retry or enter a custom ID";

/// Kills the lookup's process group: extensions may spawn helpers.
struct Probe(tokio::process::Child);

impl Drop for Probe {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.id() {
            // SAFETY: signals a process group this probe created.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.0.start_kill();
    }
}

/// Variables a probe keeps from the host's environment; none are secrets.
const PASSED: &[&str] = &[
    "HOME",
    "TMPDIR",
    "USER",
    "LOGNAME",
    "LANG",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

fn passed(command: &mut tokio::process::Command) {
    for name in PASSED {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn catalog(ids: Vec<String>, model_overridden: bool) -> ModelCatalog {
    ModelCatalog {
        models: ids
            .into_iter()
            .map(|id| ModelOption {
                name: id.clone(),
                id,
            })
            .collect(),
        model_overridden,
    }
}

pub async fn goose(context: GooseModelContext) -> Result<ModelCatalog, String> {
    let overridden = context.model_overridden;
    let response = goose_rpc(
        &context,
        "_goose/unstable/providers/supported-models/list",
        json!({ "providerId": context.provider_id }),
    )
    .await?;
    parse_goose(&response, &context.provider_id).map(|ids| catalog(ids, overridden))
}

async fn goose_rpc(
    context: &GooseModelContext,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut command = tokio::process::Command::new(&context.command);
    command
        .args(&context.args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    passed(&mut command);
    command
        .envs(&context.environment)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    #[cfg(unix)]
    command.process_group(0);
    let mut child = Probe(
        command
            .spawn()
            .map_err(|error| format!("Could not start Goose to list models: {error}"))?,
    );
    let mut stdin = child
        .0
        .stdin
        .take()
        .ok_or("Goose catalog input unavailable")?;
    let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .map_err(|_| "Could not request Goose models")?;
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or("Goose catalog output unavailable")?;
    let mut reader = BufReader::new(stdout.take(MAX_RESPONSE_BYTES + 1));
    let response = tokio::time::timeout(Duration::from_secs(60), async {
        for _ in 0..100 {
            let mut line = String::new();
            if reader
                .read_line(&mut line)
                .await
                .map_err(|_| "Could not read Goose models")?
                == 0
            {
                break;
            }
            let value: Value = serde_json::from_str(&line)
                .map_err(|_| "Goose returned an invalid model response")?;
            if value.get("id") == Some(&json!(1)) {
                return Ok(value);
            }
        }
        Err("Goose did not return a model list".to_string())
    })
    .await
    .map_err(|_| "Goose model lookup timed out; retry".to_string())?;
    drop(stdin);
    drop(child);
    response
}

fn parse_goose(value: &Value, provider: &str) -> Result<Vec<String>, String> {
    if let Some(error) = value.get("error") {
        if error.get("code").and_then(Value::as_i64) == Some(-32000) {
            return Err(
                "Goose needs authentication for this provider. Enter its API key, then retry"
                    .into(),
            );
        }
        return Err("Goose could not list models for this provider. Check its credentials or try again when its API is available".into());
    }
    if value["result"]["providerId"].as_str() != Some(provider) {
        return Err("Goose returned models for a different provider".into());
    }
    let models = value["result"]["models"]
        .as_array()
        .ok_or("Goose returned an invalid model list")?;
    if models.len() > MAX_MODELS {
        return Err("Goose model list is too large to display".into());
    }
    models
        .iter()
        .map(|item| {
            item.as_str()
                .filter(|name| {
                    !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
                })
                .map(str::to_owned)
                .ok_or_else(|| "Goose returned an invalid model name".to_string())
        })
        .collect()
}

type Output = BufReader<tokio::io::Take<tokio::process::ChildStdout>>;

/// Pi in RPC mode, without a saved session.
fn pi_spawn(
    context: &PiContext,
    args: Vec<String>,
) -> Result<(Probe, tokio::process::ChildStdin, Output), String> {
    let mut command = tokio::process::Command::new(&context.command);
    command
        .args(["--mode", "rpc", "--no-session", "--no-themes"])
        .args(args)
        .current_dir(&context.workspace)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    passed(&mut command);
    command
        .envs(&context.environment)
        .env("PATH", &context.path);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = Probe(
        command
            .spawn()
            .map_err(|error| format!("Could not start Pi: {error}"))?,
    );
    let stdin = child.0.stdin.take().ok_or(PI_FAILURE)?;
    let stdout = child.0.stdout.take().ok_or(PI_FAILURE)?;
    Ok((
        child,
        stdin,
        BufReader::new(stdout.take(MAX_RESPONSE_BYTES + 1)),
    ))
}

pub async fn pi(context: PiContext) -> Result<ModelCatalog, String> {
    let context = pi_verify(context).await?;
    let args = context.catalog_args()?;
    let (child, mut stdin, mut reader) = pi_spawn(&context, args)?;
    stdin
        .write_all(b"{\"id\":\"catalog\",\"type\":\"get_available_models\"}\n")
        .await
        .map_err(|_| PI_FAILURE)?;
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        for _ in 0..100 {
            let mut line = String::new();
            if reader.read_line(&mut line).await.map_err(|_| PI_FAILURE)? == 0 {
                break;
            }
            let value: Value = serde_json::from_str(&line).map_err(|_| PI_FAILURE)?;
            if value.get("id") == Some(&json!("catalog")) {
                return parse_pi(&value);
            }
        }
        Err(PI_FAILURE.to_string())
    })
    .await
    .map_err(|_| "Pi model lookup timed out; retry".to_string())?;
    drop(stdin);
    drop(child);
    result.map(|ids| catalog(ids, false))
}

/// Pi before 0.99 has no RPC catalog.
async fn pi_verify(context: PiContext) -> Result<PiContext, String> {
    let mut command = tokio::process::Command::from(context.version_command());
    command.kill_on_drop(true);
    let mut child = Probe(
        command
            .spawn()
            .map_err(|_| context.version_error("Could not verify the Pi CLI version"))?,
    );
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or("Could not verify the Pi CLI version")?;
        let mut output = String::new();
        stdout
            .take(129)
            .read_to_string(&mut output)
            .await
            .map_err(|_| "Could not verify the Pi CLI version")?;
        let status = child
            .0
            .wait()
            .await
            .map_err(|_| "Could not verify the Pi CLI version")?;
        if output.len() > 128 || !status.success() {
            return Err("Could not verify the Pi CLI version");
        }
        Ok(output)
    })
    .await
    .map_err(|_| context.version_error("Pi CLI version check timed out"))?
    .map_err(|error| context.version_error(error))?;
    context
        .accept_version(&output)
        .map(|verified| verified.into_context())
}

fn parse_pi(value: &Value) -> Result<Vec<String>, String> {
    if value["type"] != "response"
        || value["command"] != "get_available_models"
        || value["success"] != true
    {
        return Err(PI_FAILURE.into());
    }
    let models = value["data"]["models"].as_array().ok_or(PI_FAILURE)?;
    if models.len() > MAX_MODELS {
        return Err("Pi model catalog is too large".into());
    }
    let mut ids = Vec::new();
    for model in models {
        let provider = model["provider"].as_str().ok_or(PI_FAILURE)?;
        let id = model["id"].as_str().ok_or(PI_FAILURE)?;
        if provider.is_empty() || id.is_empty() {
            return Err("Pi returned an invalid model ID".into());
        }
        buzz_agent_controller::pi::validate_selection(provider, id)
            .map_err(|_| "Pi returned an invalid model ID")?;
        ids.push(format!("{provider}/{id}"));
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

// Connection tests, ported from buzz-app's `pi_models::test` and
// `goose_models::test`.

const PI_TEST_FAILURE: &str =
    "Connection test failed. Check the provider, model and network, then test again.";
const GOOSE_TEST_FAILURE: &str = "Goose could not complete a request with this provider and model. Check its credentials, model and network, then test again.";
const GOOSE_TEST_TIMEOUT: &str =
    "Goose connection test timed out. Check the network, then test again.";
const GOOSE_CLEANUP_FAILURE: &str = "Goose could not delete its temporary connection-test session. It may remain in Goose history; retry the test.";
const MAX_TEST_BYTES: u64 = 1024 * 1024;

/// Checks Pi's own selection for the provider, then sends one tiny prompt
/// through the agent's setup. Returns `provider/model`.
pub async fn pi_test(context: PiContext, provider: &str, model: &str) -> Result<String, String> {
    if provider.is_empty() {
        return Err("Choose a provider to test".into());
    }
    if model.is_empty() {
        // Pi chooses the model inside the provider's scope; check the
        // provider on its own.
        if provider.len() > 128
            || provider.starts_with('-')
            || provider.contains([',', '/', '*', '?', '[', ']', '{', '}', '\\'])
            || provider.chars().any(char::is_control)
        {
            return Err("Invalid Pi provider ID".into());
        }
    } else {
        buzz_agent_controller::pi::validate_selection(provider, model)?;
    }
    let context = pi_verify(context).await?;
    let mut args = context.catalog_args()?;
    args.extend(
        [
            "--no-tools",
            "--no-context-files",
            "--no-skills",
            "--no-prompt-templates",
            "--system-prompt",
            "Reply with OK.",
        ]
        .map(String::from),
    );
    if model.is_empty() {
        args.extend(["--models".into(), format!("{provider}/*")]);
    } else {
        args.extend(["--model".into(), format!("{provider}/{model}")]);
    }
    let (child, mut stdin, mut reader) = pi_spawn(&context, args)?;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        stdin
            .write_all(b"{\"id\":\"selection\",\"type\":\"get_state\"}\n")
            .await
            .map_err(|_| PI_TEST_FAILURE)?;
        let selection = pi_response(&mut reader, "selection").await?;
        let chosen = &selection["data"]["model"];
        let chosen_id = chosen["id"].as_str().unwrap_or_default().to_string();
        // An empty scope can fall back to another signed-in provider; check
        // before any inference request.
        if chosen["provider"] != provider || chosen_id.is_empty() {
            return Err("No test model available for this provider. Add its API key or sign in with Pi, then test again.".into());
        }
        buzz_agent_controller::pi::validate_selection(provider, &chosen_id)?;
        if !model.is_empty() && chosen_id != model {
            return Err(
                "Pi selected a different model. Check the model ID, then test again.".into(),
            );
        }
        stdin
            .write_all(b"{\"id\":\"test\",\"type\":\"prompt\",\"message\":\"Reply with OK.\"}\n")
            .await
            .map_err(|_| PI_TEST_FAILURE)?;
        for _ in 0..10_000 {
            let mut line = String::new();
            if reader
                .read_line(&mut line)
                .await
                .map_err(|_| PI_TEST_FAILURE)?
                == 0
            {
                break;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(outcome) = pi_reply(&value, provider, &chosen_id) {
                return outcome;
            }
        }
        Err(PI_TEST_FAILURE.into())
    })
    .await
    .map_err(|_| "Connection test timed out. Check the network, then test again.")?;
    drop(stdin);
    drop(child);
    result
}

/// What one Pi RPC line says about the test prompt, if anything.
fn pi_reply(value: &Value, provider: &str, model: &str) -> Option<Result<String, String>> {
    if value["id"] == "test" && value["success"] == false {
        return Some(Err(pi_classify(
            value["error"].as_str().unwrap_or_default(),
        )));
    }
    let message = &value["message"];
    if value["type"] != "message_end" || message["role"] != "assistant" {
        return None;
    }
    if matches!(message["stopReason"].as_str(), Some("error" | "aborted")) {
        return Some(Err(pi_classify(
            message["errorMessage"].as_str().unwrap_or_default(),
        )));
    }
    let replied = message["content"].as_array().is_some_and(|content| {
        content.iter().any(|item| {
            item["type"] == "text"
                && item["text"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty())
        })
    });
    Some(
        if message["provider"] == provider && message["model"] == model && replied {
            Ok(format!("{provider}/{model}"))
        } else {
            Err(PI_TEST_FAILURE.into())
        },
    )
}

async fn pi_response(reader: &mut Output, id: &str) -> Result<Value, String> {
    for _ in 0..100 {
        let mut line = String::new();
        if reader
            .read_line(&mut line)
            .await
            .map_err(|_| PI_TEST_FAILURE)?
            == 0
        {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value["id"] == id {
            return if value["success"] == true {
                Ok(value)
            } else {
                Err(pi_classify(value["error"].as_str().unwrap_or_default()))
            };
        }
    }
    Err(PI_TEST_FAILURE.into())
}

/// Provider errors can echo part of the key, so only fixed text leaves.
fn pi_classify(error: &str) -> String {
    let error = error.to_lowercase();
    let words: Vec<&str> = error.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let any = |codes: &[&str], phrases: &[&str]| {
        codes.iter().any(|code| words.contains(code))
            || phrases.iter().any(|phrase| error.contains(phrase))
    };
    if any(&[], &["no api key"]) {
        "No API key found for this provider. Add its API key, or sign in with Pi."
    } else if any(&["402"], &["quota", "credit", "billing"]) {
        "The provider account is out of credits or quota. Check its billing, then test again."
    } else if any(
        &["401", "403"],
        &[
            "invalid_api_key",
            "api key not valid",
            "api_key_invalid",
            "invalid x-api-key",
            "incorrect api key",
            "authentication",
            "unauthorized",
        ],
    ) {
        "The provider rejected the API key. Check the key, then test again."
    } else if any(&["429"], &["rate limit", "rate_limit", "too many requests"]) {
        "The provider is rate limiting requests. Wait a moment, then test again."
    } else if any(
        &["404"],
        &["model not found", "model_not_found", "does not exist"],
    ) {
        "The provider doesn’t recognize this model. Choose another model."
    } else {
        PI_TEST_FAILURE
    }
    .into()
}

/// Goose with a cleared environment holding only the agent's variables.
fn goose_command(context: &GooseModelContext) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(&context.command);
    command
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    passed(&mut command);
    command
        .envs(&context.environment)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    command
}

/// Runs one Goose turn without tools. `goose-acp` uses a hidden temporary
/// session it deletes afterwards; the CLI saves no session at all.
pub async fn goose_test(mut context: GooseModelContext) -> Result<String, String> {
    if context.model_id.is_empty() {
        // Goose owns each provider's default model.
        let response = goose_rpc(
            &context,
            "_goose/unstable/providers/list",
            json!({ "providerIds": [context.provider_id] }),
        )
        .await?;
        if response.get("error").is_some() {
            return Err(GOOSE_TEST_FAILURE.into());
        }
        context.model_id = response["result"]["entries"]
            .as_array()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry["providerId"] == context.provider_id)
            })
            .and_then(|entry| entry["defaultModel"].as_str())
            .filter(|model| !model.trim().is_empty())
            .ok_or("Goose has no default test model for this provider. Choose a model, then test again.")?
            .to_owned();
    }
    if context.model_id.trim().is_empty()
        || context.model_id.len() > 512
        || context.model_id.chars().any(char::is_control)
    {
        return Err("Choose a valid Goose model to test".into());
    }
    if !context.workspace.is_absolute() || !context.workspace.is_dir() {
        return Err("Choose an existing absolute workspace before testing Goose".into());
    }
    let tested = format!("{}/{}", context.provider_id, context.model_id);
    if context.command.file_stem().and_then(|name| name.to_str()) == Some("goose-acp") {
        return goose_test_acp(context).await.map(|()| tested);
    }
    let mut command = goose_command(&context);
    command
        .args([
            "run",
            "--text",
            "Reply OK.",
            "--no-session",
            "--no-profile",
            "--max-turns",
            "1",
            "--quiet",
            "--output-format",
            "json",
        ])
        .current_dir(&context.workspace)
        .stdin(Stdio::null())
        .env("GOOSE_PROVIDER", &context.provider_id)
        .env("GOOSE_MODEL", &context.model_id);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = Probe(
        command
            .spawn()
            .map_err(|error| format!("Could not start Goose to test the model: {error}"))?,
    );
    let stdout = child.0.stdout.take().ok_or(GOOSE_TEST_FAILURE)?;
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut output = Vec::new();
        stdout
            .take(MAX_TEST_BYTES + 1)
            .read_to_end(&mut output)
            .await
            .map_err(|_| GOOSE_TEST_FAILURE)?;
        if output.len() as u64 > MAX_TEST_BYTES {
            return Err(GOOSE_TEST_FAILURE.into());
        }
        let status = child.0.wait().await.map_err(|_| GOOSE_TEST_FAILURE)?;
        let response: Value = serde_json::from_slice(&output).map_err(|_| GOOSE_TEST_FAILURE)?;
        if status.success() && goose_completed(&response) {
            Ok(tested)
        } else {
            Err(GOOSE_TEST_FAILURE.into())
        }
    })
    .await
    .map_err(|_| GOOSE_TEST_TIMEOUT.to_owned())?
}

/// Goose also reports local provider errors as assistant text and exits
/// zero, so a reply needs token usage from an actual model response.
fn goose_completed(response: &Value) -> bool {
    response["metadata"]["status"] == "completed"
        && response["metadata"]["total_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
        && goose_replied(&response["messages"])
}

fn goose_replied(messages: &Value) -> bool {
    let Some(messages) = messages.as_array() else {
        return false;
    };
    let mut replied = false;
    for content in messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .filter_map(|message| message["content"].as_array())
        .flatten()
    {
        if content["type"] == "error" {
            return false;
        }
        if content["type"] == "text"
            && content["text"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
        {
            replied = true;
        }
    }
    replied
}

/// `goose-acp`'s side of a test: its transport and the session it made.
struct AcpTest {
    _child: Probe,
    input: tokio::process::ChildStdin,
    output: Output,
    line: Vec<u8>,
    session: Option<String>,
}

impl AcpTest {
    async fn send(&mut self, id: u64, method: &str, params: Value) -> Result<(), String> {
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.input
            .write_all(format!("{request}\n").as_bytes())
            .await
            .map_err(|_| GOOSE_TEST_FAILURE.to_owned())
    }

    async fn response(&mut self, id: u64) -> Result<Value, String> {
        loop {
            if self
                .output
                .read_until(b'\n', &mut self.line)
                .await
                .map_err(|_| GOOSE_TEST_FAILURE)?
                == 0
            {
                return Err(GOOSE_TEST_FAILURE.into());
            }
            let line = std::mem::take(&mut self.line);
            let value: Value = serde_json::from_slice(&line).map_err(|_| GOOSE_TEST_FAILURE)?;
            if value.get("id").and_then(Value::as_u64) == Some(id) && value.get("method").is_none()
            {
                if value.get("error").is_some() {
                    return Err(GOOSE_TEST_FAILURE.into());
                }
                return value
                    .get("result")
                    .cloned()
                    .ok_or_else(|| GOOSE_TEST_FAILURE.to_owned());
            }
            if value.get("id").is_some() && value.get("method").is_some() {
                // The test grants no file, terminal or permission requests.
                return Err(GOOSE_TEST_FAILURE.into());
            }
        }
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            self.send(4, "session/delete", json!({ "sessionId": session }))
                .await?;
            self.response(4).await.map(drop)
        })
        .await
        .map_err(|_| GOOSE_CLEANUP_FAILURE.to_owned())?
        .map_err(|_| GOOSE_CLEANUP_FAILURE.to_owned())
    }
}

async fn goose_test_acp(context: GooseModelContext) -> Result<(), String> {
    let mut command = goose_command(&context);
    command
        .current_dir(&context.workspace)
        .stdin(Stdio::piped())
        .env("GOOSE_PROVIDER", &context.provider_id)
        .env("GOOSE_MODEL", &context.model_id)
        .env("GOOSE_MODE", "chat")
        // No developer tools, without changing the user's configuration.
        .env(
            "EXTENSIONS",
            r#"{"developer":{"type":"builtin","name":"developer","enabled":false}}"#,
        );
    #[cfg(unix)]
    command.process_group(0);
    let mut child = Probe(
        command
            .spawn()
            .map_err(|error| format!("Could not start Goose to test the model: {error}"))?,
    );
    let input = child.0.stdin.take().ok_or(GOOSE_TEST_FAILURE)?;
    let output = child.0.stdout.take().ok_or(GOOSE_TEST_FAILURE)?;
    let mut test = AcpTest {
        _child: child,
        input,
        output: BufReader::new(output.take(MAX_TEST_BYTES + 1)),
        line: Vec::new(),
        session: None,
    };
    let workspace = context.workspace.clone();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        test.send(
            1,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientInfo": { "name": "buzz-connection-test", "version": env!("CARGO_PKG_VERSION") },
                "clientCapabilities": {},
            }),
        )
        .await?;
        let initialized = test.response(1).await?;
        if initialized["protocolVersion"] != 1
            || !initialized["agentCapabilities"]["sessionCapabilities"]["delete"].is_object()
        {
            return Err("Goose does not support temporary connection-test session cleanup".into());
        }
        test.send(
            2,
            "session/new",
            json!({
                "cwd": workspace,
                "mcpServers": [],
                "_meta": { "hidden": true, "sessionTitle": "Buzz connection test", "enabledExtensions": [] },
            }),
        )
        .await?;
        let created = test.response(2).await?;
        let session = created["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or(GOOSE_TEST_FAILURE)?
            .to_string();
        test.session = Some(session.clone());
        test.send(
            3,
            "session/prompt",
            json!({ "sessionId": session, "prompt": [{ "type": "text", "text": "Reply OK." }] }),
        )
        .await?;
        let response = test.response(3).await?;
        if !matches!(
            response["stopReason"].as_str(),
            Some("end_turn" | "max_tokens")
        ) {
            return Err(GOOSE_TEST_FAILURE.into());
        }
        test.send(
            5,
            "_goose/unstable/session/export",
            json!({ "sessionId": session, "format": "json" }),
        )
        .await?;
        let exported = test.response(5).await?;
        let saved: Value =
            serde_json::from_str(exported["data"].as_str().ok_or(GOOSE_TEST_FAILURE)?)
                .map_err(|_| GOOSE_TEST_FAILURE)?;
        if saved["id"] == session && goose_replied(&saved["conversation"]) {
            Ok(())
        } else {
            Err(GOOSE_TEST_FAILURE.into())
        }
    })
    .await
    .unwrap_or_else(|_| Err(GOOSE_TEST_TIMEOUT.into()));
    let cleanup = test.cleanup().await;
    cleanup?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_test_replies_are_checked() {
        let ok = json!({ "type": "message_end", "message": {
            "role": "assistant", "provider": "openai", "model": "gpt-5",
            "content": [{ "type": "text", "text": "OK" }] } });
        assert_eq!(
            pi_reply(&ok, "openai", "gpt-5").unwrap().unwrap(),
            "openai/gpt-5"
        );
        assert!(pi_reply(&ok, "openai", "gpt-4").unwrap().is_err());
        let failed = json!({ "type": "message_end", "message": {
            "role": "assistant", "stopReason": "error",
            "errorMessage": "401 Unauthorized: sk-secret" } });
        let error = pi_reply(&failed, "openai", "gpt-5").unwrap().unwrap_err();
        assert!(error.contains("rejected the API key") && !error.contains("sk-secret"));
        assert!(pi_reply(&json!({ "type": "agent_start" }), "openai", "gpt-5").is_none());
        assert!(pi_classify("Rate limit reached (429)").contains("rate limiting"));
        assert!(pi_classify("model_not_found").contains("doesn’t recognize"));
        assert_eq!(pi_classify("socket hang up"), PI_TEST_FAILURE);
    }

    #[test]
    fn goose_test_needs_a_real_reply() {
        let ok = json!({ "metadata": { "status": "completed", "total_tokens": 12 },
            "messages": [{ "role": "assistant", "content": [{ "type": "text", "text": "OK" }] }] });
        assert!(goose_completed(&ok));
        let no_usage = json!({ "metadata": { "status": "completed", "total_tokens": 0 },
            "messages": ok["messages"] });
        assert!(!goose_completed(&no_usage));
        let error = json!({ "metadata": { "status": "completed", "total_tokens": 3 },
            "messages": [{ "role": "assistant", "content": [
                { "type": "text", "text": "Sorry" }, { "type": "error", "text": "bad key" }] }] });
        assert!(!goose_completed(&error));
    }

    /// A fake Goose CLI that replies only when its provider and model come
    /// through the environment.
    #[cfg(unix)]
    #[tokio::test]
    async fn goose_test_runs_one_turn() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let goose = dir.path().join("goose");
        std::fs::write(
            &goose,
            "#!/bin/sh\n[ \"$GOOSE_PROVIDER/$GOOSE_MODEL\" = openai/gpt-test ] || exit 1\nprintf '{\"metadata\":{\"status\":\"completed\",\"total_tokens\":5},\"messages\":[{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"OK\"}]}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&goose, std::fs::Permissions::from_mode(0o755)).unwrap();
        let context = |model: &str| GooseModelContext {
            command: goose.clone(),
            args: vec![],
            workspace: dir.path().into(),
            provider_id: "openai".into(),
            model_id: model.into(),
            environment: Default::default(),
            model_overridden: false,
        };
        assert_eq!(
            goose_test(context("gpt-test")).await.unwrap(),
            "openai/gpt-test"
        );
        assert_eq!(
            goose_test(context("other")).await.unwrap_err(),
            GOOSE_TEST_FAILURE
        );
    }

    #[test]
    fn commands_pick_their_catalog() {
        assert_eq!(lookup("goose"), Lookup::Goose);
        assert_eq!(lookup("/usr/local/bin/goose-acp"), Lookup::Goose);
        assert_eq!(lookup("/opt/node/bin/buzz-pi-acp"), Lookup::Pi);
        assert_eq!(lookup("buzz-agent"), Lookup::Databricks);
        assert_eq!(lookup("/usr/bin/claude-agent-acp"), Lookup::None);
    }

    #[test]
    fn goose_catalogs_are_checked() {
        let ok = json!({ "id": 1, "result": { "providerId": "openai", "models": ["gpt-5", "gpt-5-mini"] } });
        assert_eq!(
            parse_goose(&ok, "openai").unwrap(),
            vec!["gpt-5".to_string(), "gpt-5-mini".to_string()]
        );
        assert!(parse_goose(&ok, "anthropic").is_err(), "wrong provider");
        let auth = json!({ "id": 1, "error": { "code": -32000, "message": "sk-secret leaked" } });
        let error = parse_goose(&auth, "openai").unwrap_err();
        assert!(error.contains("authentication") && !error.contains("sk-secret"));
        let bad = json!({ "id": 1, "result": { "providerId": "openai", "models": ["a\u{7}"] } });
        assert!(parse_goose(&bad, "openai").is_err());
    }

    #[test]
    fn pi_catalogs_are_sorted_provider_ids() {
        let ok = json!({
            "type": "response", "command": "get_available_models", "success": true,
            "data": { "models": [
                { "provider": "openai", "id": "gpt-5" },
                { "provider": "anthropic", "id": "claude-sonnet-4-5" },
                { "provider": "openai", "id": "gpt-5" },
            ] }
        });
        assert_eq!(
            parse_pi(&ok).unwrap(),
            vec![
                "anthropic/claude-sonnet-4-5".to_string(),
                "openai/gpt-5".to_string()
            ]
        );
        let failed =
            json!({ "type": "response", "command": "get_available_models", "success": false });
        assert!(parse_pi(&failed).is_err());
    }

    /// A fake Goose that answers the catalog request with whatever its
    /// environment says, to prove the probe passes only the agent's own
    /// variables and survives a stub executable.
    #[cfg(unix)]
    #[tokio::test]
    async fn goose_probe_runs_the_harness_with_its_environment() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let goose = dir.path().join("goose-acp");
        std::fs::write(
            &goose,
            "#!/bin/sh\nread line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"providerId\":\"openai\",\"models\":[\"%s\"]}}\\n' \"$FAKE_MODEL\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&goose, std::fs::Permissions::from_mode(0o755)).unwrap();
        let context = GooseModelContext {
            command: goose,
            args: vec![],
            workspace: dir.path().into(),
            provider_id: "openai".into(),
            model_id: String::new(),
            environment: [("FAKE_MODEL".to_string(), "gpt-test".to_string())].into(),
            model_overridden: false,
        };
        let catalog = super::goose(context).await.unwrap();
        assert_eq!(catalog.models[0].id, "gpt-test");
    }
}
