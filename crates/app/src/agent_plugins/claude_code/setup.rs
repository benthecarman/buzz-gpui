//! Claude Code on a host: whether it is installed and signed in, and the
//! installer and sign-in, each run as a declared process (buzz-app's
//! `setup.ts`).

use super::claude::env;
use crate::agent_plugins::process::{Process, SpawnOptions, Spawner, collect};
use buzz_host::api::ProcessOutput;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaudeStatus {
    Missing,
    SignedOut {
        version: String,
    },
    Ready {
        version: String,
        account: Option<String>,
    },
    Error {
        message: String,
    },
}

const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const OUTPUT_LIMIT: usize = 20_000;

/// Runs a short process and returns its exit code and output.
async fn run_short(
    spawn: &Spawner,
    id: &str,
    args: &[&str],
) -> Result<(Option<i32>, String, String), String> {
    let mut spawned = spawn
        .spawn(
            id,
            SpawnOptions {
                args: args.iter().map(|arg| arg.to_string()).collect(),
                cwd: None,
                env: env(),
            },
        )
        .await?;
    let process = spawned.process.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(CHECK_TIMEOUT).await;
        let _ = process.kill().await;
    });
    let result = collect(&mut spawned.output).await;
    timer.abort();
    Ok(result)
}

pub async fn check_claude(spawn: &Spawner) -> ClaudeStatus {
    let version = match run_short(spawn, "claude", &["--version"]).await {
        Ok((code, stdout, stderr)) => {
            if code != Some(0) {
                let stderr = stderr.trim();
                return ClaudeStatus::Error {
                    message: if stderr.is_empty() {
                        "Claude Code did not start".into()
                    } else {
                        stderr.into()
                    },
                };
            }
            stdout
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        }
        // The host reports a program it cannot find as a failure to start it.
        Err(message) if message.to_lowercase().contains("could not start") => {
            return ClaudeStatus::Missing;
        }
        Err(message) => return ClaudeStatus::Error { message },
    };
    let Ok((_, stdout, _)) = run_short(spawn, "claude", &["auth", "status", "--json"]).await else {
        return ClaudeStatus::SignedOut { version };
    };
    let Ok(status) = serde_json::from_str::<serde_json::Value>(&stdout) else {
        return ClaudeStatus::SignedOut { version };
    };
    if status.get("loggedIn") != Some(&serde_json::Value::Bool(true)) {
        return ClaudeStatus::SignedOut { version };
    }
    let account = ["email", "orgName"]
        .iter()
        .filter_map(|key| status.get(*key).and_then(serde_json::Value::as_str))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    ClaudeStatus::Ready {
        version,
        account: (!account.is_empty()).then_some(account),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Install,
    Login,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupState {
    pub status: Option<ClaudeStatus>,
    pub checking: bool,
    pub running: Option<Step>,
    /// The running or last step's output.
    pub output: String,
}

/// The one view of Claude Code on a host. Cheap to clone.
#[derive(Clone)]
pub struct ClaudeSetup {
    inner: Arc<Inner>,
}

struct Inner {
    spawn: Option<Spawner>,
    state: watch::Sender<SetupState>,
    process: Mutex<Option<Process>>,
    /// Cancel was asked for before the running step's process was returned.
    cancelled: AtomicBool,
}

/// Why setup cannot run without a host to run it on.
pub const NO_HOST: &str = "Claude Code agents run only in the desktop app";

impl ClaudeSetup {
    pub fn new(spawn: Option<Spawner>) -> Self {
        Self {
            inner: Arc::new(Inner {
                spawn,
                state: watch::Sender::new(SetupState::default()),
                process: Mutex::default(),
                cancelled: AtomicBool::new(false),
            }),
        }
    }

    pub fn snapshot(&self) -> SetupState {
        self.inner.state.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<SetupState> {
        self.inner.state.subscribe()
    }
    fn set(&self, change: impl FnOnce(&mut SetupState)) {
        self.inner.state.send_modify(change);
    }
    fn process(&self) -> std::sync::MutexGuard<'_, Option<Process>> {
        self.inner
            .process
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub async fn check(&self) {
        let Some(spawn) = self.inner.spawn.clone() else {
            self.set(|state| {
                state.status = Some(ClaudeStatus::Error {
                    message: NO_HOST.into(),
                })
            });
            return;
        };
        let mut already = false;
        self.inner.state.send_modify(|state| {
            already = state.checking;
            state.checking = true;
        });
        if already {
            return;
        }
        let status = check_claude(&spawn).await;
        self.set(|state| {
            state.status = Some(status);
            state.checking = false;
        });
    }

    /// Runs the installer or sign-in, then checks again.
    pub async fn run(&self, step: Step) {
        let Some(spawn) = self.inner.spawn.clone() else {
            return;
        };
        let mut already = false;
        self.inner.state.send_modify(|state| {
            already = state.running.is_some();
            if !already {
                state.running = Some(step);
                state.output.clear();
            }
        });
        if already {
            return;
        }
        self.inner.cancelled.store(false, Ordering::SeqCst);
        let (id, args) = match step {
            Step::Install => ("install", vec![]),
            Step::Login => ("claude", vec!["auth".to_string(), "login".to_string()]),
        };
        let spawned = spawn
            .spawn(
                id,
                SpawnOptions {
                    args,
                    cwd: None,
                    env: env(),
                },
            )
            .await;
        match spawned {
            Ok(mut spawned) => {
                *self.process() = Some(spawned.process.clone());
                if self.inner.cancelled.load(Ordering::SeqCst) {
                    let _ = spawned.process.kill().await;
                }
                let mut code = None;
                while let Some(item) = spawned.output.recv().await {
                    match item {
                        ProcessOutput::Stdout(text) | ProcessOutput::Stderr(text) => {
                            self.append(&text)
                        }
                        ProcessOutput::Exit(exit) => {
                            code = exit;
                            break;
                        }
                    }
                }
                if code != Some(0) {
                    let name = match step {
                        Step::Install => "Install",
                        Step::Login => "Sign-in",
                    };
                    let code = code.map_or("stopped".into(), |code| code.to_string());
                    self.append(&format!("\n{name} ended ({code})."));
                }
            }
            Err(error) => self.append(&format!("\n{error}")),
        }
        *self.process() = None;
        self.set(|state| state.running = None);
        self.check().await;
    }

    fn append(&self, text: &str) {
        self.set(|state| {
            state.output.push_str(text);
            let length = state.output.chars().count();
            if length > OUTPUT_LIMIT {
                state.output = state.output.chars().skip(length - OUTPUT_LIMIT).collect();
            }
        });
    }

    /// Sends a line to the running step, such as a sign-in code.
    pub async fn answer(&self, text: &str) {
        let process = self.process().clone();
        if let Some(process) = process {
            let _ = process.write(format!("{text}\n")).await;
        }
    }

    pub async fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        let process = self.process().clone();
        if let Some(process) = process {
            let _ = process.kill().await;
        }
    }
}

#[cfg(test)]
mod tests;
