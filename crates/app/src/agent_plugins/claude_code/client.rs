//! The Buzz tools' client inside the app (buzz-app's `client.ts`): the
//! agent's host-held key signs and reads through its Agents2 handle, files
//! are read by a declared process on that host, and memory is read back
//! through the owner, to whom it is encrypted.

use crate::agent_plugins::process::{SpawnOptions, Spawner};
use crate::agents2::AgentHandle;
use crate::buzz_mcp::{BuzzClient, BuzzEvent, ClientResult, Memory, Template, Upload};
use base64::Engine;
use buzz_host::api::{AppAgentEvent, ProcessOutput};
use futures::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// The largest file a tool reads: the community's upload limit.
const READ_LIMIT: usize = 50 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// What the tools do as the agent: buzz-app's `AgentHandle`.
pub trait AgentActions: Send + Sync {
    fn pubkey(&self) -> String;
    fn owner(&self) -> String;
    fn publish(&self, event: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>>;
    fn query(&self, filters: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>>;
    fn upload(&self, data: Vec<u8>, mime: String) -> BoxFuture<'static, ClientResult<Upload>>;
    fn remember(
        &self,
        slug: String,
        body: String,
        after: u64,
    ) -> BoxFuture<'static, ClientResult<()>>;
}

fn event(value: Value) -> ClientResult<BuzzEvent> {
    serde_json::from_value(value).map_err(|_| "Invalid agent event".to_string())
}

impl AgentActions for AgentHandle {
    fn pubkey(&self) -> String {
        self.pubkey.clone()
    }
    fn owner(&self) -> String {
        self.owner.clone()
    }
    fn publish(&self, template: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>> {
        let handle = self.clone();
        Box::pin(async move {
            let kind = u16::try_from(template.kind).map_err(|_| "Invalid event kind")?;
            event(
                handle
                    .publish(AppAgentEvent {
                        kind,
                        content: template.content,
                        tags: template.tags,
                    })
                    .await?,
            )
        })
    }
    fn query(&self, filters: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>> {
        let handle = self.clone();
        Box::pin(async move {
            handle
                .query(&filters)
                .await?
                .into_iter()
                .map(event)
                .collect()
        })
    }
    fn upload(&self, data: Vec<u8>, mime: String) -> BoxFuture<'static, ClientResult<Upload>> {
        let handle = self.clone();
        Box::pin(async move {
            let receipt = handle.upload(&data, &mime).await?;
            serde_json::from_value(receipt).map_err(|_| "Invalid upload receipt".to_string())
        })
    }
    fn remember(
        &self,
        slug: String,
        body: String,
        after: u64,
    ) -> BoxFuture<'static, ClientResult<()>> {
        let handle = self.clone();
        Box::pin(async move { handle.remember(&slug, &body, after).await.map(drop) })
    }
}

pub type Memories = Arc<dyn Fn() -> BoxFuture<'static, ClientResult<Vec<Memory>>> + Send + Sync>;
/// Called after a memory write, with its slug.
pub type Remembered = Arc<dyn Fn(&str) + Send + Sync>;

/// The tools' client for one agent.
pub struct AppClient {
    pub agent: Arc<dyn AgentActions>,
    pub spawn: Spawner,
    /// Where relative paths start: the agent's working directory.
    pub cwd: String,
    pub memories: Memories,
    pub remembered: Remembered,
}

impl BuzzClient for AppClient {
    fn pubkey(&self) -> String {
        self.agent.pubkey()
    }
    fn query(&self, filters: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>> {
        self.agent.query(filters)
    }
    fn publish(&self, event: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>> {
        self.agent.publish(event)
    }
    fn upload(&self, data: Vec<u8>, mime: String) -> BoxFuture<'static, ClientResult<Upload>> {
        self.agent.upload(data, mime)
    }
    fn read(&self, path: String) -> BoxFuture<'static, ClientResult<Vec<u8>>> {
        let (spawn, cwd) = (self.spawn.clone(), self.cwd.clone());
        Box::pin(async move { read_file(&spawn, &cwd, &path).await })
    }
    fn memories(&self) -> BoxFuture<'static, ClientResult<Vec<Memory>>> {
        (self.memories)()
    }
    fn remember(
        &self,
        slug: String,
        body: String,
        after: u64,
    ) -> BoxFuture<'static, ClientResult<()>> {
        let write = self.agent.remember(slug.clone(), body, after);
        let remembered = self.remembered.clone();
        Box::pin(async move {
            write.await?;
            remembered(&slug);
            Ok(())
        })
    }
}

/// Reads a file with the type's declared `read` process (`base64 -i`),
/// which passes binary files through intact.
pub async fn read_file(spawn: &Spawner, cwd: &str, path: &str) -> ClientResult<Vec<u8>> {
    // `-` would wait on stdin.
    if path.trim() == "-" || path.trim().is_empty() {
        return Err(format!("Cannot read {path}"));
    }
    let mut spawned = spawn
        .spawn(
            "read",
            SpawnOptions {
                args: vec![path.into()],
                cwd: Some(cwd.into()),
                env: Default::default(),
            },
        )
        .await?;
    let process = spawned.process.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(READ_TIMEOUT).await;
        let _ = process.kill().await;
    });
    let (mut output, mut error) = (String::new(), String::new());
    let mut large = false;
    let mut code = None;
    while let Some(item) = spawned.output.recv().await {
        match item {
            ProcessOutput::Stdout(text) => {
                output += &text;
                if output.len() > READ_LIMIT / 3 * 4 + 1024 && !large {
                    large = true;
                    let _ = spawned.process.kill().await;
                }
            }
            ProcessOutput::Stderr(text) => error += &text,
            ProcessOutput::Exit(exit) => {
                code = exit;
                break;
            }
        }
    }
    timer.abort();
    if large {
        return Err(format!("{path} is larger than 50 MB"));
    }
    if code != Some(0) {
        let error = error.trim();
        return Err(if error.is_empty() {
            format!("Could not read {path}")
        } else {
            error.into()
        });
    }
    let text: String = output.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| format!("Could not read {path}"))
}
