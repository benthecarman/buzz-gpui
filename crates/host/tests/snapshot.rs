//! Snapshot memory restore over host-rpc, and what a GUI sees from a host
//! that predates it.

use buzz_host::api::*;
use buzz_host::credentials::FileCredentials;
use buzz_host::{AgentHost, HostConfig, RpcHandler};
use buzz_host_rpc::{
    ClientHandler, ConnectionState, EventSource, HostClient, HostClientOptions, HostHandler,
    HostInfo, Peer, Protocol, RpcError, ServerOptions, serve, transport,
};
use futures::future::BoxFuture;
use secp256k1::{Keypair, Secp256k1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

struct NoAsks;

impl ClientHandler<HostProtocol> for NoAsks {
    fn handle(&self, _: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        Box::pin(async { Err(RpcError::Failed("unexpected ask".into())) })
    }
}

fn info() -> HostInfo {
    HostInfo {
        id: "laptop".into(),
        name: "This laptop".into(),
        platform: "linux".into(),
        version: "test".into(),
    }
}

async fn connect(connector: transport::MemoryConnector) -> HostClient<HostProtocol> {
    let client = HostClient::<HostProtocol>::spawn(
        &tokio::runtime::Handle::current(),
        Arc::new(connector),
        Arc::new(NoAsks),
        HostClientOptions::default(),
    );
    let mut state = client.state();
    tokio::time::timeout(
        Duration::from_secs(5),
        state.wait_for(|s| matches!(s, ConnectionState::Connected { .. })),
    )
    .await
    .unwrap()
    .unwrap();
    client
}

fn restore(id: &str, owner: &str, entries: Vec<MemoryRestore>) -> Request {
    Request::RestoreMemory {
        id: id.into(),
        owner: owner.into(),
        entries,
    }
}

fn entry(slug: &str, body: &str) -> MemoryRestore {
    MemoryRestore {
        slug: slug.into(),
        body: body.into(),
        after: None,
    }
}

fn attest(owner: &Keypair, agent: &str) -> String {
    let digest: [u8; 32] = Sha256::digest(format!("nostr:agent-auth:{agent}:")).into();
    let signature = Secp256k1::new().sign_schnorr_no_aux_rand(&digest, owner);
    serde_json::to_string(&[
        "auth",
        &owner.x_only_public_key().0.to_string(),
        "",
        &signature.to_string(),
    ])
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_checks_custody_before_writing() {
    let state = tempfile::tempdir().unwrap();
    let host = AgentHost::start(HostConfig {
        info: info(),
        state_dir: state.path().join("state"),
        workspace: state.path().join("workspace"),
        credentials: Arc::new(FileCredentials::new(state.path().join("keys")).unwrap()),
        watch_interval: Duration::from_millis(100),
        legacy_dir: None,
    })
    .unwrap();
    let (connector, listener) = transport::memory();
    tokio::spawn(serve(
        listener,
        Arc::new(RpcHandler(host)),
        ServerOptions::default(),
    ));
    let client = connect(connector).await;

    // The capability probe.
    let probe = client.request(restore("", "", Vec::new())).await.unwrap();
    assert!(matches!(
        probe,
        Response::MemoryRestored { published, errors } if published.is_empty() && errors.is_empty()
    ));

    let human = Keypair::new(&Secp256k1::new(), &mut secp256k1::rand::rng());
    let owner = human.x_only_public_key().0.to_string();
    let Response::Prepared { id, pubkey, .. } = client
        .request(Request::CreatePrepare {
            request_id: "snap".into(),
            // Nothing listens here: publication fails fast.
            relay_url: "wss://127.0.0.1:9".into(),
            owner: owner.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected prepared");
    };
    client
        .request(Request::CreateCommit {
            request_id: "snap".into(),
            edit: AgentEdit {
                name: "Scout".into(),
                harness: HarnessEdit {
                    command: "buzz-agent".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            auth: attest(&human, &pubkey),
        })
        .await
        .unwrap();

    let failed = |result: Result<Response, RpcError>| match result {
        Err(RpcError::Failed(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let entries = vec![
        entry("core", "I am Scout."),
        entry("mem/notes", "Likes tea."),
    ];
    assert_eq!(
        failed(
            client
                .request(restore("missing", &owner, entries.clone()))
                .await
        ),
        "Agent no longer exists"
    );
    assert_eq!(
        failed(
            client
                .request(restore(&id, &"ab".repeat(32), entries.clone()))
                .await
        ),
        "Snapshot memory belongs to another owner"
    );
    assert_eq!(
        failed(
            client
                .request(restore(&id, &owner, vec![entry("bad slug", "x")]))
                .await
        ),
        "Invalid snapshot memory slug"
    );
    // Custody holds; each entry fails on its own when the relay is away.
    let Response::MemoryRestored { published, errors } =
        client.request(restore(&id, &owner, entries)).await.unwrap()
    else {
        panic!("expected a restore result");
    };
    assert!(published.is_empty());
    assert_eq!(
        errors,
        [
            "core: memory restore failed; retry this agent",
            "mem/notes: memory restore failed; retry this agent"
        ]
    );
}

/// A host from before snapshots: the same protocol without
/// `RestoreMemory` in its request enum.
struct OldProtocol;

#[derive(Clone, Debug, Serialize, Deserialize)]
enum OldRequest {
    Agents,
}

impl Protocol for OldProtocol {
    type ClientRequest = OldRequest;
    type ClientResponse = Response;
    type HostRequest = HostAsk;
    type HostResponse = HostAnswer;
    type Event = HostEvent;
}

struct OldHost(tokio::sync::broadcast::Sender<(u64, HostEvent)>);

impl HostHandler<OldProtocol> for OldHost {
    fn info(&self) -> HostInfo {
        info()
    }

    fn events(&self) -> EventSource<OldProtocol> {
        self.0.subscribe()
    }

    fn handle(
        &self,
        _: Peer<OldProtocol>,
        _: OldRequest,
    ) -> BoxFuture<'static, Result<Response, RpcError>> {
        Box::pin(async { Ok(Response::Agents(AgentsSnapshot::default())) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn old_hosts_answer_restore_as_unsupported() {
    let (connector, listener) = transport::memory();
    tokio::spawn(serve::<OldProtocol, _>(
        listener,
        Arc::new(OldHost(tokio::sync::broadcast::channel(4).0)),
        ServerOptions::default(),
    ));
    let client = connect(connector).await;
    let refused = client
        .request(restore("", "", Vec::new()))
        .await
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "The other side does not support this request; update it."
    );
    // The connection survives, and known requests still work.
    assert!(matches!(
        client.request(Request::Agents).await,
        Ok(Response::Agents(_))
    ));
    // Old agents views lack the snapshot fields; they decode as absent.
    let old: Agent =
        serde_json::from_value(serde_json::json!({ "id": "a", "name": "Old" })).unwrap();
    assert_eq!(old.snapshot_export_limitations, None);
}
