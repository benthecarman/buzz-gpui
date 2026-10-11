//! The Buzz tools an agent acts with, a port of buzz-app's `src/buzz-mcp`.
//!
//! Claude Code agents call them as an in-process MCP server over their
//! stream-json pipe ([`rpc::respond`]); whoever holds the agent's key
//! implements [`BuzzClient`], so the tools never see the key itself.

pub mod app_client;
pub mod rpc;
pub mod tools;

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A signed event, as the community returns it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuzzEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// An event for the agent to sign.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub kind: u32,
    pub content: String,
    pub tags: Vec<Vec<String>>,
}

/// The community's description of an uploaded blob.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Upload {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub mime: String,
    pub dim: Option<String>,
    pub blurhash: Option<String>,
    pub thumb: Option<String>,
    pub duration: Option<f64>,
}

/// One of the agent's memory entries, decrypted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    pub slug: String,
    pub body: String,
    pub created_at: u64,
}

/// A failed client call, worded for the agent.
pub type ClientResult<T> = Result<T, String>;

/// What the Buzz tools need from whoever holds the agent's key.
pub trait BuzzClient: Send + Sync {
    fn pubkey(&self) -> String;
    /// Reads the community as the agent. Every filter names its kinds.
    fn query(&self, filters: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>>;
    /// Signs `event` as the agent and posts it.
    fn publish(&self, event: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>>;
    fn upload(&self, data: Vec<u8>, mime: String) -> BoxFuture<'static, ClientResult<Upload>>;
    /// A local file's bytes, for tools that take a `path`.
    fn read(&self, path: String) -> BoxFuture<'static, ClientResult<Vec<u8>>>;
    /// The agent's memory entries, decrypted.
    fn memories(&self) -> BoxFuture<'static, ClientResult<Vec<Memory>>>;
    /// Writes entry `slug`, newer than the one it replaces (`after`, or 0).
    fn remember(
        &self,
        slug: String,
        body: String,
        after: u64,
    ) -> BoxFuture<'static, ClientResult<()>>;
}

/// Where the agent is working, so tools can default to it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub channel: Option<String>,
    pub root: Option<String>,
}
