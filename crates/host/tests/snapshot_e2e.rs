//! Snapshot memory restore end to end against the TLS test relay: the host
//! publishes owner-addressed memories with a new agent's key, and the owner
//! reads them back with the human key, as the import dialog does.
//!
//! Skipped unless `BUZZ_TEST_RELAY_TLS` (or `BUZZ_TEST_RELAY`) names the
//! test relay's state directory after `scripts/test-relay.sh up-tls`. Like
//! `agent_e2e`, it sets the process environment before any thread starts,
//! so it has no libtest harness.

use buzz_host::api::*;
use buzz_host::credentials::FileCredentials;
use buzz_host::{AgentHost, HostConfig};
use buzz_host_rpc::HostInfo;
use buzz_identity::Identity;
use buzz_relay_client::{RelayHttp, RelayOrigin};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    buzz_host::dispatch_supervisor();
    let Some(dir) = std::env::var_os("BUZZ_TEST_RELAY_TLS")
        .or_else(|| std::env::var_os("BUZZ_TEST_RELAY"))
        .map(PathBuf::from)
    else {
        eprintln!("BUZZ_TEST_RELAY_TLS not set; skipping snapshot_e2e");
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
    // SAFETY: no other thread exists yet.
    unsafe {
        std::env::set_var("SSL_CERT_FILE", tls["ca_bundle"].as_str().unwrap());
    }
    let alice = Arc::new(
        Identity::from_nsec(identities["identities"]["alice"]["nsec"].as_str().unwrap()).unwrap(),
    );
    let relay = tls["relay_ws_url"].as_str().unwrap().to_string();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(restore(alice, relay));
    println!("snapshot_e2e: memory restore over TLS ... ok");
}

async fn memories(alice: &Arc<Identity>, relay: &str, agent: &str) -> Value {
    let http = RelayHttp::new(RelayOrigin::parse(relay).unwrap(), alice.clone());
    let query = buzz_desktop::relay::agent::memory_query(&alice.pubkey_hex(), agent).unwrap();
    let body = http.post("/query", query).await.unwrap();
    let records =
        buzz_desktop::relay::agent::memory_wire_records(std::str::from_utf8(&body).unwrap())
            .unwrap();
    alice.decode_agent_memories(agent, &records).unwrap()
}

fn entry<'a>(listing: &'a Value, slug: &str) -> Option<&'a Value> {
    listing["entries"]
        .as_array()?
        .iter()
        .find(|e| e["slug"] == slug)
}

async fn restore(alice: Arc<Identity>, relay: String) {
    let scratch = tempfile::tempdir().unwrap();
    let host = AgentHost::start(HostConfig {
        info: HostInfo {
            id: "snapshot-e2e".into(),
            name: "Snapshot e2e".into(),
            platform: std::env::consts::OS.into(),
            version: "test".into(),
        },
        state_dir: scratch.path().join("state"),
        workspace: scratch.path().join("workspace"),
        credentials: Arc::new(FileCredentials::new(scratch.path().join("keys")).unwrap()),
        watch_interval: Duration::from_millis(200),
        legacy_dir: None,
    })
    .unwrap();
    let Response::Prepared { id, pubkey, .. } = host
        .request(Request::CreatePrepare {
            request_id: "snapshot-e2e".into(),
            relay_url: relay.clone(),
            owner: alice.pubkey_hex(),
        })
        .await
        .unwrap()
    else {
        panic!("expected prepared");
    };
    host.request(Request::CreateCommit {
        request_id: "snapshot-e2e".into(),
        edit: AgentEdit {
            name: "Snapshot Scout".into(),
            harness: HarnessEdit {
                command: "buzz-agent".into(),
                ..Default::default()
            },
            ..Default::default()
        },
        auth: alice.attest_agent(&pubkey).unwrap(),
    })
    .await
    .unwrap();

    let restore = |entries: Vec<MemoryRestore>| Request::RestoreMemory {
        id: id.clone(),
        owner: alice.pubkey_hex(),
        entries,
    };
    let Response::MemoryRestored { published, errors } = host
        .request(restore(vec![
            MemoryRestore {
                slug: "core".into(),
                body: "I am Scout.".into(),
                after: None,
            },
            MemoryRestore {
                slug: "mem/notes".into(),
                body: "Likes tea.".into(),
                after: None,
            },
        ]))
        .await
        .unwrap()
    else {
        panic!("expected a restore result");
    };
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(published.len(), 2);

    // The owner reads exactly those events back as current.
    let listing = memories(&alice, &relay, &pubkey).await;
    assert_eq!(listing["partial"], false);
    for restored in &published {
        let current = entry(&listing, &restored.slug).unwrap();
        assert_eq!(current["eventId"], restored.event_id.as_str());
    }
    assert_eq!(entry(&listing, "core").unwrap()["body"], "I am Scout.");

    // A retry with a changed body supersedes the earlier entry.
    let created = entry(&listing, "core").unwrap()["createdAt"]
        .as_u64()
        .unwrap();
    let Response::MemoryRestored { published, errors } = host
        .request(restore(vec![MemoryRestore {
            slug: "core".into(),
            body: "I am Scout, restored.".into(),
            after: Some(created),
        }]))
        .await
        .unwrap()
    else {
        panic!("expected a restore result");
    };
    assert!(errors.is_empty(), "{errors:?}");
    let listing = memories(&alice, &relay, &pubkey).await;
    let core = entry(&listing, "core").unwrap();
    assert_eq!(core["eventId"], published[0].event_id.as_str());
    assert_eq!(core["body"], "I am Scout, restored.");
    assert!(core["createdAt"].as_u64().unwrap() > created);

    host.request(Request::Delete { id, revision: 1 }).await.ok();
}
