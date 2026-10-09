//! The agent lifecycle end to end, against the TLS test relay with the real
//! buzz-acp: create with the owner's attestation, publish the profile, start,
//! answer a mention, stop and delete. The harness is a fake ACP agent
//! (`tests/fixtures/fake_acp_agent.py`) that replies `echo: <mention>`.
//!
//! Skipped unless `BUZZ_TEST_RELAY_TLS` names the test relay's state
//! directory (`.test-relay`) after `scripts/test-relay.sh up-tls`.
//!
//! There is no libtest harness: the controller re-executes this binary as the
//! agent supervisor, so `main` dispatches that first, and sets the process
//! environment before any thread starts.

use buzz_host::api::*;
use buzz_host::credentials::FileCredentials;
use buzz_host::{AgentHost, HostConfig};
use buzz_host_rpc::HostInfo;
use buzz_identity::{Identity, Purpose};
use buzz_relay_client::{RelayHttp, RelayOrigin};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture {
    relay_url: String,
    general: String,
    buzz_cli: String,
    alice: Arc<Identity>,
}

fn main() {
    buzz_host::dispatch_supervisor();
    let Some(dir) = std::env::var_os("BUZZ_TEST_RELAY_TLS").map(PathBuf::from) else {
        eprintln!("BUZZ_TEST_RELAY_TLS not set; skipping agent_e2e");
        return;
    };
    let read = |path: PathBuf| -> Value {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{}: {error}; run scripts/test-relay.sh up-tls",
                path.display()
            )
        });
        serde_json::from_str(&text).unwrap()
    };
    let identities = read(dir.join("identities.json"));
    let tls = read(dir.join("tls/relay.json"));
    let text = |value: &Value| value.as_str().unwrap().to_string();

    let scratch = tempfile::tempdir().unwrap();
    // Agents run buzz-acp from the tool PATH, which starts at
    // $HOME/.local/bin. A private HOME puts the buzz-acp built by up-tls
    // ahead of any installed one, and isolates the agent's home.
    let home = scratch.path().join("home");
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(text(&tls["buzz_acp"]), home.join(".local/bin/buzz-acp")).unwrap();
    // SAFETY: no other thread exists yet.
    unsafe {
        std::env::set_var("HOME", &home);
        // Trusted by our HTTP clients and, passed through by the
        // controller, by buzz-acp and the buzz CLI the fake agent runs.
        std::env::set_var("SSL_CERT_FILE", text(&tls["ca_bundle"]));
    }

    let fixture = Fixture {
        relay_url: text(&tls["relay_ws_url"]),
        general: text(&tls["channels"]["general"]),
        buzz_cli: text(&tls["buzz_cli"]),
        alice: Arc::new(
            Identity::from_nsec(identities["identities"]["alice"]["nsec"].as_str().unwrap())
                .unwrap(),
        ),
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(lifecycle(fixture, scratch.path()));
    println!("agent_e2e: agent lifecycle over TLS ... ok");
}

async fn lifecycle(f: Fixture, scratch: &Path) {
    let state = scratch.join("host");
    let host = AgentHost::start(HostConfig {
        info: HostInfo {
            id: "e2e".into(),
            name: "Agent e2e".into(),
            platform: std::env::consts::OS.into(),
            version: "test".into(),
        },
        state_dir: state.join("state"),
        workspace: state.join("workspace"),
        credentials: Arc::new(FileCredentials::new(state.join("keys")).unwrap()),
        watch_interval: Duration::from_millis(200),
    })
    .unwrap();
    let log = |id: &str| state.join(format!("state/agent-controller/logs/{id}.log"));

    // Create: the host makes the key, alice attests it.
    let Response::Prepared { id, pubkey } = host
        .request(Request::CreatePrepare {
            request_id: "e2e".into(),
            relay_url: f.relay_url.clone(),
            owner: f.alice.pubkey_hex(),
        })
        .await
        .unwrap()
    else {
        panic!("expected prepared");
    };
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_acp_agent.py");
    host.request(Request::CreateCommit {
        request_id: "e2e".into(),
        edit: AgentEdit {
            name: "E2E Echo".into(),
            system_prompt: "Echo every mention.".into(),
            harness: HarnessEdit {
                command: fake.to_string_lossy().into_owned(),
                ..Default::default()
            },
            environment: [("FAKE_ACP_BUZZ_CLI".into(), Some(f.buzz_cli.clone()))].into(),
            ..Default::default()
        },
        auth: f.alice.attest_agent(&pubkey).unwrap(),
    })
    .await
    .unwrap();

    // The host publishes the agent's profile over HTTPS after the create.
    agent_until(&host, &id, &log(&id), "the profile is published", |a| {
        !a.profile_pending
    })
    .await;

    // alice, the channel's creator, adds the agent to #general.
    let http = RelayHttp::new(RelayOrigin::parse(&f.relay_url).unwrap(), f.alice.clone());
    publish(
        &f.alice,
        &http,
        9000,
        vec![
            vec!["h".into(), f.general.clone()],
            vec!["p".into(), pubkey.clone()],
            vec!["role".into(), "member".into()],
        ],
        String::new(),
    )
    .await;

    host.request(Request::Action {
        id: id.clone(),
        action: AgentAction::Start,
    })
    .await
    .unwrap();
    agent_until(&host, &id, &log(&id), "the agent runs", |a| {
        a.status == ProcessStatus::Running
    })
    .await;

    // Mention the agent until it answers: a mention sent before buzz-acp
    // has subscribed may never reach it.
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut sent = Vec::new();
    let reply = 'reply: loop {
        assert!(
            Instant::now() < deadline,
            "no echo for {sent:?}\n{}",
            read_log(&log(&id))
        );
        // The p tag is the mention. No "@name" text: the CLI would resolve
        // it again in the echo, ambiguously after earlier runs.
        let content = format!("ping {nonce}-{}", sent.len());
        publish(
            &f.alice,
            &http,
            9,
            vec![
                vec!["h".into(), f.general.clone()],
                vec!["p".into(), pubkey.clone()],
            ],
            content.clone(),
        )
        .await;
        sent.push(content);
        let wait = Instant::now() + Duration::from_secs(20);
        while Instant::now() < wait.min(deadline) {
            let mut filter = buzz_relay_client::Filter::new();
            filter.insert("kinds".into(), json!([9]));
            filter.insert("authors".into(), json!([pubkey]));
            filter.insert("#h".into(), json!([f.general]));
            filter.insert("limit".into(), json!(20));
            let replies = http.query(&[filter]).await.unwrap();
            if let Some(event) = replies
                .into_iter()
                .find(|e| sent.iter().any(|s| e.content == format!("echo: {s}")))
            {
                break 'reply event;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    assert_eq!(reply.pubkey.to_hex(), pubkey);
    assert!(reply.content.starts_with(&format!("echo: ping {nonce}-")));

    host.request(Request::Action {
        id: id.clone(),
        action: AgentAction::Stop,
    })
    .await
    .unwrap();
    let stopped = agent_until(&host, &id, &log(&id), "the agent stops", |a| {
        a.status == ProcessStatus::Stopped
    })
    .await;

    let Response::Agents(after) = host
        .request(Request::Delete {
            id: id.clone(),
            revision: stopped.revision,
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    assert!(after.agents.is_empty());
    assert!(
        !state.join("keys").join(format!("{id}.key")).exists(),
        "deleting the agent deletes its key"
    );

    // Leave #general as it was for the next run.
    publish(
        &f.alice,
        &http,
        9001,
        vec![
            vec!["h".into(), f.general.clone()],
            vec!["p".into(), pubkey.clone()],
        ],
        String::new(),
    )
    .await;
}

/// Polls the host until the agent satisfies `done`.
async fn agent_until(
    host: &AgentHost,
    id: &str,
    log: &Path,
    what: &str,
    done: impl Fn(&Agent) -> bool,
) -> Agent {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let Response::Agents(snapshot) = host.request(Request::Agents).await.unwrap() else {
            panic!("expected agents");
        };
        let agent = snapshot
            .agents
            .into_iter()
            .find(|a| a.id == id)
            .expect("agent exists");
        if done(&agent) {
            return agent;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting until {what}: {agent:#?}\n{}",
            read_log(log)
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Signs as `who` and posts to `/events`; the relay must accept it.
async fn publish(
    who: &Identity,
    http: &RelayHttp,
    kind: u16,
    tags: Vec<Vec<String>>,
    content: String,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let unsigned = who
        .template(Purpose::Content, kind, now, tags, content)
        .unwrap();
    let event = who.sign(Purpose::Content, unsigned).unwrap();
    let receipt = http
        .post("/events", serde_json::to_string(&event).unwrap())
        .await
        .unwrap();
    let receipt: Value = serde_json::from_slice(&receipt).unwrap();
    assert_eq!(receipt["accepted"], true, "kind {kind} refused: {receipt}");
}

fn read_log(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!(
            "--- agent log {} ---\n{}",
            path.display(),
            String::from_utf8_lossy(&bytes)
        ),
        Err(error) => format!("(no agent log at {}: {error})", path.display()),
    }
}
