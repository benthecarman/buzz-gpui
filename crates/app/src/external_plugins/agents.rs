//! Agent tools and agent types a plugin adds (buzz-app's `agents.addTool`
//! and `agents2.register`, `docs/plugins.md`).
//!
//! A tool call or an agent run is a request to the plugin, open until it
//! answers. Meanwhile the plugin can act as that agent with `agent/tool`,
//! which runs one of the Buzz tools (send, read, react, …) as the agent.
//! The agent's key stays on its host, as for every agent: the plugin never
//! sees it.

use super::process::Requester;
use super::protocol::{AgentToolDecl, AgentTypeDecl, INVALID_PARAMS};
use super::{ExternalPlugins, capabilities::Failure, contribute, intern};
use crate::agents2::Agent;
use crate::agents2::attention::{AttentionValue, EventData, Interest};
use crate::agents2::registry::{self, AgentTab, AgentType, TypeDefaults};
use crate::agents2::runtime::{Delivery, Trigger};
use crate::buzz_mcp::added::{self, AddedTool};
use crate::buzz_mcp::app_client::AgentActions;
use crate::buzz_mcp::tools::{Args, Reply, buzz_tool, call_tool};
use crate::buzz_mcp::{BuzzClient, BuzzEvent, ChannelTemplate, ClientResult, Context, Memory};
use crate::buzz_mcp::{Template, Upload};
use futures::FutureExt;
use futures::future::BoxFuture;
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long one tool call may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// Who an open call or run acts as.
struct Acting {
    plugin: String,
    client: Arc<dyn BuzzClient>,
    context: Context,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static ACTING: Mutex<Option<HashMap<u64, Acting>>> = Mutex::new(None);

fn acting<R>(f: impl FnOnce(&mut HashMap<u64, Acting>) -> R) -> R {
    let mut acting = ACTING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(acting.get_or_insert_with(HashMap::new))
}

/// Holds `client` as call `id` until the returned guard drops.
fn open(plugin: &str, client: Arc<dyn BuzzClient>, context: Context) -> (u64, impl Drop + use<>) {
    struct Close(u64);
    impl Drop for Close {
        fn drop(&mut self) {
            acting(|acting| acting.remove(&self.0));
        }
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    acting(|acting| {
        acting.insert(
            id,
            Acting {
                plugin: plugin.to_owned(),
                client,
                context,
            },
        )
    });
    (id, Close(id))
}

/// A tool's answer: `{"text"}`, `{"image": {"data", "mime"}}` or a string.
fn reply(value: Value) -> Result<Reply, String> {
    if let Some(text) = value.as_str() {
        return Ok(Reply::Text(text.to_owned()));
    }
    if let Some(image) = value.get("image") {
        let field = |key: &str| image.get(key).and_then(Value::as_str).map(str::to_owned);
        return match (field("data"), field("mime")) {
            (Some(data), Some(mime)) if mime.starts_with("image/") => {
                Ok(Reply::Image { data, mime })
            }
            _ => Err("The plugin answered with an image that is not valid.".into()),
        };
    }
    Ok(Reply::Text(
        value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    ))
}

fn reply_json(reply: Reply) -> Value {
    match reply {
        Reply::Text(text) => json!({ "text": text }),
        Reply::Image { data, mime } => json!({ "image": { "data": data, "mime": mime } }),
    }
}

fn tool(plugin: &str, decl: &AgentToolDecl, requester: Requester) -> AddedTool {
    let (owner, name) = (plugin.to_owned(), decl.name.clone());
    AddedTool {
        owner: plugin.to_owned(),
        name: decl.name.clone(),
        description: decl.description.clone(),
        input_schema: decl.input_schema.clone(),
        call: Arc::new(move |client, context, args| {
            let (owner, name, requester) = (owner.clone(), name.clone(), requester.clone());
            async move {
                let agent = client.pubkey();
                let (call, _open) = open(&owner, client, context.clone());
                let answered = requester
                    .request(
                        "agentTool/call",
                        json!({
                            "call": call,
                            "tool": name,
                            "arguments": Value::Object(args),
                            "agent": agent,
                            "channel": context.channel,
                            "root": context.root,
                        }),
                        TOOL_TIMEOUT,
                    )
                    .await;
                match answered {
                    Ok(Ok(value)) => reply(value),
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(error.to_string()),
                }
            }
            .boxed()
        }),
    }
}

/// The Buzz tools' client for a plugin's agent: its handle, without the
/// local files and memories the harness runtimes add.
struct HandleClient(Arc<dyn AgentActions>);

fn refused<T: Send + 'static>(what: &str) -> BoxFuture<'static, ClientResult<T>> {
    let error = format!("{what} is not available to plugin agents");
    Box::pin(async move { Err(error) })
}

impl BuzzClient for HandleClient {
    fn pubkey(&self) -> String {
        self.0.pubkey()
    }
    fn query(&self, filters: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>> {
        self.0.query(filters)
    }
    fn publish(&self, event: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>> {
        self.0.publish(event)
    }
    fn upload(&self, data: Vec<u8>, mime: String) -> BoxFuture<'static, ClientResult<Upload>> {
        self.0.upload(data, mime)
    }
    fn media(&self, url: String) -> BoxFuture<'static, ClientResult<String>> {
        self.0.media(url)
    }
    fn read(&self, _: String) -> BoxFuture<'static, ClientResult<Vec<u8>>> {
        refused("Reading local files")
    }
    fn memories(&self) -> BoxFuture<'static, ClientResult<Vec<Memory>>> {
        refused("Reading memories")
    }
    fn remember(
        &self,
        slug: String,
        body: String,
        after: u64,
    ) -> BoxFuture<'static, ClientResult<()>> {
        self.0.remember(slug, body, after)
    }
    fn templates(&self) -> Option<BoxFuture<'static, ClientResult<Vec<ChannelTemplate>>>> {
        self.0.templates()
    }
}

fn event_json(event: &EventData) -> Value {
    json!({
        "id": event.id,
        "pubkey": event.pubkey,
        "createdAt": event.created_at,
        "kind": event.kind,
        "content": event.content,
        "tags": event.tags,
    })
}

fn trigger_json(trigger: &Trigger) -> Value {
    match trigger {
        Trigger::Mention { event } => json!({ "kind": "mention", "event": event_json(event) }),
        Trigger::Watch {
            event,
            slug,
            interest,
            ..
        } => json!({
            "kind": "watch",
            "event": event_json(event),
            "watch": slug,
            "interest": interest.as_ref().map(|interest| interest.instructions.clone()),
        }),
        Trigger::Timer {
            slug,
            interest,
            spent,
            ..
        } => json!({
            "kind": "timer",
            "timer": slug,
            "interest": interest.as_ref().map(|interest| interest.instructions.clone()),
            "spent": spent,
        }),
    }
}

/// Where a run's tools default to: the event's channel and thread.
fn context(delivery: &Delivery) -> Context {
    let event = match &delivery.trigger {
        Trigger::Mention { event } | Trigger::Watch { event, .. } => Some(event),
        Trigger::Timer { .. } => None,
    };
    let root = event.map(|event| {
        event
            .tags
            .iter()
            .find(|tag| {
                tag.first().map(String::as_str) == Some("e")
                    && tag.get(3).map(String::as_str) == Some("root")
            })
            .and_then(|tag| tag.get(1).cloned())
            .unwrap_or_else(|| event.id.clone())
    });
    Context {
        channel: event
            .and_then(|event| event.tag("h").map(str::to_owned))
            .or_else(|| delivery.channel.clone()),
        root,
        top_level: false,
    }
}

struct PluginType {
    plugin: String,
    id: &'static str,
    title: SharedString,
    description: Option<SharedString>,
    config: Value,
    instructions: Option<String>,
    timeout: Duration,
    requester: Requester,
    summary: bool,
    peek: bool,
    tabs: Vec<(String, String)>,
}

/// An agent as a plugin sees it.
pub(super) fn agent_json(agent: &Agent) -> Value {
    json!({
        "pubkey": agent.pubkey,
        "name": agent.name,
        "owner": agent.owner,
        "config": agent.config,
    })
}

/// A summary answer, `{"text"}`, as a text tree for the answer cache.
fn summary_of(value: Value) -> Option<super::protocol::Node> {
    let text = value.get("text")?.as_str()?.trim();
    (!text.is_empty()).then(|| super::protocol::Node::Text {
        text: text.chars().take(120).collect(),
        style: Default::default(),
        tone: Default::default(),
    })
}

impl PluginType {
    /// The answer cache's key for `what` (`summary` or `peek`), and its
    /// target: the agent as it is now, so a saved change asks again.
    fn cached(&self, what: &str, agent: &Agent) -> (&'static str, String) {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        (&agent.name, agent.config.to_string()).hash(&mut hasher);
        (
            intern(format!("{}/{}#{what}", self.plugin, self.id)),
            format!("{}:{:x}", agent.pubkey, hasher.finish()),
        )
    }

    /// What the plugin answered about `agent`, asking it the first time.
    fn ask(
        &self,
        what: &str,
        agent: &Agent,
        parse: fn(Value) -> Option<super::protocol::Node>,
    ) -> Option<(&'static str, String, std::rc::Rc<super::protocol::Node>)> {
        let (key, target) = self.cached(what, agent);
        match contribute::answered(key, &target) {
            Some(tree) => tree.map(|tree| (key, target, tree)),
            None => {
                contribute::ask_with(
                    &self.requester,
                    key,
                    &target,
                    &format!("agent/{what}"),
                    json!({ "type": self.id, "agent": agent_json(agent) }),
                    parse,
                );
                None
            }
        }
    }
}

impl AgentType for PluginType {
    fn id(&self) -> &'static str {
        self.id
    }

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn description(&self) -> Option<SharedString> {
        self.description.clone()
    }

    fn defaults(&self) -> TypeDefaults {
        TypeDefaults {
            config: self.config.clone(),
            attention: self
                .instructions
                .iter()
                .map(|instructions| {
                    (
                        "interest/default".to_owned(),
                        AttentionValue::Interest(Interest {
                            instructions: instructions.clone(),
                        }),
                    )
                })
                .collect(),
        }
    }

    fn summary(&self, agent: &Agent) -> Option<String> {
        if !self.summary {
            return None;
        }
        match &*self.ask("summary", agent, summary_of)?.2 {
            super::protocol::Node::Text { text, .. } => Some(text.clone()),
            _ => None,
        }
    }

    fn peek(&self, agent: &Agent, _: &mut Window, cx: &mut App) -> Option<AnyElement> {
        if !self.peek {
            return None;
        }
        let (key, target, tree) = self.ask("peek", agent, contribute::tree_of)?;
        Some(contribute::drawn(
            &self.plugin,
            self.id,
            key,
            &target,
            &tree,
            cx,
        ))
    }

    fn tabs(&self) -> Vec<AgentTab> {
        self.tabs
            .iter()
            .map(|(id, title)| {
                let (plugin, kind, tab) = (self.plugin.clone(), self.id, id.clone());
                AgentTab {
                    id: id.clone().into(),
                    title: title.clone().into(),
                    build: Arc::new(move |view, window, cx| {
                        super::view::ExternalView::agent_tab(&plugin, kind, &tab, view, window, cx)
                    }),
                }
            })
            .collect()
    }

    fn runs(&self) -> bool {
        true
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn run(&self, delivery: Delivery, cx: &mut App) -> Task<Result<(), String>> {
        let Some(requester) = ExternalPlugins::running_connection(&self.plugin, cx)
            .map(|connection| connection.requester())
        else {
            return Task::ready(Err("The plugin that runs this agent is not running.".into()));
        };
        let context = context(&delivery);
        let agent = &delivery.agent;
        let params_agent =
            json!({ "pubkey": agent.pubkey, "name": agent.name, "owner": agent.owner });
        let client: Arc<dyn BuzzClient> = Arc::new(HandleClient(Arc::new(agent.clone())));
        let (run, open) = open(&self.plugin, client, context.clone());
        let request = requester.request(
            "agent/run",
            json!({
                "run": run,
                "type": self.id,
                "trigger": trigger_json(&delivery.trigger),
                "channel": context.channel,
                "root": context.root,
                "agent": params_agent,
                "config": delivery.config,
                "model": delivery.model,
                "effort": delivery.effort,
                "team": delivery.team,
            }),
            self.timeout,
        );
        let cancel = delivery.cancel.clone();
        cx.background_spawn(async move {
            let _open = open;
            let cancelled = std::pin::pin!(cancel.cancelled());
            match futures::future::select(request, cancelled).await {
                futures::future::Either::Left((answered, _)) => match answered {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(error.to_string()),
                },
                futures::future::Either::Right(((), _)) => {
                    requester.notify("agent/cancel", json!({ "run": run }));
                    Err("The run was cancelled.".into())
                }
            }
        })
    }
}

/// What a plugin added to the agent registries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentAdded {
    Tool(&'static str, &'static str),
    Type(&'static str),
}

pub fn take_away(added: AgentAdded, cx: &mut App) {
    match added {
        AgentAdded::Tool(owner, name) => added::remove(owner, name),
        AgentAdded::Type(key) => registry::unregister_type(key, cx),
    }
}

/// Registers the plugin's agent tools and types. An error names what was
/// refused; what passed stays registered.
pub fn register(
    plugin: &str,
    tools: &[AgentToolDecl],
    types: &[AgentTypeDecl],
    requester: Requester,
    cx: &mut App,
) -> (Vec<AgentAdded>, Vec<String>) {
    let (mut done, mut errors) = (Vec::new(), Vec::new());
    let owner = intern(plugin.to_owned());
    for decl in tools {
        match added::add(tool(plugin, decl, requester.clone())) {
            Ok(()) => done.push(AgentAdded::Tool(owner, intern(decl.name.clone()))),
            Err(error) => errors.push(error),
        }
    }
    for decl in types {
        let kind = PluginType {
            plugin: plugin.to_owned(),
            id: intern(decl.id.clone()),
            title: decl.title.clone().into(),
            description: decl.description.clone().map(Into::into),
            config: decl.config.clone().unwrap_or_else(|| json!({})),
            instructions: decl.instructions.clone(),
            timeout: Duration::from_secs(decl.timeout_seconds),
            requester: requester.clone(),
            summary: decl.summary,
            peek: decl.peek,
            tabs: decl
                .tabs
                .iter()
                .map(|tab| (tab.id.clone(), tab.title.clone()))
                .collect(),
        };
        match registry::register_type(plugin, Arc::new(kind), cx) {
            Ok(()) => done.push(AgentAdded::Type(intern(format!("{plugin}/{}", decl.id)))),
            Err(error) => errors.push(error),
        }
    }
    (done, errors)
}

/// `agent/tool`: runs a Buzz tool as the agent of an open call or run.
pub fn act(
    plugin: &str,
    params: &Value,
    cx: &App,
) -> Result<Task<Result<Value, Failure>>, Failure> {
    let call = params
        .get("call")
        .or_else(|| params.get("run"))
        .and_then(Value::as_u64)
        .ok_or((INVALID_PARAMS, "call or run is required".to_owned()))?;
    let name = params
        .get("tool")
        .and_then(Value::as_str)
        .filter(|name| buzz_tool(name))
        .ok_or((
            INVALID_PARAMS,
            "tool must be one of the Buzz tools".to_owned(),
        ))?
        .to_owned();
    let args: Args = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let (client, context) = acting(|acting| {
        acting
            .get(&call)
            .filter(|open| open.plugin == plugin)
            .map(|open| (open.client.clone(), open.context.clone()))
    })
    .ok_or((1, "That call or run has ended.".to_owned()))?;
    let done = crate::runtime::Runtime::handle(cx)
        .spawn(async move { call_tool(client, &context, &name, &args).await });
    Ok(cx.background_spawn(async move {
        match done.await {
            Ok(Ok(reply)) => Ok(reply_json(reply)),
            Ok(Err(error)) => Err((2, error)),
            Err(error) => Err((2, error.to_string())),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    struct Nobody;

    impl BuzzClient for Nobody {
        fn pubkey(&self) -> String {
            "a".repeat(64)
        }
        fn query(&self, _: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>> {
            refused("query")
        }
        fn publish(&self, _: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>> {
            refused("publish")
        }
        fn upload(&self, _: Vec<u8>, _: String) -> BoxFuture<'static, ClientResult<Upload>> {
            refused("upload")
        }
        fn read(&self, _: String) -> BoxFuture<'static, ClientResult<Vec<u8>>> {
            refused("read")
        }
        fn memories(&self) -> BoxFuture<'static, ClientResult<Vec<Memory>>> {
            refused("memories")
        }
        fn remember(&self, _: String, _: String, _: u64) -> BoxFuture<'static, ClientResult<()>> {
            refused("remember")
        }
    }

    #[test]
    fn replies_are_text_or_images() {
        assert_eq!(reply(json!("hi")), Ok(Reply::Text("hi".into())));
        assert_eq!(reply(json!({ "text": "ok" })), Ok(Reply::Text("ok".into())));
        assert_eq!(
            reply(json!({ "image": { "data": "AA==", "mime": "image/png" } })),
            Ok(Reply::Image {
                data: "AA==".into(),
                mime: "image/png".into()
            })
        );
        assert!(reply(json!({ "image": { "data": "AA==", "mime": "text/plain" } })).is_err());
    }

    #[test]
    fn open_calls_close_with_their_guard() {
        let (id, guard) = open("a.b", Arc::new(Nobody), Context::default());
        assert!(acting(|acting| acting.contains_key(&id)));
        drop(guard);
        assert!(!acting(|acting| acting.contains_key(&id)));
    }

    /// The example plugin answers a tool call, and a Greeter run sends
    /// through `agent/tool` before it ends.
    #[test]
    fn the_example_plugin_runs_tools_and_agents() {
        use super::super::process::{Connection, Event};
        use super::super::protocol::{self, Contributions, Manifest};
        use futures::StreamExt;

        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipped: no python3");
            return;
        }
        let folder =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/hello");
        let manifest =
            Manifest::parse(&std::fs::read_to_string(folder.join(protocol::MANIFEST)).unwrap())
                .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (connection, mut events) =
            Connection::start(&manifest, &folder, runtime.handle()).unwrap();
        runtime.block_on(async {
            let contributions = Contributions::parse(
                connection
                    .request("initialize", json!({ "apiVersion": 1 }))
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(contributions.agent_types[0].id, "greeter");

            let added = tool(
                "com.example.hello",
                &contributions.agent_tools[0],
                connection.requester(),
            );
            let answer = (added.call)(Arc::new(Nobody), Context::default(), Args::new())
                .await
                .unwrap();
            assert!(
                matches!(&answer, Reply::Text(text) if text.starts_with("It is ")),
                "{answer:?}"
            );

            let run = connection.requester().request(
                "agent/run",
                json!({
                    "run": 7,
                    "type": "greeter",
                    "trigger": { "kind": "mention", "event": { "content": "hi there" } },
                    "agent": { "pubkey": "a", "name": "Greeter", "owner": "b" },
                    "config": { "greeting": "Howdy" },
                }),
                Duration::from_secs(10),
            );
            let Some(Event::Request { id, method, params }) = events.next().await else {
                panic!("no agent/tool");
            };
            assert_eq!(method, "agent/tool");
            assert_eq!(params["run"], 7);
            assert_eq!(params["tool"], "send");
            assert_eq!(
                params["arguments"]["text"],
                "Howdy from Greeter! You said: hi there"
            );
            connection.respond(&id, Ok(json!({ "text": "sent" })));
            assert_eq!(run.await.unwrap(), Ok(Value::Null));
        });
    }
}
