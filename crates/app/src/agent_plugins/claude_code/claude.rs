//! One `claude` process driven over its stream-json protocol, a port of
//! buzz-app's `claude.ts`: the protocol the Claude Agent SDK speaks to the
//! same binary, so no Node runtime is needed. The process stays alive
//! between turns; messages sent while it works are folded into the running
//! turn at its next tool call. Buzz tools are served over the same pipe, as
//! the SDK serves an in-process MCP server, so the process holds no Buzz
//! key.

use crate::agent_plugins::process::{Lines, Process, SpawnOptions, Spawner};
use buzz_host::api::ProcessOutput;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// Its tools are `mcp__buzz__<name>`.
pub const TOOL_SERVER: &str = "buzz";
const FINAL_TOOL: &str = "mcp__buzz__send";
const STDERR_LIMIT: usize = 4_000;
/// How long Claude may stay idle with a message it has not taken before
/// the messages are settled anyway.
const UNREAD_GRACE: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(3)
};
/// The longest line of output read. Tool results with images run to a
/// few megabytes.
const FRAME_LIMIT: usize = 64 * 1024 * 1024;

/// Answers the in-process server's JSON-RPC for the conversation a process
/// serves. A call of `send` with `final: true` ends the turn when it
/// succeeds.
pub type ToolServer =
    Arc<dyn Fn(Option<String>, Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct Launch {
    pub cwd: String,
    /// Appended to Claude Code's own system prompt.
    pub system_prompt: String,
    pub model: Option<String>,
    /// A saved session to continue.
    pub resume: Option<String>,
    /// The id a new session takes, so it is known before its first turn.
    pub session_id: Option<String>,
    /// The in-process MCP server's tools.
    pub tools: Option<ToolServer>,
}

/// How a burst of work ended: every message sent so far has been answered,
/// or the process is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    pub ok: bool,
    /// Why it failed, for the person waiting.
    pub error: Option<String>,
}

impl Settled {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
        }
    }
}

/// What `claude` is started with: as the SDK starts it, reporting when it
/// is idle, signed in as `claude auth login` left it, and never as part of
/// a Claude Code session that started the app.
pub fn env() -> BTreeMap<String, Option<String>> {
    let mut env = BTreeMap::new();
    env.insert("CLAUDE_CODE_ENTRYPOINT".into(), Some("sdk-ts".into()));
    env.insert(
        "CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS".into(),
        Some("1".into()),
    );
    // A key in the shell that launched the app would outrank the sign-in;
    // the interactive CLI asks before using one, but stream-json mode uses
    // it silently.
    for name in [
        "ANTHROPIC_API_KEY",
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_AGENT_SDK_VERSION",
        "CLAUDE_PID",
    ] {
        env.insert(name.into(), None);
    }
    env
}

/// The arguments `claude` is started with.
pub fn args(launch: &Launch) -> Vec<String> {
    let mut args: Vec<String> = [
        "--output-format",
        "stream-json",
        "--input-format",
        "stream-json",
        "--verbose",
        // Echoes each message as Claude takes it, so a turn that ends just
        // as one arrives is not taken for the answer to it.
        "--replay-user-messages",
        "--permission-mode",
        "bypassPermissions",
    ]
    .map(String::from)
    .to_vec();
    if let Some(model) = &launch.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(resume) = &launch.resume {
        args.extend(["--resume".into(), resume.clone()]);
    } else if let Some(id) = &launch.session_id {
        args.extend(["--session-id".into(), id.clone()]);
    }
    if launch.tools.is_some() {
        // Listed up front, so Claude need not search for them first.
        let config = json!({
            "mcpServers": {
                TOOL_SERVER: { "type": "sdk", "name": TOOL_SERVER, "alwaysLoad": true },
            },
        });
        args.extend(["--mcp-config".into(), config.to_string()]);
    }
    args
}

struct State {
    /// Its session, known once the first turn starts (or at once when
    /// resuming).
    session_id: Option<String>,
    /// The conversation it serves, set when a spare is put to use.
    conversation: Option<String>,
    /// Messages sent and not yet answered.
    pending: usize,
    /// Messages sent that Claude has not echoed back as taken yet.
    unread: usize,
    /// Bumped on every reported state change, so a late check can tell
    /// whether Claude moved on.
    state_changes: u64,
    /// It has begun a turn, so its session loaded.
    began: bool,
    waiters: Vec<oneshot::Sender<Settled>>,
    buffer: Lines,
    stderr: String,
    last_error: Option<String>,
    alive: bool,
    /// It reports idle; an older `claude` that does not is settled per
    /// result.
    reports_state: bool,
    requests: u64,
}

struct Inner {
    process: Process,
    /// Replaced when a spare is given to the agent that takes it.
    tools: Mutex<Option<ToolServer>>,
    state: Mutex<State>,
    /// Lines for stdin, written one at a time in the order sent.
    writes: mpsc::UnboundedSender<(String, oneshot::Sender<Result<(), String>>)>,
    exited: watch::Sender<Option<Option<i32>>>,
}

/// A running `claude`. Cheap to clone; clones are the same process.
#[derive(Clone)]
pub struct ClaudeProcess {
    inner: Arc<Inner>,
}

impl ClaudeProcess {
    pub async fn start(spawn: &Spawner, launch: Launch) -> Result<Self, String> {
        let claude = Self::spawn(spawn, &launch).await?;
        claude.initialize(&launch).await?;
        Ok(claude)
    }

    /// Starts `claude` without its prompt, which `launch` may leave empty.
    /// This is most of what a start costs, and fixes nothing about the agent
    /// it serves but its folder and model.
    pub async fn spawn(spawn: &Spawner, launch: &Launch) -> Result<Self, String> {
        let spawned = spawn
            .spawn(
                "claude",
                SpawnOptions {
                    args: args(launch),
                    cwd: Some(launch.cwd.clone()),
                    env: env(),
                },
            )
            .await?;
        Ok(Self::attach(
            spawned.process,
            spawned.output,
            launch.resume.clone().or(launch.session_id.clone()),
            launch.tools.clone(),
        ))
    }

    /// Gives a started process its prompt, and its tools to whoever it now
    /// works for. Loads settings and tools now rather than on the first
    /// message.
    pub async fn initialize(&self, launch: &Launch) -> Result<(), String> {
        if let Some(tools) = &launch.tools {
            *self.tools() = Some(tools.clone());
        }
        let mut initialize = json!({
            "subtype": "initialize",
            "appendSystemPrompt": launch.system_prompt,
        });
        if launch.tools.is_some() {
            initialize["sdkMcpServers"] = json!([TOOL_SERVER]);
            initialize["hooks"] = json!({
                "PostToolUse": [{ "matcher": FINAL_TOOL, "hookCallbackIds": ["final"] }],
            });
        }
        self.request(initialize).await
    }

    /// Drives a spawned process: reads its output and writes its input.
    fn attach(
        process: Process,
        mut output: mpsc::UnboundedReceiver<ProcessOutput>,
        session_id: Option<String>,
        tools: Option<ToolServer>,
    ) -> Self {
        let (writes, mut queue) =
            mpsc::unbounded_channel::<(String, oneshot::Sender<Result<(), String>>)>();
        let inner = Arc::new(Inner {
            process: process.clone(),
            tools: Mutex::new(tools),
            state: Mutex::new(State {
                session_id,
                conversation: None,
                pending: 0,
                unread: 0,
                state_changes: 0,
                began: false,
                waiters: Vec::new(),
                buffer: Lines::default(),
                stderr: String::new(),
                last_error: None,
                alive: true,
                reports_state: false,
                requests: 0,
            }),
            writes,
            exited: watch::Sender::new(None),
        });
        tokio::spawn(async move {
            while let Some((line, done)) = queue.recv().await {
                let _ = done.send(process.write(line).await);
            }
        });
        let claude = Self { inner };
        let reader = claude.clone();
        tokio::spawn(async move {
            let mut code = None;
            while let Some(item) = output.recv().await {
                match item {
                    ProcessOutput::Stdout(data) => reader.read(&data),
                    ProcessOutput::Stderr(data) => reader.read_error(&data),
                    ProcessOutput::Exit(exit) => {
                        code = exit;
                        break;
                    }
                }
            }
            reader.on_exit(code);
        });
        claude
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
    fn tools(&self) -> MutexGuard<'_, Option<ToolServer>> {
        self.inner
            .tools
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn running(&self) -> bool {
        self.state().alive
    }
    pub fn busy(&self) -> bool {
        self.state().pending > 0
    }
    pub fn started(&self) -> bool {
        self.state().began
    }
    pub fn session_id(&self) -> Option<String> {
        self.state().session_id.clone()
    }
    pub fn set_conversation(&self, conversation: &str) {
        self.state().conversation = Some(conversation.into());
    }

    /// Resolves with the exit code once the process is gone.
    pub async fn exited(&self) -> Option<i32> {
        let mut exited = self.inner.exited.subscribe();
        match exited.wait_for(Option::is_some).await {
            Ok(code) => code.flatten(),
            Err(_) => None,
        }
    }

    /// Sends one user message. Resolves when it, and anything sent after
    /// it, has been answered and the session is idle again.
    pub fn send(&self, text: &str) -> impl std::future::Future<Output = Settled> + Send + 'static {
        let (sender, receiver) = oneshot::channel();
        let alive = {
            let mut state = self.state();
            if state.alive {
                state.pending += 1;
                state.unread += 1;
                state.waiters.push(sender);
            }
            state.alive
        };
        let write = alive.then(|| {
            self.write(json!({
                "type": "user",
                "message": { "role": "user", "content": text },
                "parent_tool_use_id": null,
                "session_id": "",
            }))
        });
        let claude = self.clone();
        async move {
            let Some(write) = write else {
                return Settled::failed("Claude Code is not running");
            };
            if let Err(error) = write.await {
                claude.settle(Settled::failed(format!(
                    "Claude Code did not take the message: {error}"
                )));
            }
            receiver
                .await
                .unwrap_or_else(|_| Settled::failed("Claude Code was stopped"))
        }
    }

    pub async fn kill(&self) {
        let _ = self.inner.process.kill().await;
    }

    fn write(
        &self,
        message: Value,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send + 'static {
        let (done, result) = oneshot::channel();
        let queued = self.inner.writes.send((format!("{message}\n"), done));
        async move {
            queued.map_err(|_| "Claude Code is not running".to_string())?;
            result
                .await
                .unwrap_or_else(|_| Err("Claude Code is not running".into()))
        }
    }

    async fn request(&self, request: Value) -> Result<(), String> {
        let id = {
            let mut state = self.state();
            state.requests += 1;
            state.requests
        };
        self.write(json!({
            "type": "control_request",
            "request_id": format!("buzz-{id}"),
            "request": request,
        }))
        .await
    }

    fn settle(&self, settled: Settled) {
        let waiters = {
            let mut state = self.state();
            state.pending = 0;
            state.unread = 0;
            state.last_error = None;
            std::mem::take(&mut state.waiters)
        };
        for waiter in waiters {
            let _ = waiter.send(settled.clone());
        }
    }

    fn on_exit(&self, code: Option<i32>) {
        let error = {
            let mut state = self.state();
            state.alive = false;
            state.last_error.clone().unwrap_or_else(|| {
                let last = state.stderr.trim().lines().last().unwrap_or("").to_string();
                if last.is_empty() {
                    format!(
                        "Claude Code exited ({})",
                        code.map_or("signal".into(), |code| code.to_string())
                    )
                } else {
                    last
                }
            })
        };
        self.settle(Settled::failed(error));
        self.inner.exited.send_replace(Some(code));
    }

    fn read_error(&self, data: &str) {
        let mut state = self.state();
        state.stderr.push_str(data);
        if state.stderr.len() > STDERR_LIMIT {
            let mut cut = state.stderr.len() - STDERR_LIMIT;
            while !state.stderr.is_char_boundary(cut) {
                cut += 1;
            }
            state.stderr.drain(..cut);
        }
    }

    fn read(&self, data: &str) {
        let (lines, oversized) = self.state().buffer.push(data, FRAME_LIMIT);
        for line in lines {
            if let Ok(message) = serde_json::from_str::<Value>(line.trim()) {
                self.handle(message);
            }
        }
        // Its output can't be followed any more, so it stops.
        if oversized {
            self.state().last_error = Some("Claude Code output frame is too large".into());
            let claude = self.clone();
            tokio::spawn(async move { claude.kill().await });
        }
    }

    fn handle(&self, message: Value) {
        let kind = message.get("type").and_then(Value::as_str);
        let subtype = message.get("subtype").and_then(Value::as_str);
        {
            let mut state = self.state();
            if let Some(id) = message
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                state.session_id = Some(id.into());
            }
            if kind == Some("system") && subtype == Some("init") {
                state.began = true;
            }
        }
        if kind == Some("control_request") {
            self.answer(message);
            return;
        }
        if kind == Some("user") && message.get("isReplay") == Some(&Value::Bool(true)) {
            let mut state = self.state();
            state.unread = state.unread.saturating_sub(1);
            return;
        }
        if kind == Some("system") && subtype == Some("session_state_changed") {
            let (idle, unread, change) = {
                let mut state = self.state();
                state.reports_state = true;
                state.state_changes += 1;
                let idle = message.get("state").and_then(Value::as_str) == Some("idle")
                    && state.pending > 0;
                (idle, state.unread, state.state_changes)
            };
            if idle && unread == 0 {
                self.done();
            } else if idle {
                // A message written as the turn ended is still to be read:
                // Claude goes running again for it. Should it stay idle
                // instead, the messages are settled anyway, never left
                // waiting.
                let claude = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(UNREAD_GRACE).await;
                    let still_idle = {
                        let state = claude.state();
                        state.state_changes == change && state.pending > 0
                    };
                    if still_idle {
                        claude.done();
                    }
                });
            }
            return;
        }
        if kind != Some("result") {
            return;
        }
        let done = {
            let mut state = self.state();
            // An interrupt is not a failure to report.
            if message.get("is_error").is_some_and(truthy)
                && message.get("terminal_reason").and_then(Value::as_str)
                    != Some("aborted_streaming")
            {
                state.last_error = Some(
                    message
                        .get("result")
                        .and_then(Value::as_str)
                        .filter(|result| !result.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            format!("Claude Code stopped ({})", subtype.unwrap_or("undefined"))
                        }),
                );
            }
            !state.reports_state && state.pending > 0
        };
        if done {
            self.done();
        }
    }

    fn done(&self) {
        let error = self.state().last_error.clone();
        self.settle(match error {
            Some(error) => Settled::failed(error),
            None => Settled::ok(),
        });
    }

    /// Answers what Claude Code asks its host: calls to the Buzz tools, the
    /// hook after `send`, and, since permissions are bypassed, anything
    /// else only so an unexpected request does not stall the turn.
    fn answer(&self, message: Value) {
        let id = message.get("request_id").cloned().unwrap_or(Value::Null);
        let request = message.get("request").cloned().unwrap_or_else(|| json!({}));
        let subtype = request.get("subtype").and_then(Value::as_str);
        let success = |response: Value| json!({ "subtype": "success", "request_id": id, "response": response });
        let tools = self.tools().clone();
        let response = match (subtype, tools) {
            (Some("mcp_message"), Some(tools)) => {
                let conversation = self.state().conversation.clone();
                let mcp = request.get("message").cloned().unwrap_or(Value::Null);
                let claude = self.clone();
                tokio::spawn(async move {
                    let call_id = mcp.get("id").cloned().unwrap_or(json!(0));
                    let call_id = if call_id.is_null() { json!(0) } else { call_id };
                    let answer = tools(conversation, mcp).await.unwrap_or_else(|error| {
                        json!({
                            "jsonrpc": "2.0",
                            "id": call_id,
                            "error": { "code": -32603, "message": error },
                        })
                    });
                    let response = json!({
                        "subtype": "success",
                        "request_id": id,
                        "response": { "mcp_response": answer },
                    });
                    let _ = claude
                        .write(json!({ "type": "control_response", "response": response }))
                        .await;
                });
                return;
            }
            (Some("can_use_tool"), _) => success(json!({
                "behavior": "allow",
                "updatedInput": request.get("input").cloned().unwrap_or_else(|| json!({})),
            })),
            (Some("hook_callback"), _) => success(if self.is_final(request.get("input")) {
                json!({ "continue": false })
            } else {
                json!({})
            }),
            (other, _) => json!({
                "subtype": "error",
                "request_id": id,
                "error": format!("Buzz does not handle {}", other.unwrap_or("undefined")),
            }),
        };
        let write = self.write(json!({ "type": "control_response", "response": response }));
        tokio::spawn(async move {
            let _ = write.await;
        });
    }

    /// A `send` marked final succeeded (a failed call runs no PostToolUse
    /// hook), so the turn can end without a closing model call. Not when
    /// another message came in meanwhile: that one still needs an answer.
    fn is_final(&self, input: Option<&Value>) -> bool {
        let input = input.cloned().unwrap_or(Value::Null);
        input.get("tool_name").and_then(Value::as_str) == Some(FINAL_TOOL)
            && input
                .get("tool_input")
                .and_then(|tool_input| tool_input.get("final"))
                == Some(&Value::Bool(true))
            && self.state().pending == 1
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
