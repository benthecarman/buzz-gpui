//! A stand-in for the `claude` binary's stream-json protocol, for tests
//! (buzz-app's `claude-testing.ts`).

use crate::agent_plugins::process::{
    Process, ProcessControl, Spawn, SpawnOptions, Spawned, Spawner,
};
use buzz_host::api::ProcessOutput;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::mpsc;

pub struct FakeState {
    pub id: String,
    pub options: SpawnOptions,
    /// Every JSON message written to its stdin.
    pub received: Vec<Value>,
    /// User messages it has taken, in order.
    pub prompts: Vec<String>,
    /// When set, a turn stays open until `finish()`.
    pub hold: bool,
    pub killed: bool,
    session: String,
    resume: Option<String>,
    open: usize,
    started: bool,
    alive: bool,
    output: mpsc::UnboundedSender<ProcessOutput>,
}

/// One fake process. Cheap to clone.
#[derive(Clone)]
pub struct FakeClaude(Arc<Mutex<FakeState>>, Arc<Mutex<HashSet<String>>>);

impl FakeClaude {
    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().unwrap()
    }
    pub fn args(&self) -> Vec<String> {
        self.state().options.args.clone()
    }
    pub fn prompts(&self) -> Vec<String> {
        self.state().prompts.clone()
    }
    pub fn killed(&self) -> bool {
        self.state().killed
    }
    pub fn set_hold(&self, hold: bool) {
        self.state().hold = hold;
    }
    /// The control responses it was sent, in order.
    pub fn responses(&self) -> Vec<Value> {
        self.state()
            .received
            .iter()
            .filter(|message| message["type"] == "control_response")
            .map(|message| message["response"].clone())
            .collect()
    }

    pub fn emit(&self, message: Value) {
        let state = self.state();
        if state.alive {
            let _ = state
                .output
                .send(ProcessOutput::Stdout(format!("{message}\n")));
        }
    }

    pub fn finish(&self, error: Option<&str>) {
        let (session, idle) = {
            let mut state = self.state();
            if state.open == 0 {
                return;
            }
            state.open = 0;
            let idle = state
                .options
                .env
                .get("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS")
                == Some(&Some("1".into()));
            (state.session.clone(), idle)
        };
        let mut result = json!({
            "type": "result",
            "subtype": if error.is_some() { "error_during_execution" } else { "success" },
            "is_error": error.is_some(),
            "session_id": session,
        });
        if let Some(error) = error {
            result["result"] = json!(error);
        }
        self.emit(result);
        // Claude Code reports idle only when asked to.
        if idle {
            self.emit(json!({
                "type": "system", "subtype": "session_state_changed",
                "state": "idle", "session_id": session,
            }));
        }
    }

    pub fn exit(&self, code: Option<i32>, stderr: Option<&str>) {
        let mut state = self.state();
        if !state.alive {
            return;
        }
        if let Some(stderr) = stderr {
            let _ = state.output.send(ProcessOutput::Stderr(stderr.into()));
        }
        state.alive = false;
        let _ = state.output.send(ProcessOutput::Exit(code));
    }

    fn take(&self, message: Value) {
        self.state().received.push(message.clone());
        if message["type"] == "control_request" {
            self.emit(json!({
                "type": "control_response",
                "response": { "subtype": "success", "request_id": message["request_id"] },
            }));
            return;
        }
        if message["type"] != "user" {
            return;
        }
        let resume = self.state().resume.clone();
        if let Some(resume) = resume
            && !self.1.lock().unwrap().contains(&resume)
        {
            self.exit(
                Some(1),
                Some(&format!(
                    "No conversation found with session ID: {resume}\n"
                )),
            );
            return;
        }
        let content = message["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (session, first, hold) = {
            let mut state = self.state();
            state.prompts.push(content.clone());
            let first = !state.started;
            state.started = true;
            state.open += 1;
            (state.session.clone(), first, state.hold)
        };
        if first {
            self.1.lock().unwrap().insert(session.clone());
            self.emit(json!({ "type": "system", "subtype": "init", "session_id": session }));
        }
        self.emit(json!({
            "type": "assistant",
            "session_id": session,
            "message": { "content": [{ "type": "text", "text": format!("ok: {content}") }] },
        }));
        if !hold {
            self.finish(None);
        }
    }
}

impl ProcessControl for FakeClaude {
    fn write(&self, data: String, _close: bool) -> BoxFuture<'static, Result<(), String>> {
        if !self.state().alive {
            return Box::pin(async { Err("Process has exited".into()) });
        }
        for line in data.split('\n').filter(|line| !line.is_empty()) {
            self.take(serde_json::from_str(line).unwrap());
        }
        Box::pin(async { Ok(()) })
    }
    fn kill(&self) -> BoxFuture<'static, Result<(), String>> {
        self.state().killed = true;
        self.exit(None, None);
        Box::pin(async { Ok(()) })
    }
}

/// A spawn whose `claude` processes answer like Claude Code. Sessions named
/// in `known` (or created by a process) can be resumed; others fail to
/// load.
#[derive(Clone, Default)]
pub struct FakeSpawn {
    pub processes: Arc<Mutex<Vec<FakeClaude>>>,
    pub known: Arc<Mutex<HashSet<String>>>,
    /// Every new process holds its turns open until finished.
    pub hold: bool,
}

impl FakeSpawn {
    pub fn spawner(&self) -> Spawner {
        Arc::new(self.clone())
    }
    pub fn all(&self) -> Vec<FakeClaude> {
        self.processes.lock().unwrap().clone()
    }
    pub fn claudes(&self) -> Vec<FakeClaude> {
        self.all()
            .into_iter()
            .filter(|process| process.state().id == "claude")
            .collect()
    }
    pub fn live(&self) -> Vec<FakeClaude> {
        self.claudes()
            .into_iter()
            .filter(|process| !process.killed())
            .collect()
    }
}

impl Spawn for FakeSpawn {
    fn spawn(
        &self,
        id: &str,
        options: SpawnOptions,
    ) -> BoxFuture<'static, Result<Spawned, String>> {
        let flag = |name: &str| {
            options
                .args
                .iter()
                .position(|arg| arg == name)
                .and_then(|index| options.args.get(index + 1).cloned())
        };
        let resume = flag("--resume");
        let session = resume
            .clone()
            .or_else(|| flag("--session-id"))
            .unwrap_or_else(|| "fresh".into());
        let (output, receiver) = mpsc::unbounded_channel();
        let fake = FakeClaude(
            Arc::new(Mutex::new(FakeState {
                id: id.into(),
                options,
                received: Vec::new(),
                prompts: Vec::new(),
                hold: self.hold,
                killed: false,
                session,
                resume,
                open: 0,
                started: false,
                alive: true,
                output,
            })),
            self.known.clone(),
        );
        self.processes.lock().unwrap().push(fake.clone());
        if id != "claude" {
            fake.exit(Some(0), None);
        }
        Box::pin(async move {
            Ok(Spawned {
                process: Process(Arc::new(fake)),
                output: receiver,
            })
        })
    }
}

/// Lets spawned tasks run.
pub async fn flush() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}
