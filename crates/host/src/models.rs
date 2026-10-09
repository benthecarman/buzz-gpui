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

pub async fn pi(context: PiContext) -> Result<ModelCatalog, String> {
    let context = pi_verify(context).await?;
    let args = context.catalog_args()?;
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
    let mut stdin = child.0.stdin.take().ok_or(PI_FAILURE)?;
    let stdout = child.0.stdout.take().ok_or(PI_FAILURE)?;
    let mut reader = BufReader::new(stdout.take(MAX_RESPONSE_BYTES + 1));
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

#[cfg(test)]
mod tests {
    use super::*;

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
