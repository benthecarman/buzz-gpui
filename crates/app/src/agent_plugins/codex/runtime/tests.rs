//! buzz-app's `runtime.test.ts`: the app-server fixture completes turns
//! only when a test says so; the runtime owns routing, steering, cleanup
//! and what it posts.

use super::*;
use crate::agent_plugins::codex::rpc::testing::{FakeCodex, Gate, Held, flush, gate, until};
use crate::agent_plugins::owner_relay::ChannelInfo;
use crate::buzz_mcp::{ClientResult, Upload};
use std::collections::VecDeque;

const SCOPE: &str = "https://relay.test:owner";
const CHANNEL: &str = "channel";

fn owner() -> String {
    "a".repeat(64)
}
fn agent_key() -> String {
    "b".repeat(64)
}
fn root() -> String {
    "c".repeat(64)
}
fn other_root() -> String {
    "e".repeat(64)
}

type Pending = BoxFuture<'static, Result<Vec<BuzzEvent>, String>>;

#[derive(Default)]
struct FakeRelay {
    dm: Mutex<bool>,
    /// Answers for the next reads; later reads find nothing.
    reads: Mutex<VecDeque<Pending>>,
    read_count: Mutex<usize>,
    memories: Arc<Mutex<Vec<Memory>>>,
    memory_reads: Mutex<usize>,
}

impl FakeRelay {
    /// The next read waits for `gate`, then answers `events` (or fails).
    fn hold(&self, gate: Gate, answer: Result<Vec<BuzzEvent>, String>) {
        locked(&self.reads).push_back(
            async move {
                gate.await;
                answer
            }
            .boxed(),
        );
    }
    fn reads(&self) -> usize {
        *locked(&self.read_count)
    }
}

impl OwnerRelay for FakeRelay {
    fn channel(&self, id: &str) -> Option<ChannelInfo> {
        (id == CHANNEL).then(|| ChannelInfo {
            name: Some("test".into()),
            dm: *locked(&self.dm),
            members: vec![],
        })
    }
    fn read(&self, _: Vec<Value>) -> BoxFuture<'static, Result<Vec<BuzzEvent>, String>> {
        *locked(&self.read_count) += 1;
        locked(&self.reads)
            .pop_front()
            .unwrap_or_else(|| async { Ok(vec![]) }.boxed())
    }
    fn names(&self, _: Vec<String>, _: Vec<String>) -> BoxFuture<'static, HashMap<String, String>> {
        async { HashMap::new() }.boxed()
    }
    fn memories(&self, agent: &str) -> BoxFuture<'static, Result<Vec<Memory>, String>> {
        assert_eq!(agent, agent_key());
        *locked(&self.memory_reads) += 1;
        let memories = locked(&self.memories).clone();
        async move { Ok(memories) }.boxed()
    }
}

#[derive(Default)]
struct FakeAgent {
    /// Another agent's pubkey and name; the fixture's own without one.
    identity: Option<(String, String)>,
    published: Mutex<Vec<Template>>,
    /// Publishing fails this many more times.
    reject: Mutex<usize>,
    events: Mutex<Vec<BuzzEvent>>,
    uploads: Mutex<Vec<(Vec<u8>, String)>>,
    remembered: Mutex<Vec<(String, String, u64)>>,
    memories: Arc<Mutex<Vec<Memory>>>,
}

fn matches(event: &BuzzEvent, filter: &Value) -> bool {
    filter.as_object().is_none_or(|filter| {
        filter.iter().all(|(key, value)| {
            let list = || value.as_array().cloned().unwrap_or_default();
            match key.as_str() {
                "ids" => list().contains(&json!(event.id)),
                "kinds" => list().contains(&json!(event.kind)),
                key if key.starts_with('#') => event.tags.iter().any(|tag| {
                    tag.first().map(String::as_str) == Some(&key[1..])
                        && tag.get(1).is_some_and(|item| list().contains(&json!(item)))
                }),
                _ => true,
            }
        })
    })
}

impl AgentActions for FakeAgent {
    fn pubkey(&self) -> String {
        self.identity
            .as_ref()
            .map_or_else(agent_key, |(pubkey, _)| pubkey.clone())
    }
    fn owner(&self) -> String {
        owner()
    }
    fn name(&self) -> String {
        self.identity
            .as_ref()
            .map_or_else(|| "Codex".into(), |(_, name)| name.clone())
    }
    fn publish(&self, template: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>> {
        locked(&self.published).push(template.clone());
        let mut reject = locked(&self.reject);
        if *reject > 0 {
            *reject -= 1;
            return async { Err("Signing rejected".to_string()) }.boxed();
        }
        let event = BuzzEvent {
            id: "d".repeat(64),
            pubkey: self.pubkey(),
            kind: template.kind,
            content: template.content,
            tags: template.tags,
            ..Default::default()
        };
        async move { Ok(event) }.boxed()
    }
    fn query(&self, filters: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>> {
        let found: Vec<BuzzEvent> = locked(&self.events)
            .iter()
            .filter(|event| filters.iter().any(|filter| matches(event, filter)))
            .cloned()
            .collect();
        async move { Ok(found) }.boxed()
    }
    fn upload(&self, data: Vec<u8>, mime: String) -> BoxFuture<'static, ClientResult<Upload>> {
        locked(&self.uploads).push((data, mime.clone()));
        async move {
            Ok(Upload {
                url: "https://files.test/file".into(),
                sha256: "e".repeat(64),
                size: 12,
                mime,
                ..Default::default()
            })
        }
        .boxed()
    }
    fn remember(
        &self,
        slug: String,
        body: String,
        after: u64,
    ) -> BoxFuture<'static, ClientResult<()>> {
        locked(&self.remembered).push((slug.clone(), body.clone(), after));
        let mut memories = locked(&self.memories);
        memories.retain(|memory| memory.slug != slug);
        memories.push(Memory {
            slug,
            body,
            created_at: after + 1,
        });
        async { Ok(()) }.boxed()
    }
}

#[derive(Default)]
struct MemoryStorage(Mutex<HashMap<String, Value>>);

impl Storage for MemoryStorage {
    fn get(&self, key: &str) -> Option<Value> {
        locked(&self.0).get(key).cloned()
    }
    fn set(&self, key: &str, value: Value) -> Result<(), String> {
        locked(&self.0).insert(key.into(), value);
        Ok(())
    }
}

struct Fixture {
    fake: FakeCodex,
    runtime: CodexRuntime,
    relay: Arc<FakeRelay>,
    agent: Arc<FakeAgent>,
    storage: Arc<MemoryStorage>,
    next: Mutex<u64>,
    tools: Mutex<u64>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.dispose();
    }
}

fn fixture() -> Fixture {
    fixture_with(Arc::default())
}

fn fixture_with(storage: Arc<MemoryStorage>) -> Fixture {
    let memories = Arc::new(Mutex::new(Vec::new()));
    let relay = Arc::new(FakeRelay {
        memories: memories.clone(),
        ..Default::default()
    });
    let agent = Arc::new(FakeAgent {
        memories,
        ..Default::default()
    });
    let runtime = CodexRuntime::with_storage(storage.clone());
    runtime.set_relays(HashMap::from([(
        agent_key(),
        (SCOPE.into(), relay.clone() as Arc<dyn OwnerRelay>),
    )]));
    runtime.sync(&[agent_key()]);
    Fixture {
        fake: FakeCodex::default(),
        runtime,
        relay,
        agent,
        storage,
        next: Mutex::new(0),
        tools: Mutex::new(0),
    }
}

impl Fixture {
    fn event(&self, content: &str, thread: &str) -> BuzzEvent {
        let id = {
            let mut next = locked(&self.next);
            *next += 1;
            format!("{:064x}", *next)
        };
        let event = BuzzEvent {
            id,
            pubkey: owner(),
            kind: 9,
            content: content.into(),
            created_at: 10,
            tags: vec![
                vec!["h".into(), CHANNEL.into()],
                vec!["e".into(), thread.into(), String::new(), "reply".into()],
            ],
            sig: String::new(),
        };
        locked(&self.agent.events).push(event.clone());
        event
    }

    fn delivery_with(&self, content: &str, thread: &str, config: Value) -> Run {
        Run {
            wake: Wake::Mention {
                event: self.event(content, thread),
            },
            channel: Some(CHANNEL.into()),
            agent: self.agent.clone(),
            config,
            spawn: self.fake.spawner(),
        }
    }

    fn delivery(&self, content: &str) -> Run {
        self.delivery_in(content, &root())
    }

    fn delivery_in(&self, content: &str, thread: &str) -> Run {
        self.delivery_with(
            content,
            thread,
            json!({
                "workspace": "/tmp/codex-test",
                "model": "test-model",
                "effort": "low",
                "instructions": "TEST_INSTRUCTIONS",
            }),
        )
    }

    async fn run(&self, run: Run) {
        self.runtime.run(run).await.unwrap();
    }

    fn starts(&self) -> Vec<Value> {
        self.fake.calls("turn/start")
    }

    /// Waits until `count` turns were asked for and the runtime has taken
    /// Codex's acceptance of the latest: its thread is saved and its tool
    /// calls are routed. Seeing the request sent is not enough, as the
    /// acceptance is read after it.
    async fn starts_reach(&self, count: usize) {
        let turn = format!("turn-{count}");
        until(|| {
            self.starts().len() == count
                && self.actives().iter().any(|active| {
                    active.turn_id().as_ref() == Some(&turn)
                        && active.ready.clone().now_or_never().is_some()
                })
        })
        .await;
    }

    fn published(&self) -> Vec<Template> {
        locked(&self.agent.published).clone()
    }

    fn contents(&self) -> Vec<String> {
        self.published()
            .into_iter()
            .map(|template| template.content)
            .collect()
    }

    async fn published_reach(&self, count: usize) {
        until(|| self.published().len() == count).await;
    }

    fn thread_of(&self, index: usize) -> Value {
        self.starts()[index]["params"]["threadId"].clone()
    }

    fn status(&self) -> Option<String> {
        self.runtime
            .sessions(&agent_key())
            .first()
            .map(|view| view.status.clone())
    }

    async fn status_reaches(&self, status: &str) {
        until(|| self.status().as_deref() == Some(status)).await;
    }

    fn steers(&self) -> usize {
        self.fake.calls("turn/steer").len()
    }

    /// Every turn the runtime is running.
    fn actives(&self) -> Vec<Arc<Active>> {
        // Entries first: the runtime locks a lane, then the entries.
        let entries: Vec<Arc<Entry>> = locked(&self.runtime.inner.entries)
            .values()
            .cloned()
            .collect();
        entries
            .iter()
            .flat_map(|entry| {
                locked(&entry.lanes)
                    .values()
                    .filter_map(|lane| lane.active())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Waits until `count` follow-ups were steered in and the runtime has
    /// taken Codex's acceptance of each: tools then answer the latest.
    /// Seeing the request sent is not enough, as the acceptance is read
    /// after it.
    async fn steered(&self, count: u64) {
        until(|| {
            self.steers() == count as usize
                && self
                    .actives()
                    .iter()
                    .any(|active| active.version.load(Ordering::SeqCst) == count)
        })
        .await;
    }

    /// Codex calls a Buzz tool in turn `index` and gets its answer.
    async fn tool_in(&self, name: &str, args: Value, index: usize, overrides: Value) -> Value {
        let id = {
            let mut tools = locked(&self.tools);
            *tools += 1;
            format!("tool-{}", *tools)
        };
        let mut params = json!({
            "threadId": self.thread_of(index),
            "turnId": format!("turn-{}", index + 1),
            "namespace": "buzz",
            "callId": id,
            "tool": name,
            "arguments": args,
        });
        if let (Some(params), Value::Object(overrides)) = (params.as_object_mut(), overrides) {
            params.extend(overrides);
        }
        self.fake
            .emit(json!({ "id": id, "method": "item/tool/call", "params": params }));
        let answer = || {
            self.fake
                .state()
                .sent
                .iter()
                .find(|message| message["id"] == id.as_str() && message.get("method").is_none())
                .cloned()
        };
        until(|| answer().is_some()).await;
        answer().unwrap()["result"].clone()
    }

    async fn tool(&self, name: &str, args: Value) -> Value {
        let index = self.starts().len() - 1;
        self.tool_in(name, args, index, json!({})).await
    }

    /// Codex finishes turn `index`, after a final `send` of `text`.
    async fn complete_turn(&self, index: usize, text: &str) {
        if !text.is_empty() {
            self.tool_in(
                "send",
                json!({ "text": text, "final": true }),
                index,
                json!({}),
            )
            .await;
        }
        self.fake.emit(json!({
            "method": "item/completed",
            "params": {
                "threadId": self.thread_of(index),
                "item": { "type": "agentMessage", "text": text, "phase": "final_answer" },
            },
        }));
        self.end_turn(index, "completed");
    }

    async fn complete(&self) {
        self.complete_turn(self.starts().len() - 1, "DONE").await;
    }

    fn end_turn(&self, index: usize, status: &str) {
        self.fake.emit(json!({
            "method": "turn/completed",
            "params": {
                "threadId": self.thread_of(index),
                "turn": { "id": format!("turn-{}", index + 1), "status": status },
            },
        }));
    }

    fn interrupt(&self) {
        self.end_turn(self.starts().len() - 1, "interrupted");
    }

    fn saved(&self) -> Value {
        locked(&self.storage.0)
            .values()
            .next()
            .cloned()
            .unwrap_or_default()
    }
}

fn key(thread: &str) -> String {
    json!([CHANNEL, thread]).to_string()
}

fn held() -> (Held, oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (started, began) = oneshot::channel();
    let (release, open) = gate();
    (Held { started, release }, began, open)
}

#[tokio::test(flavor = "multi_thread")]
async fn hands_over_promptly_and_runs_conversations_on_one_server() {
    let f = fixture();
    f.run(f.delivery("@Codex first")).await;
    f.starts_reach(1).await;
    assert!(f.published().is_empty());
    f.run(f.delivery_in("@Codex independent", &other_root()))
        .await;
    f.starts_reach(2).await;
    assert_eq!(f.fake.spawned("app-server").len(), 1);
    f.complete_turn(0, "FIRST").await;
    f.complete_turn(1, "OTHER").await;
    f.published_reach(2).await;
    let started = &f.fake.calls("thread/start")[0]["params"];
    assert_eq!(started["model"], "test-model");
    assert_eq!(started["sandbox"], "danger-full-access");
    assert_eq!(started["approvalPolicy"], "never");
    assert!(started.get("baseInstructions").is_none());
    assert!(
        started["developerInstructions"]
            .as_str()
            .unwrap()
            .contains("coding agent in Buzz")
    );
    assert_eq!(
        started["config"],
        json!({
            "web_search": "disabled",
            "mcp_servers": { "company.tools": { "command": "secret-tool", "enabled": false } },
            "features.plugins": false,
            "features.apps": false,
        })
    );
    assert_eq!(started["dynamicTools"][0]["name"], "buzz");
    assert_eq!(started["dynamicTools"][0]["tools"][0]["type"], "function");
    assert_eq!(started["dynamicTools"][0]["tools"][0]["name"], "send");
    let start = &f.starts()[0]["params"];
    assert_eq!(start["model"], "test-model");
    assert_eq!(start["effort"], "low");
    assert_eq!(
        start["additionalContext"]["buzz.instructions"]["kind"],
        "application"
    );
    assert!(
        start["additionalContext"]["buzz.instructions"]["value"]
            .as_str()
            .unwrap()
            .contains("TEST_INSTRUCTIONS")
    );
    let named = &f.fake.calls("thread/name/set")[0]["params"];
    assert_eq!(named["name"], "Buzz #test · thread cccccccc · Codex");
    let input = start["input"][0]["text"].as_str().unwrap();
    assert!(input.contains(&format!("\"session_thread\":\"{}\"", root())));
    let first = &f.published()[0];
    assert_eq!(first.kind, 9);
    assert_eq!(first.content, "FIRST");
    assert!(
        first
            .tags
            .contains(&vec!["e".into(), root(), String::new(), "reply".into()])
    );
    // The shared server starts in the default workspace; each thread
    // carries its agent's own.
    assert_eq!(
        f.fake.spawned("app-server")[0].cwd.as_deref(),
        Some("~/.buzz")
    );
    assert_eq!(started["cwd"], "/tmp/codex-test");
}

/// A second agent on the same runtime, with its own identity and
/// publisher, both running.
fn second(f: &Fixture) -> Arc<FakeAgent> {
    let other = Arc::new(FakeAgent {
        identity: Some(("9".repeat(64), "Other".into())),
        ..Default::default()
    });
    f.runtime.set_relays(HashMap::from([
        (
            agent_key(),
            (SCOPE.into(), f.relay.clone() as Arc<dyn OwnerRelay>),
        ),
        (
            "9".repeat(64),
            (SCOPE.into(), f.relay.clone() as Arc<dyn OwnerRelay>),
        ),
    ]));
    f.runtime.sync(&[agent_key(), "9".repeat(64)]);
    other
}

impl Fixture {
    /// A delivery to `agent` in its own thread.
    fn delivery_for(&self, agent: &Arc<FakeAgent>, content: &str) -> Run {
        Run {
            agent: agent.clone(),
            ..self.delivery_in(content, &"7".repeat(64))
        }
    }

    /// Where the last message with `method` for `thread` was sent, if one
    /// was.
    fn last_sent(&self, method: &str, thread: &Value) -> Option<usize> {
        self.fake
            .state()
            .sent
            .iter()
            .rposition(|wire| wire["method"] == method && wire["params"]["threadId"] == *thread)
    }
}

fn contents_of(agent: &FakeAgent) -> Vec<String> {
    locked(&agent.published)
        .iter()
        .map(|template| template.content.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_every_agent_from_one_server_and_routes_each_tool_call_to_its_own_agent() {
    let f = fixture();
    let other = second(&f);
    f.run(f.delivery("@Codex first")).await;
    f.starts_reach(1).await;
    f.run(f.delivery_for(&other, "@Other second")).await;
    f.starts_reach(2).await;
    assert_eq!(f.fake.spawned("app-server").len(), 1);
    assert!(
        f.starts()[1]["params"]["additionalContext"]["buzz.instructions"]["value"]
            .as_str()
            .unwrap()
            .contains("Other")
    );
    f.complete_turn(1, "FROM OTHER").await;
    f.complete_turn(0, "FROM CODEX").await;
    f.published_reach(1).await;
    until(|| locked(&other.published).len() == 1).await;
    assert_eq!(f.contents(), ["FROM CODEX"]);
    assert_eq!(contents_of(&other), ["FROM OTHER"]);
    until(|| {
        f.runtime
            .sessions(&"9".repeat(64))
            .first()
            .is_some_and(|view| view.status == "Idle")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stops_a_removed_agents_turn_without_ending_the_server_the_others_share() {
    let f = fixture();
    let other = second(&f);
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.run(f.delivery_for(&other, "other work")).await;
    f.starts_reach(2).await;
    let removed = f.thread_of(1);
    f.runtime.sync(&[agent_key()]);
    until(|| f.last_sent("thread/unsubscribe", &removed).is_some()).await;
    for method in ["turn/interrupt", "thread/backgroundTerminals/clean"] {
        assert_eq!(f.fake.calls(method)[0]["params"]["threadId"], removed);
    }
    assert_eq!(f.fake.state().ended, 0);
    assert!(f.runtime.sessions(&"9".repeat(64)).is_empty());
    f.complete_turn(0, "STILL HERE").await;
    f.published_reach(1).await;
    assert_eq!(f.contents(), ["STILL HERE"]);
    assert!(locked(&other.published).is_empty());
    f.runtime.sync(&[]);
    until(|| f.fake.state().ended == 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_a_stopped_agents_leftover_shells_and_lets_it_start_again_only_after_letting_go() {
    let f = fixture();
    let other = second(&f);
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.run(f.delivery_for(&other, "start a dev server")).await;
    f.starts_reach(2).await;
    let thread = f.thread_of(1);
    f.complete_turn(1, "SERVER RUNNING").await;
    until(|| locked(&other.published).len() == 1).await;
    f.run(f.delivery_for(&other, "busy again")).await;
    f.starts_reach(3).await;
    let before = f.last_sent("thread/unsubscribe", &thread).unwrap();
    f.runtime.sync(&[agent_key()]);
    f.runtime.sync(&[agent_key(), "9".repeat(64)]);
    f.run(f.delivery_for(&other, "back")).await;
    f.starts_reach(4).await;
    let unsubscribed = f.last_sent("thread/unsubscribe", &thread).unwrap();
    let cleaned = f
        .last_sent("thread/backgroundTerminals/clean", &thread)
        .unwrap();
    let resumed = f.last_sent("thread/resume", &thread).unwrap();
    assert!(unsubscribed > before);
    assert!(cleaned > unsubscribed);
    assert!(resumed > cleaned);
    assert_eq!(f.fake.state().ended, 0);
    f.complete_turn(3, "BACK").await;
    f.complete_turn(0, "STILL HERE").await;
    f.published_reach(1).await;
    until(|| locked(&other.published).len() == 2).await;
    assert_eq!(contents_of(&other), ["SERVER RUNNING", "BACK"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn reopens_the_shared_server_after_it_exits() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.fake.exit(Some(3), "");
    f.published_reach(1).await;
    f.run(f.delivery("again")).await;
    until(|| f.fake.spawned("app-server").len() == 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn serializes_follow_ups_and_replies_to_the_latest_accepted_request() {
    let f = fixture();
    f.run(f.delivery("first")).await;
    f.starts_reach(1).await;
    let (wait, open) = gate();
    f.relay.hold(wait, Ok(vec![]));
    f.run(f.delivery("🤖 @Codex FIRST_STEER")).await;
    f.run(f.delivery("SECOND_STEER")).await;
    until(|| f.relay.reads() == 2).await;
    flush().await;
    assert!(f.published().is_empty());
    assert_eq!(f.steers(), 0);
    open.send(()).unwrap();
    f.steered(2).await;
    f.complete().await;
    f.published_reach(1).await;
    let steering = f.fake.calls("turn/steer");
    let text = |index: usize| steering[index]["params"]["input"][0]["text"].to_string();
    assert!(text(0).contains("Request: \\\"FIRST_STEER\\\""));
    assert!(text(1).contains("SECOND_STEER"));
    assert!(text(0).contains("<new-message-arrived-while-you-were-working>"));
    assert_eq!(steering[0]["params"]["expectedTurnId"], "turn-1");
    // The reply goes to the thread, not under the latest mention.
    assert!(f.published()[0].tags.contains(&vec![
        "e".into(),
        root(),
        String::new(),
        "reply".into()
    ]));
}

/// Reads with the `read` tool in each scope, after a request and a
/// follow-up in `latest`.
async fn reads_follow_the_latest_conversation(dm: bool, scope: &str) {
    let whole = dm;
    let f = fixture();
    *locked(&f.relay.dm) = dm;
    f.event("OTHER_THREAD_CONTEXT", &other_root());
    let config = json!({ "workspace": "/tmp/codex-test", "scope": scope });
    f.run(f.delivery_with("CURRENT_REQUEST", &root(), config.clone()))
        .await;
    f.starts_reach(1).await;
    let initial = f.tool("read", json!({})).await.to_string();
    assert!(initial.contains("CURRENT_REQUEST"), "{initial}");
    assert_eq!(initial.contains("OTHER_THREAD_CONTEXT"), whole, "{initial}");
    let latest = if scope == "thread" && !whole {
        root()
    } else {
        other_root()
    };
    f.run(f.delivery_with("LATEST_REQUEST", &latest, config))
        .await;
    f.steered(1).await;
    let after = f.tool("read", json!({})).await.to_string();
    assert!(after.contains("LATEST_REQUEST"), "{after}");
    assert_eq!(
        after.contains("CURRENT_REQUEST"),
        whole || scope == "thread"
    );
    assert_eq!(
        after.contains("OTHER_THREAD_CONTEXT"),
        whole || scope == "channel"
    );
    f.complete().await;
    f.status_reaches("Idle").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn buzz_reads_default_to_the_latest_conversation() {
    reads_follow_the_latest_conversation(true, "thread").await;
    reads_follow_the_latest_conversation(false, "channel").await;
    reads_follow_the_latest_conversation(false, "thread").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn interruption_cleans_terminals_and_the_thread_recovers() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.interrupt();
    f.status_reaches("Interrupted").await;
    assert_eq!(f.fake.calls("thread/backgroundTerminals/clean").len(), 1);
    assert_eq!(f.starts().len(), 1);
    assert!(f.contents()[0].contains("turn was interrupted"));
    f.run(f.delivery("recover")).await;
    f.starts_reach(2).await;
    f.complete().await;
    f.published_reach(2).await;
    assert_eq!(f.fake.calls("thread/resume").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn closes_a_server_returned_after_disposal() {
    let f = fixture();
    let (hold, began, open) = held();
    f.fake.state().hold_server = Some(hold);
    f.run(f.delivery("work")).await;
    began.await.unwrap();
    f.runtime.dispose();
    open.send(()).unwrap();
    until(|| f.fake.state().ended == 1).await;
    flush().await;
    assert!(f.starts().is_empty());
    assert!(f.published().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ignores_other_peoples_messages_without_spawning() {
    let f = fixture();
    let mut run = f.delivery("work");
    if let Wake::Mention { event } = &mut run.wake {
        event.pubkey = "f".repeat(64);
    }
    f.run(run).await;
    flush().await;
    assert!(f.fake.state().spawns.is_empty());
    assert!(f.published().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn timers_and_unready_communities_are_refused() {
    let f = fixture();
    let mut run = f.delivery("work");
    run.wake = Wake::Timer;
    assert_eq!(
        f.runtime.run(run).await,
        Err("Codex timers are not supported yet; use mentions or event watches.".into())
    );
    f.runtime.set_relays(HashMap::new());
    assert_eq!(
        f.runtime.run(f.delivery("work")).await,
        Err("Connect to the agent's community first".into())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_an_exit_and_a_turn_without_a_final_reply() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.complete_turn(0, "").await;
    f.published_reach(1).await;
    assert!(f.contents()[0].contains("without a final reply"));
    f.run(f.delivery("next")).await;
    f.starts_reach(2).await;
    f.fake.exit(Some(3), "");
    f.published_reach(2).await;
    assert_eq!(f.contents()[1], "Codex could not finish: Codex exited (3).");
    f.status_reaches("Needs attention").await;
    // The next mention starts a new server.
    f.run(f.delivery("again")).await;
    f.starts_reach(3).await;
    assert_eq!(f.fake.spawned("app-server").len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn ends_the_server_when_its_agent_stops_running_and_drops_its_late_answer() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.runtime.sync(&[]);
    f.end_turn(0, "completed");
    until(|| f.fake.state().ended == 1).await;
    flush().await;
    assert!(f.published().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn work_stays_alive_while_its_agent_keeps_running() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    // Another community selected, or a reconnect: the agent still runs.
    f.runtime.sync(&[agent_key()]);
    f.complete().await;
    f.published_reach(1).await;
    assert_eq!(f.fake.state().ended, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn surviving_terminals_fail_the_interruption() {
    let f = fixture();
    f.fake.state().terminals = vec![json!({ "processId": "42" })];
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.interrupt();
    f.published_reach(1).await;
    assert!(f.contents()[0].contains("background terminals could not be stopped"));
    f.status_reaches("Needs attention").await;
    until(|| f.fake.state().ended == 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn waits_for_terminals_to_end_before_reporting_an_interruption() {
    let f = fixture();
    let (termination, open) = gate();
    {
        let mut state = f.fake.state();
        state.terminals = vec![json!({ "processId": "42" })];
        state.termination = Some(termination);
    }
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.interrupt();
    until(|| {
        !f.fake
            .calls("thread/backgroundTerminals/terminate")
            .is_empty()
    })
    .await;
    flush().await;
    assert!(f.published().is_empty());
    open.send(()).unwrap();
    f.published_reach(1).await;
    assert!(f.contents()[0].contains("turn was interrupted"));
    assert_eq!(f.fake.state().ended, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn restores_a_saved_binding_and_starts_fresh_in_another_workspace() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.complete().await;
    until(|| !f.fake.calls("thread/unsubscribe").is_empty()).await;
    assert_eq!(
        *locked(&f.storage.0),
        HashMap::from([(
            format!("buzz.codex.sessions.v3:{SCOPE}:{}", agent_key()),
            json!({ key(&root()): { "threadId": "thread-1", "workspace": "/tmp/codex-test" } }),
        )])
    );
    f.runtime.dispose();
    let restored = fixture_with(f.storage.clone());
    restored.run(restored.delivery("continue")).await;
    restored.starts_reach(1).await;
    let resumed = &restored.fake.calls("thread/resume")[0]["params"];
    assert_eq!(resumed["threadId"], "thread-1");
    assert_eq!(resumed["excludeTurns"], true);
    assert!(resumed.get("dynamicTools").is_none());
    restored.complete().await;
    restored.status_reaches("Idle").await;
    // Saved settings that are blank use Codex's defaults.
    let defaults = restored.delivery_with(
        "defaults",
        &root(),
        json!({ "workspace": "/tmp/codex-test" }),
    );
    restored.run(defaults).await;
    restored.starts_reach(2).await;
    assert_eq!(restored.starts()[1]["params"]["model"], "test-model");
    assert_eq!(restored.starts()[1]["params"]["effort"], "medium");
    restored.complete().await;
    restored.published_reach(2).await;
    let moved = restored.delivery_with(
        "new workspace",
        &root(),
        json!({ "workspace": "/tmp/different-workspace" }),
    );
    restored.run(moved).await;
    restored.starts_reach(3).await;
    assert_eq!(restored.fake.calls("thread/start").len(), 1);
    assert_eq!(restored.fake.calls("thread/resume").len(), 2);
    restored.complete().await;
    restored.published_reach(3).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relative_workspace_is_reported_without_starting() {
    let f = fixture();
    let run = f.delivery_with("hello", &root(), json!({ "workspace": "relative" }));
    f.run(run).await;
    assert!(f.contents()[0].contains("absolute workspace"));
    assert!(f.fake.state().spawns.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn blank_workspaces_run_in_the_resolved_default() {
    for config in [
        json!({}),
        json!({ "workspace": "" }),
        json!({ "workspace": "  " }),
    ] {
        let f = fixture();
        f.run(f.delivery_with("hello", &root(), config.clone()))
            .await;
        f.starts_reach(1).await;
        assert_eq!(
            f.fake.spawned("app-server")[0].cwd.as_deref(),
            Some("~/.buzz")
        );
        assert_eq!(
            f.fake.spawned("workspace")[0].cwd.as_deref(),
            Some("~/.buzz")
        );
        for method in ["config/read", "thread/start"] {
            assert_eq!(f.fake.calls(method)[0]["params"]["cwd"], "/home/test/.buzz");
        }
        f.complete().await;
        f.published_reach(1).await;
        f.run(f.delivery_with("again", &root(), config)).await;
        f.starts_reach(2).await;
        assert_eq!(
            f.fake.calls("thread/resume")[0]["params"]["cwd"],
            "/home/test/.buzz"
        );
        f.complete().await;
        f.published_reach(2).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_a_default_workspace_that_cannot_be_resolved() {
    let f = fixture();
    f.fake.state().workspace_exit = 1;
    f.run(f.delivery_with("hello", &root(), json!({}))).await;
    f.published_reach(1).await;
    assert!(f.contents()[0].contains("Could not resolve the default Codex workspace"));
    assert!(f.starts().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_started_elsewhere_still_uses_the_default() {
    let f = fixture();
    f.run(f.delivery("custom")).await;
    f.starts_reach(1).await;
    f.complete_turn(0, "CUSTOM").await;
    f.published_reach(1).await;
    f.run(f.delivery_with("default", &root(), json!({}))).await;
    f.starts_reach(2).await;
    let cwds: Vec<Value> = f
        .fake
        .calls("thread/start")
        .iter()
        .map(|call| call["params"]["cwd"].clone())
        .collect();
    assert_eq!(cwds, [json!("/tmp/codex-test"), json!("/home/test/.buzz")]);
    f.complete_turn(1, "DEFAULT").await;
    f.published_reach(2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn posts_what_tools_send_and_never_native_text() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    f.tool("send", json!({ "text": "PART ONE" })).await;
    f.tool("send", json!({ "text": "PART TWO" })).await;
    f.complete_turn(0, "FINAL").await;
    f.status_reaches("Idle").await;
    assert_eq!(f.contents(), ["PART ONE", "PART TWO", "FINAL"]);
    assert!(f.fake.calls("turn/interrupt").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_follow_up_during_startup_steers_once_the_turn_starts() {
    let f = fixture();
    let (hold, began, open) = held();
    f.fake.state().hold_server = Some(hold);
    f.run(f.delivery("first")).await;
    began.await.unwrap();
    // The first turn holds its lane before the follow-up arrives, or the
    // follow-up queues as a turn of its own.
    until(|| !f.actives().is_empty()).await;
    f.run(f.delivery("during startup")).await;
    flush().await;
    assert!(f.starts().is_empty());
    open.send(()).unwrap();
    f.steered(1).await;
    assert_eq!(f.starts().len(), 1);
    f.complete().await;
    f.published_reach(1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_completion_racing_a_follow_up_starts_the_next_turn() {
    let f = fixture();
    f.run(f.delivery("first")).await;
    f.starts_reach(1).await;
    let (wait, open) = gate();
    f.relay.hold(wait, Ok(vec![]));
    f.run(f.delivery("follow-up")).await;
    until(|| f.relay.reads() == 2).await;
    // The turn waits for steering in flight before it settles. Waiting for
    // the runtime to see the completion makes it race the follow-up, as
    // the test means; otherwise the follow-up may steer first and only be
    // refused as too late.
    f.complete_turn(0, "FIRST").await;
    until(|| f.actives().iter().all(|active| active.finished())).await;
    flush().await;
    assert_eq!(f.status().as_deref(), Some("Working"));
    open.send(()).unwrap();
    f.starts_reach(2).await;
    assert_eq!(f.steers(), 0);
    f.complete_turn(1, "SECOND").await;
    f.published_reach(2).await;
    assert_eq!(f.contents()[1], "SECOND");
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_commands_are_ordinary_steering_text() {
    for command in ["/queue", "/stop", "/steer", "/reset"] {
        let f = fixture();
        f.run(f.delivery("work")).await;
        f.starts_reach(1).await;
        let content = format!("{command} ordinary text");
        f.run(f.delivery(&content)).await;
        f.steered(1).await;
        let steer = &f.fake.calls("turn/steer")[0];
        assert!(
            steer["params"]["input"][0]["text"]
                .as_str()
                .unwrap()
                .contains(&format!("Request: {}", json!(content)))
        );
        assert!(f.published().is_empty());
        f.complete().await;
        f.published_reach(1).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bindings_from_before_tools_start_fresh() {
    let storage = Arc::new(MemoryStorage::default());
    storage
        .set(
            &format!("buzz.codex.sessions.v2:{SCOPE}:{}", agent_key()),
            json!({ key(&root()): { "threadId": "legacy-thread", "workspace": "/tmp/codex-test" } }),
        )
        .unwrap();
    let f = fixture_with(storage);
    f.run(f.delivery("new work")).await;
    f.starts_reach(1).await;
    assert!(f.fake.calls("thread/resume").is_empty());
    f.complete().await;
    f.published_reach(1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_first_start_stays_unbound() {
    let f = fixture();
    let (wait, open) = gate();
    f.relay.hold(wait, Err("Relay history unavailable".into()));
    f.run(f.delivery("first")).await;
    until(|| f.relay.reads() == 1).await;
    assert!(locked(&f.storage.0).is_empty());
    assert!(f.starts().is_empty());
    open.send(()).unwrap();
    f.published_reach(1).await;
    assert!(f.contents()[0].contains("Relay history unavailable"));
    f.run(f.delivery("retry")).await;
    f.starts_reach(1).await;
    assert_eq!(f.fake.calls("thread/start").len(), 2);
    assert!(f.fake.calls("thread/resume").is_empty());
    f.complete().await;
    f.published_reach(2).await;
    assert_eq!(f.saved()[key(&root())]["threadId"], "thread-2");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_missing_saved_thread_starts_fresh() {
    for (error, recover) in [
        ("no rollout found for thread id thread-1", true),
        ("Permission denied reading saved thread", false),
    ] {
        let f = fixture();
        f.run(f.delivery("first")).await;
        f.starts_reach(1).await;
        f.complete().await;
        f.published_reach(1).await;
        f.status_reaches("Idle").await;
        f.fake
            .state()
            .errors
            .insert("thread/resume".into(), error.into());
        f.run(f.delivery("follow-up")).await;
        if recover {
            f.starts_reach(2).await;
            assert_eq!(f.starts()[1]["params"]["threadId"], "thread-2");
            f.complete().await;
        }
        f.published_reach(2).await;
        if recover {
            assert_eq!(f.contents()[1], "DONE");
        } else {
            assert!(f.contents()[1].contains(error));
        }
        let thread = if recover { "thread-2" } else { "thread-1" };
        assert_eq!(f.saved()[key(&root())]["threadId"], thread);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_calls_are_routed_by_thread_and_turn() {
    let f = fixture();
    f.run(f.delivery("first")).await;
    f.run(f.delivery_in("other", &other_root())).await;
    f.starts_reach(2).await;
    let first = f
        .starts()
        .iter()
        .position(|start| {
            start["params"]["input"][0]["text"]
                .to_string()
                .contains("first")
        })
        .unwrap();
    for overrides in [
        json!({ "turnId": "stale" }),
        json!({ "threadId": "missing" }),
        json!({ "namespace": "other" }),
        json!({ "tool": "unknown" }),
        json!({ "arguments": [] }),
    ] {
        let answer = f
            .tool_in("send", json!({ "text": "must not send" }), first, overrides)
            .await;
        assert_eq!(answer["success"], false);
        assert_eq!(answer["contentItems"][0]["type"], "inputText");
    }
    assert!(f.published().is_empty());
    let other = 1 - first;
    f.complete_turn(other, "OTHER").await;
    f.complete_turn(first, "FIRST").await;
    until(|| {
        let views = f.runtime.sessions(&agent_key());
        views.len() == 2 && views.iter().all(|view| view.status == "Idle")
    })
    .await;
    let published = f.published();
    assert_eq!(published[0].content, "OTHER");
    assert!(published[0].tags.contains(&vec![
        "e".into(),
        other_root(),
        String::new(),
        "reply".into()
    ]));
    assert_eq!(published[1].content, "FIRST");
    assert!(
        published[1]
            .tags
            .contains(&vec!["e".into(), root(), String::new(), "reply".into()])
    );
    // A turn that has ended has no tools.
    let late = f
        .tool_in("send", json!({ "text": "late" }), first, json!({}))
        .await;
    assert_eq!(late["success"], false);
    assert_eq!(f.published().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn file_tools_use_the_launched_workspace_and_memory_the_owners_view() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    let moved = f.delivery_with(
        "follow-up",
        &root(),
        json!({ "workspace": "/tmp/different" }),
    );
    f.run(moved).await;
    f.steered(1).await;
    let set = f
        .tool(
            "mem_set",
            json!({ "slug": "mem/test", "text": "remembered" }),
        )
        .await;
    assert_eq!(set["success"], true);
    let got = f.tool("mem_get", json!({ "slug": "mem/test" })).await;
    assert_eq!(got["success"], true);
    assert_eq!(got["contentItems"][0]["text"], "remembered");
    assert_eq!(
        *locked(&f.agent.remembered),
        [("mem/test".to_string(), "remembered".to_string(), 0)]
    );
    assert_eq!(*locked(&f.relay.memory_reads), 2);
    let sent = f
        .tool(
            "send",
            json!({ "path": "answer.txt", "files": ["attachment.jpg"], "final": true }),
        )
        .await;
    assert_eq!(sent["success"], true, "{sent}");
    f.end_turn(0, "completed");
    f.status_reaches("Idle").await;
    let mut reads = f.fake.state().reads.clone();
    reads.sort();
    assert_eq!(
        reads,
        [
            (
                Some("/tmp/codex-test".to_string()),
                "answer.txt".to_string()
            ),
            (
                Some("/tmp/codex-test".to_string()),
                "attachment.jpg".to_string()
            ),
        ]
    );
    assert_eq!(
        *locked(&f.agent.uploads),
        [(vec![0xff, 0xd8, 0xff], "image/jpeg".to_string())]
    );
    let published = &f.published()[0];
    assert!(published.content.contains("FILE CONTENT"));
    assert!(
        published
            .tags
            .contains(&vec!["e".into(), root(), String::new(), "reply".into()])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_send_goes_back_to_codex_without_ending_its_turn() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    *locked(&f.agent.reject) = 1;
    let answer = f
        .tool("send", json!({ "text": "answer", "final": true }))
        .await;
    assert_eq!(
        answer,
        json!({ "success": false, "contentItems": [{ "type": "inputText", "text": "Signing rejected" }] })
    );
    assert!(f.fake.calls("turn/interrupt").is_empty());
    f.complete().await;
    f.status_reaches("Idle").await;
    assert_eq!(f.published().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_follow_up_pending_after_a_final_send_gets_its_own_answer() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    let (wait, open) = gate();
    f.relay.hold(wait, Ok(vec![]));
    f.run(f.delivery("new request")).await;
    until(|| f.relay.reads() == 2).await;
    f.tool("send", json!({ "text": "prior answer", "final": true }))
        .await;
    assert!(f.fake.calls("turn/interrupt").is_empty());
    open.send(()).unwrap();
    f.steered(1).await;
    f.complete_turn(0, "NEW ANSWER").await;
    f.status_reaches("Idle").await;
    assert_eq!(f.contents(), ["prior answer", "NEW ANSWER"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_final_send_finishes_normally_and_keeps_background_terminals() {
    let f = fixture();
    f.fake.state().terminals = vec![json!({ "processId": "dev-server" })];
    f.run(f.delivery("start server")).await;
    f.starts_reach(1).await;
    let reply = f
        .tool(
            "send",
            json!({ "text": "http://localhost:3000", "final": true }),
        )
        .await;
    assert_eq!(reply["success"], true);
    assert_eq!(f.published().len(), 1);
    assert_eq!(f.status().as_deref(), Some("Working"));
    f.end_turn(0, "completed");
    until(|| !f.fake.calls("thread/unsubscribe").is_empty()).await;
    f.status_reaches("Idle").await;
    assert!(!f.fake.state().sent.iter().any(|message| {
        message["method"]
            .as_str()
            .is_some_and(|method| method.starts_with("thread/backgroundTerminals/"))
    }));
    assert_eq!(f.fake.state().ended, 0);
    assert_eq!(f.published().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_reader_in_flight_stops_with_its_turn() {
    for late in [false, true] {
        let f = fixture();
        f.run(f.delivery("work")).await;
        f.starts_reach(1).await;
        let (hold, began, open) = held();
        f.fake.state().hold_read = Some(hold);
        // A reader that started in time stays open until it is killed.
        let late_open = if late {
            Some(open)
        } else {
            open.send(()).unwrap();
            None
        };
        let reading = f.tool_in(
            "send",
            json!({ "path": "answer.txt", "final": true }),
            0,
            json!({}),
        );
        let interrupt = async {
            began.await.unwrap();
            f.interrupt();
            f.status_reaches("Interrupted").await;
            if let Some(open) = late_open {
                open.send(()).unwrap();
            }
        };
        // The reader's start is awaited with no deadline of its own; a
        // tool refused before it reads would leave this waiting forever.
        let (answer, ()) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(reading, interrupt)
        })
        .await
        .expect("the reader starts and stops with its turn");
        assert_eq!(answer["success"], false);
        until(|| f.fake.state().reads_killed == 1).await;
        assert_eq!(f.published().len(), 1);
        assert!(f.contents()[0].contains("turn was interrupted"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn conversations_report_commands_as_they_run() {
    let f = fixture();
    f.run(f.delivery("work")).await;
    f.starts_reach(1).await;
    let thread = f.thread_of(0);
    f.fake.emit(json!({
        "method": "item/started",
        "params": { "threadId": thread, "item": { "type": "commandExecution", "command": "ls" } },
    }));
    until(|| {
        f.runtime
            .sessions(&agent_key())
            .first()
            .is_some_and(|view| view.detail == "ls")
    })
    .await;
    f.fake.emit(json!({
        "method": "item/completed",
        "params": {
            "threadId": thread,
            "item": { "type": "commandExecution", "command": "ls", "aggregatedOutput": "a.txt" },
        },
    }));
    until(|| {
        f.runtime
            .sessions(&agent_key())
            .first()
            .is_some_and(|view| view.detail == "ls\na.txt")
    })
    .await;
    assert_eq!(f.runtime.sessions(&agent_key())[0].key, key(&root()));
    f.complete().await;
    f.status_reaches("Idle").await;
}
