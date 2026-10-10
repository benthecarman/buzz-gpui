//! End to end through a real agent host: a fake `claude` on the host's
//! `PATH` speaks stream-json, the host runs it through the process bridge,
//! and the runtime serves it the Buzz tools as the agent. No network and no
//! Claude account.

use super::runtime::{ChannelInfo, ClaudeRuntime, Config, OwnerRelay, Run, Synced, Wake};
use super::sessions::SessionView;
use super::setup::{ClaudeSetup, ClaudeStatus};
use crate::agent_plugins::claude_code::client::AgentActions;
use crate::agent_plugins::process::{HostSpawner, ProcessSinks, Spawner};
use crate::buzz_mcp::{BuzzEvent, ClientResult, Memory, Template, Upload};
use buzz_host::api::{HostAnswer, HostAsk, HostProtocol};
use buzz_host::credentials::FileCredentials;
use buzz_host::{AgentHost, HostConfig, RpcHandler};
use buzz_host_rpc::{
    ClientHandler, ConnectionState, HostClient, HostClientOptions, HostInfo, RpcError,
    ServerOptions, serve, transport,
};
use futures::future::BoxFuture;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const FAKE_CLAUDE: &str = include_str!("e2e/fake-claude.sh");
const CHANNEL: &str = "chan-e2e";

/// A directory of its own under the system temp directory.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("buzz-claude-e2e-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Puts the fake `claude` first on this process's `PATH`, once.
fn fake_on_path() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = scratch("bin");
        let claude = dir.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.clone()];
        paths.extend(std::env::split_paths(&path));
        // SAFETY: set once, before this test's host spawns anything.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
        dir
    })
}

/// The GUI's side of the connection: process output only.
struct Gui(ProcessSinks);

impl ClientHandler<HostProtocol> for Gui {
    fn handle(&self, ask: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        let answer = match ask {
            HostAsk::ProcessOutput { handle, output } => self.0.deliver(handle, output),
            _ => Err(RpcError::Failed("unexpected ask".into())),
        };
        Box::pin(async move { answer })
    }
}

#[derive(Default)]
struct Agent {
    published: Mutex<Vec<Template>>,
}

impl AgentActions for Agent {
    fn pubkey(&self) -> String {
        "c".repeat(64)
    }
    fn owner(&self) -> String {
        "a".repeat(64)
    }
    fn publish(&self, template: Template) -> BoxFuture<'static, ClientResult<BuzzEvent>> {
        self.published.lock().unwrap().push(template.clone());
        let event = BuzzEvent {
            id: "e".repeat(64),
            pubkey: self.pubkey(),
            kind: template.kind,
            content: template.content,
            tags: template.tags,
            ..Default::default()
        };
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

struct Relay;

impl OwnerRelay for Relay {
    fn channel(&self, _: &str) -> Option<ChannelInfo> {
        Some(ChannelInfo {
            name: Some("general".into()),
            ..Default::default()
        })
    }
    fn read(&self, _: Vec<Value>) -> BoxFuture<'static, Result<Vec<BuzzEvent>, String>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn names(&self, _: Vec<String>, _: Vec<String>) -> BoxFuture<'static, HashMap<String, String>> {
        Box::pin(async { HashMap::new() })
    }
    fn memories(&self, _: &str) -> BoxFuture<'static, Result<Vec<Memory>, String>> {
        Box::pin(async {
            Ok(vec![Memory {
                slug: "core".into(),
                body: "I am the e2e agent.".into(),
                created_at: 1,
            }])
        })
    }
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mention_runs_a_claude_turn_on_the_agents_host() {
    fake_on_path();
    let state = scratch("host");
    let host = AgentHost::start(HostConfig {
        info: HostInfo {
            id: "e2e".into(),
            name: "e2e".into(),
            platform: "linux".into(),
            version: "test".into(),
        },
        state_dir: state.join("state"),
        workspace: state.join("workspace"),
        credentials: Arc::new(FileCredentials::new(state.join("keys")).unwrap()),
        watch_interval: Duration::from_millis(200),
        legacy_dir: None,
    })
    .unwrap();
    let (connector, listener) = transport::memory();
    tokio::spawn(serve(
        listener,
        Arc::new(RpcHandler(host)),
        ServerOptions::default(),
    ));
    let sinks = ProcessSinks::default();
    let client = Arc::new(HostClient::<HostProtocol>::spawn(
        &tokio::runtime::Handle::current(),
        Arc::new(connector),
        Arc::new(Gui(sinks.clone())),
        HostClientOptions::default(),
    ));
    sinks.follow(&client, &tokio::runtime::Handle::current());
    client
        .state()
        .wait_for(|state| matches!(state, ConnectionState::Connected { .. }))
        .await
        .unwrap();
    let spawn: Spawner = Arc::new(HostSpawner {
        client,
        sinks,
        owner: super::PROCESS_OWNER,
    });

    // Setup reads the version and account through the host.
    let setup = ClaudeSetup::new(Some(spawn.clone()));
    setup.check().await;
    assert_eq!(
        setup.snapshot().status,
        Some(ClaudeStatus::Ready {
            version: "2.1.3".into(),
            account: Some("e2e@example.com".into()),
        })
    );

    // A mention becomes a turn; Claude answers with a final `send`.
    let sessions = scratch("sessions").join("sessions.json");
    let runtime = ClaudeRuntime::new(Some(sessions.clone()));
    runtime.set_relay(Some(Arc::new(Relay)));
    let agent = Arc::new(Agent::default());
    let workspace = scratch("workspace");
    let config = Config {
        workspace: workspace.to_string_lossy().into_owned(),
        ..Config::default()
    };
    runtime.sync(vec![Synced {
        pubkey: agent.pubkey(),
        config: config.value(),
        spawn: spawn.clone(),
    }]);
    let mention = BuzzEvent {
        id: "1".repeat(64),
        pubkey: agent.owner(),
        created_at: 100,
        kind: 9,
        content: "@Claude ping".into(),
        tags: vec![
            vec!["h".into(), CHANNEL.into()],
            vec!["p".into(), agent.pubkey()],
        ],
        sig: String::new(),
    };
    runtime
        .run(Run {
            wake: Wake::Mention {
                event: mention.clone(),
            },
            channel: Some(CHANNEL.into()),
            agent: agent.clone(),
            config: config.value(),
            spawn: spawn.clone(),
        })
        .await;
    wait_for("the reply", || !agent.published.lock().unwrap().is_empty()).await;
    let key = format!("{CHANNEL}/{}", mention.id);
    assert_eq!(
        agent.published.lock().unwrap()[0],
        Template {
            kind: 9,
            content: "pong".into(),
            tags: vec![
                vec!["h".into(), CHANNEL.into()],
                vec![
                    "e".into(),
                    mention.id.clone(),
                    String::new(),
                    "reply".into()
                ],
            ],
        }
    );
    wait_for("the turn to settle", || {
        runtime.sessions(&agent.pubkey())
            == vec![SessionView {
                key: key.clone(),
                busy: false,
            }]
    })
    .await;
    // The session is saved under the conversation, to resume later.
    let saved: Value = serde_json::from_slice(&std::fs::read(&sessions).unwrap()).unwrap();
    assert!(saved[agent.pubkey()][&key]["id"].is_string(), "{saved}");

    // A file is read on the host from the agent's workspace.
    std::fs::write(workspace.join("report.md"), "from a file").unwrap();
    let follow_up = BuzzEvent {
        id: "2".repeat(64),
        created_at: 120,
        content: "READ:report.md please".into(),
        tags: vec![
            vec!["h".into(), CHANNEL.into()],
            vec![
                "e".into(),
                mention.id.clone(),
                String::new(),
                "reply".into(),
            ],
        ],
        ..mention.clone()
    };
    runtime
        .run(Run {
            wake: Wake::Mention { event: follow_up },
            channel: Some(CHANNEL.into()),
            agent: agent.clone(),
            config: config.value(),
            spawn,
        })
        .await;
    wait_for("the file", || agent.published.lock().unwrap().len() == 2).await;
    assert_eq!(agent.published.lock().unwrap()[1].content, "from a file");
    runtime.dispose();
}
