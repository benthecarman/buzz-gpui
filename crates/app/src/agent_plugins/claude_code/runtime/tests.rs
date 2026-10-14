//! buzz-app's `runtime.test.ts`.

use super::*;
use crate::agent_plugins::claude_code::claude::testing::{FakeClaude, FakeSpawn, flush};
use crate::agent_plugins::claude_code::sessions::SavedSession;
use crate::agent_plugins::owner_relay::ChannelInfo;
use crate::buzz_mcp::{ClientResult, Upload};

fn own() -> String {
    "c".repeat(64)
}
fn alice() -> String {
    "a".repeat(64)
}
const CHANNEL: &str = "chan-1";

fn message(id: &str, content: &str, change: impl FnOnce(&mut BuzzEvent)) -> BuzzEvent {
    let mut event = BuzzEvent {
        id: id.repeat(64)[..64].to_string(),
        pubkey: alice(),
        kind: 9,
        created_at: 100,
        content: content.into(),
        tags: vec![vec!["h".into(), CHANNEL.into()]],
        sig: String::new(),
    };
    change(&mut event);
    event
}

fn tags(list: &[&[&str]]) -> Vec<Vec<String>> {
    list.iter()
        .map(|tag| tag.iter().map(|part| part.to_string()).collect())
        .collect()
}

#[derive(Default)]
struct FakeRelay {
    thread: Mutex<Vec<BuzzEvent>>,
}

impl OwnerRelay for FakeRelay {
    fn channel(&self, id: &str) -> Option<ChannelInfo> {
        (id == CHANNEL).then(|| ChannelInfo {
            name: Some("general".into()),
            dm: false,
            members: vec![],
        })
    }
    fn read(&self, _: Vec<Value>) -> BoxFuture<'static, Result<Vec<BuzzEvent>, String>> {
        let thread = locked(&self.thread).clone();
        Box::pin(async move { Ok(thread) })
    }
    fn names(
        &self,
        pubkeys: Vec<String>,
        _: Vec<String>,
    ) -> BoxFuture<'static, HashMap<String, String>> {
        let names = pubkeys
            .into_iter()
            .filter(|key| *key == alice())
            .map(|key| (key, "Alice".to_string()))
            .collect();
        Box::pin(async move { names })
    }
    fn memories(&self, _: &str) -> BoxFuture<'static, Result<Vec<Memory>, String>> {
        Box::pin(async { Ok(vec![]) })
    }
}

#[derive(Default)]
struct FakeAgent {
    published: Mutex<Vec<Template>>,
}

impl AgentActions for FakeAgent {
    fn pubkey(&self) -> String {
        own()
    }
    fn owner(&self) -> String {
        alice()
    }
    fn name(&self) -> String {
        "Claude".into()
    }
    fn publish(&self, template: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>> {
        let mut published = locked(&self.published);
        published.push(template.clone());
        let event = message(&published.len().to_string(), &template.content, |event| {
            event.kind = template.kind;
            event.tags = template.tags;
            event.pubkey = own();
        });
        Box::pin(async move { Ok(event) })
    }
    fn query(&self, _: Vec<Value>) -> BoxFuture<'static, ClientResult<Vec<BuzzEvent>>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn upload(&self, _: Vec<u8>, _: String) -> BoxFuture<'static, ClientResult<Upload>> {
        Box::pin(async { Err("no uploads".into()) })
    }
    fn remember(&self, _: String, _: String, _: u64) -> BoxFuture<'static, ClientResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Default)]
struct MemoryStore(Mutex<HashMap<String, SavedSession>>);

impl SessionStore for MemoryStore {
    fn get(&self, key: &str) -> Option<SavedSession> {
        locked(&self.0).get(key).cloned()
    }
    fn set(&self, key: &str, session: SavedSession) {
        locked(&self.0).insert(key.into(), session);
    }
    fn delete(&self, key: &str) {
        locked(&self.0).remove(key);
    }
}

struct Setup {
    runtime: ClaudeRuntime,
    fake: FakeSpawn,
    relay: Arc<FakeRelay>,
    agent: Arc<FakeAgent>,
    store: Arc<MemoryStore>,
}

impl Setup {
    fn new(thread: Vec<BuzzEvent>) -> Self {
        let store = Arc::new(MemoryStore::default());
        let shared = store.clone();
        let runtime =
            ClaudeRuntime::with_store(Arc::new(move |_: &str| -> Arc<dyn SessionStore> {
                shared.clone()
            }));
        let relay = Arc::new(FakeRelay {
            thread: Mutex::new(thread),
        });
        runtime.set_relays(HashMap::from([
            (own(), relay.clone() as Arc<dyn OwnerRelay>),
            (alice(), relay.clone() as Arc<dyn OwnerRelay>),
        ]));
        Self {
            runtime,
            fake: FakeSpawn::default(),
            relay,
            agent: Arc::default(),
            store,
        }
    }
    fn deliver(&self, wake: Wake, config: Value) -> impl std::future::Future<Output = ()> + use<> {
        let runtime = self.runtime.clone();
        let run = Run {
            wake,
            channel: Some(CHANNEL.into()),
            agent: self.agent.clone(),
            config,
            spawn: self.fake.spawner(),
            model: None,
            effort: None,
            working: std::sync::Arc::new(|_| {}),
        };
        async move { runtime.run(run).await }
    }
    async fn mention(&self, event: &BuzzEvent) {
        self.deliver(
            Wake::Mention {
                event: event.clone(),
            },
            Config::default().value(),
        )
        .await
    }
    fn sync(&self, config: Config) {
        self.runtime.sync(vec![Synced {
            pubkey: own(),
            config: config.value(),
            spawn: self.fake.spawner(),
        }]);
    }
    fn used(&self) -> Vec<FakeClaude> {
        self.fake
            .claudes()
            .into_iter()
            .filter(|process| !process.prompts().is_empty())
            .collect()
    }
    fn published(&self) -> Vec<Template> {
        locked(&self.agent.published).clone()
    }
}

fn thread_root_and_mention() -> (BuzzEvent, BuzzEvent) {
    let root = message("1", "Build is red", |_| {});
    let mention = message("2", "@Claude can you look?", |event| {
        event.created_at = 110;
        event.tags = tags(&[
            &["h", CHANNEL],
            &["e", &root.id, "", "reply"],
            &["p", &own()],
        ]);
    });
    (root, mention)
}

fn tool_call(id: &str, arguments: Value) -> Value {
    json!({
        "type": "control_request", "request_id": id,
        "request": {
            "subtype": "mcp_message", "server_name": "buzz",
            "message": {
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": { "name": "send", "arguments": arguments },
            },
        },
    })
}

#[tokio::test]
async fn hands_a_mention_to_its_threads_session() {
    let (root, mention) = thread_root_and_mention();
    let setup = Setup::new(vec![root.clone(), mention.clone()]);
    setup.mention(&mention).await;
    flush().await;
    let claude = setup.used()[0].clone();
    let prompt = claude.prompts()[0].clone();
    assert!(prompt.contains(&format!("Thread root: {}", root.id)));
    assert!(prompt.contains("<thread-context included=\"1\" total=\"2\""));
    assert!(prompt.contains(&format!(
        "Alice ({}) (1970-01-01T00:01:40.000Z): Build is red",
        alice()
    )));
    assert!(prompt.contains("Content: @Claude can you look?"));
    // The process holds no key: it acts through the tools.
    assert!(claude.args().contains(&"--mcp-config".into()));
    assert_eq!(claude.state().options.cwd.as_deref(), Some("~/.buzz"));
    assert!(setup.published().is_empty());
}

#[tokio::test]
async fn disposing_waits_for_the_processes_to_exit() {
    let (root, mention) = thread_root_and_mention();
    let setup = Setup::new(vec![root, mention.clone()]);
    setup.mention(&mention).await;
    flush().await;
    let stopped = setup.runtime.dispose_and_wait();
    tokio::time::timeout(Duration::from_secs(5), stopped)
        .await
        .unwrap();
    assert!(setup.used().iter().all(FakeClaude::killed));
}

#[tokio::test]
async fn answers_claudes_tool_calls_as_the_agent_in_its_thread() {
    let (root, mention) = thread_root_and_mention();
    let setup = Setup::new(vec![root.clone(), mention.clone()]);
    setup.mention(&mention).await;
    flush().await;
    let claude = setup.used()[0].clone();
    claude.emit(tool_call("call-1", json!({ "text": "Looking now" })));
    flush().await;
    assert_eq!(
        setup.published(),
        vec![Template {
            kind: 9,
            content: "Looking now".into(),
            tags: tags(&[&["h", CHANNEL], &["e", &root.id, "", "reply"]]),
        }]
    );
    let answer = claude
        .responses()
        .into_iter()
        .find(|response| response["request_id"] == "call-1")
        .unwrap();
    assert_eq!(answer["subtype"], "success");
    assert_eq!(answer["response"]["mcp_response"]["id"], 7);
    assert!(answer["response"]["mcp_response"]["result"].is_object());
}

#[tokio::test]
async fn reads_a_busy_turns_files_from_the_workspace_it_started_in() {
    let mention = message("2", "@Claude write a report", |event| {
        event.tags = tags(&[&["h", CHANNEL], &["p", &own()]]);
    });
    let setup = Setup::new(vec![]);
    setup.sync(Config::default());
    flush().await;
    let spare = setup.fake.claudes()[0].clone();
    spare.set_hold(true);
    setup.mention(&mention).await;
    flush().await;
    // Saved mid-turn: the busy process keeps working where it started.
    setup.sync(Config {
        workspace: "/new".into(),
        ..Config::default()
    });
    flush().await;
    assert!(!spare.killed());
    spare.emit(tool_call("call-1", json!({ "path": "report.md" })));
    flush().await;
    let reads: Vec<(Vec<String>, Option<String>)> = setup
        .fake
        .all()
        .iter()
        .filter(|process| process.state().id == "read")
        .map(|process| (process.args(), process.state().options.cwd.clone()))
        .collect();
    assert_eq!(
        reads,
        vec![(vec!["report.md".to_string()], Some("~/.buzz".into()))]
    );
}

#[tokio::test]
async fn shows_a_threads_later_turns_only_what_the_session_has_not_seen() {
    let root = message("1", "first", |event| {
        event.tags = tags(&[&["h", CHANNEL], &["p", &own()]]);
    });
    let reply = message("3", "my answer", |event| {
        event.pubkey = own();
        event.created_at = 105;
        event.tags = tags(&[&["h", CHANNEL], &["e", &root.id, "", "reply"]]);
    });
    let later = message("4", "@Claude thanks, one more", |event| {
        event.created_at = 120;
        event.tags = tags(&[&["h", CHANNEL], &["e", &root.id, "", "reply"]]);
    });
    let setup = Setup::new(vec![]);
    setup.mention(&root).await;
    flush().await;
    *locked(&setup.relay.thread) = vec![root.clone(), reply, later.clone()];
    setup.mention(&later).await;
    flush().await;
    let used = setup.used();
    assert_eq!(used.len(), 1);
    let second = used[0].prompts()[1].clone();
    assert!(!second.contains("<thread-context"));
    assert!(second.contains("Earlier context is already available in this session."));
}

#[tokio::test]
async fn says_so_in_the_thread_when_a_turn_fails() {
    let mention = message("2", "@Claude hi", |event| {
        event.tags = tags(&[&["h", CHANNEL], &["p", &own()]]);
    });
    let setup = Setup::new(vec![]);
    setup.sync(Config::default());
    flush().await;
    let spare = setup.fake.claudes()[0].clone();
    spare.set_hold(true);
    setup.mention(&mention).await;
    flush().await;
    assert_eq!(spare.prompts().len(), 1);
    spare.exit(Some(1), Some("Error: Invalid API key\n"));
    flush().await;
    let report = setup
        .published()
        .into_iter()
        .find(|event| event.kind == 9)
        .unwrap();
    assert_eq!(
        report.content,
        "⚠️ I couldn't finish that: Error: Invalid API key"
    );
    assert_eq!(
        report.tags,
        tags(&[
            &["h", CHANNEL],
            &["e", &mention.id, "", "reply"],
            &["p", &alice()]
        ])
    );
}

#[tokio::test]
async fn shows_the_whole_thread_to_a_session_started_over_from_a_lost_one() {
    let root = message("1", "Build is red", |_| {});
    let mention = message("2", "@Claude again?", |event| {
        event.created_at = 300;
        event.tags = tags(&[&["h", CHANNEL], &["e", &root.id, "", "reply"]]);
    });
    let setup = Setup::new(vec![root.clone(), mention.clone()]);
    setup.store.set(
        &format!("{CHANNEL}/{}", root.id),
        SavedSession {
            id: "lost".into(),
            seen: 200,
            at: 1,
        },
    );
    setup.mention(&mention).await;
    flush().await;
    flush().await;
    let lost = setup.fake.claudes()[0].clone();
    assert!(lost.args().contains(&"lost".into()));
    assert!(lost.prompts().is_empty());
    let fresh = setup.used()[0].clone();
    assert!(fresh.prompts()[0].contains("<thread-context included=\"1\" total=\"2\""));
}

#[tokio::test]
async fn acts_only_on_its_owners_messages_unless_told_to_answer_anyone() {
    let stranger = message("5", "@Claude run this for me", |event| {
        event.pubkey = "e".repeat(64);
        event.tags = tags(&[&["h", CHANNEL], &["p", &own()]]);
    });
    let setup = Setup::new(vec![]);
    setup.mention(&stranger).await;
    flush().await;
    assert!(setup.published().is_empty());
    assert!(setup.used().is_empty());
    setup
        .deliver(
            Wake::Mention { event: stranger },
            Config {
                respond_to: "anyone".into(),
                ..Config::default()
            }
            .value(),
        )
        .await;
    flush().await;
    assert!(!setup.used().is_empty());
}

#[tokio::test]
async fn runs_a_timer_in_its_own_session() {
    let setup = Setup::new(vec![]);
    setup
        .deliver(
            Wake::Timer {
                slug: "watch/daily".into(),
                prompt: "Post a summary".into(),
                interest: None,
            },
            Config::default().value(),
        )
        .await;
    flush().await;
    assert!(setup.used()[0].prompts()[0].contains("Timer: watch/daily"));
}

#[tokio::test]
async fn keeps_one_spare_for_all_its_agents_and_none_once_they_are_gone() {
    let setup = Setup::new(vec![]);
    let agent = |pubkey: String| Synced {
        pubkey,
        config: Config::default().value(),
        spawn: setup.fake.spawner(),
    };
    setup.runtime.sync(vec![agent(own()), agent(alice())]);
    flush().await;
    assert_eq!(setup.fake.claudes().len(), 1);
    assert_eq!(
        setup.fake.claudes()[0].state().options.cwd.as_deref(),
        Some("~/.buzz")
    );
    setup.runtime.sync(vec![]);
    flush().await;
    assert!(setup.fake.claudes().iter().all(FakeClaude::killed));
}

#[tokio::test]
async fn replaces_the_spare_when_no_agent_runs_where_it_was_started() {
    let setup = Setup::new(vec![]);
    setup.sync(Config::default());
    flush().await;
    setup.sync(Config {
        model: "sonnet".into(),
        ..Config::default()
    });
    flush().await;
    let claudes = setup.fake.claudes();
    assert_eq!(claudes.len(), 2);
    assert!(claudes[0].killed());
    let args = claudes[1].args();
    assert!(
        args.windows(2).any(|pair| pair == ["--model", "sonnet"]),
        "{args:?}"
    );
}

#[test]
fn reads_config_over_the_defaults() {
    assert_eq!(Config::read(&Value::Null), Config::default());
    let config = Config::read(&json!({ "model": "opus", "respondTo": "anyone", "scope": 3 }));
    assert_eq!(config.model, "opus");
    assert_eq!(config.respond_to, "anyone");
    assert_eq!(config.scope, "thread");
    assert_eq!(config.workspace, "~/.buzz");
    assert_eq!(
        Config::default().value(),
        json!({ "model": "", "instructions": "", "workspace": "~/.buzz",
                "scope": "thread", "respondTo": "owner" })
    );
}

#[test]
fn thread_references_follow_nip10() {
    let root = "1".repeat(64);
    let parent = "2".repeat(64);
    let event = |tags_: Vec<Vec<String>>| BuzzEvent {
        tags: tags_,
        ..Default::default()
    };
    assert_eq!(
        thread_reference(&event(tags(&[&["e", &root, "", "root"]]))),
        None
    );
    assert_eq!(
        thread_reference(&event(tags(&[&["e", &root, "", "reply"]]))),
        Some(Thread {
            root_id: root.clone(),
            parent_id: root.clone()
        })
    );
    assert_eq!(
        thread_reference(&event(tags(&[
            &["e", &root, "", "root"],
            &["e", &parent, "", "reply"]
        ]))),
        Some(Thread {
            root_id: root,
            parent_id: parent
        })
    );
}

fn levels(ids: &[&str]) -> Vec<Effort> {
    efforts(None)
        .into_iter()
        .filter(|level| ids.contains(&level.id.as_str()))
        .collect()
}

#[tokio::test]
async fn knows_the_agents_own_model_once_one_of_its_sessions_has_begun() {
    let setup = Setup::new(vec![]);
    let mention = message("2", "@Claude can you look?", |event| {
        event.tags = tags(&[&["h", CHANNEL], &["p", &own()]]);
    });
    setup.mention(&mention).await;
    flush().await;
    assert_eq!(setup.runtime.models(&own()).await.unwrap().current, "opus");
    setup.runtime.dispose();
}

#[tokio::test]
async fn discovers_models_and_effort_without_a_turn_or_keeping_a_process_for_it() {
    let setup = Setup::new(vec![]);
    setup.sync(Config::default());
    let all = levels(&["low", "medium", "high", "xhigh"]);
    assert_eq!(
        setup.runtime.models(&own()).await.unwrap(),
        Models {
            current: "opus".into(),
            models: vec![
                Model {
                    id: "opus".into(),
                    label: "Opus 5.5".into(),
                    efforts: all,
                    effort: Some("high".into()),
                },
                Model {
                    id: "haiku".into(),
                    label: "Haiku 5.5".into(),
                    efforts: levels(&["low", "high"]),
                    effort: Some("high".into()),
                },
                Model {
                    id: "legacy".into(),
                    label: "Legacy 1".into(),
                    efforts: vec![],
                    effort: None,
                },
            ],
        }
    );
    // Only the shared spare stays; the lookup's process is gone.
    flush().await;
    assert_eq!(setup.fake.live().len(), 1);
    assert!(
        setup
            .fake
            .claudes()
            .iter()
            .all(|process| process.prompts().is_empty())
    );
    setup.runtime.dispose();
}

#[tokio::test]
async fn still_offers_a_configured_model_claude_code_does_not_list() {
    let setup = Setup::new(vec![]);
    setup.sync(Config {
        model: "claude-sonnet-9-9".into(),
        ..Config::default()
    });
    let Models { current, models } = setup.runtime.models(&own()).await.unwrap();
    assert_eq!(current, "claude-sonnet-9-9");
    let labels: Vec<String> = models.into_iter().map(|model| model.label).collect();
    assert_eq!(labels, ["Sonnet 9.9", "Opus 5.5", "Haiku 5.5", "Legacy 1"]);
    setup.runtime.dispose();
}

#[tokio::test]
async fn runs_a_mention_on_the_model_and_effort_its_delivery_picked() {
    let setup = Setup::new(vec![]);
    setup.sync(Config::default());
    assert_eq!(setup.runtime.models(&own()).await.unwrap().current, "opus");
    let mention = message("2", "@Claude think hard", |event| {
        event.tags = tags(&[&["h", CHANNEL], &["p", &own()]]);
    });
    setup
        .runtime
        .run(Run {
            wake: Wake::Mention { event: mention },
            channel: Some(CHANNEL.into()),
            agent: setup.agent.clone(),
            config: Config::default().value(),
            spawn: setup.fake.spawner(),
            model: Some("haiku".into()),
            effort: Some("low".into()),
            working: std::sync::Arc::new(|_| {}),
        })
        .await;
    flush().await;
    let used = setup.used();
    assert_eq!(used[0].models(), ["haiku"]);
    assert_eq!(used[0].efforts(), ["low"]);
    assert_eq!(setup.runtime.models(&own()).await.unwrap().current, "opus");
    setup.runtime.dispose();
}

#[test]
fn reads_model_ids_and_aliases_as_names() {
    assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
    assert_eq!(model_name("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
    assert_eq!(model_name("claude-opus-4[1m]"), "Opus 4");
    assert_eq!(model_name("opus"), "Opus");
    assert_eq!(model_name("my-proxy/model"), "my-proxy/model");
}
