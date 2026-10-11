use buzz_host::api::*;
use buzz_host::credentials::FileCredentials;
use buzz_host::{AgentHost, HostConfig, RpcHandler};
use buzz_host_rpc::{
    ClientEvent, ClientHandler, HostClient, HostClientOptions, HostInfo, RpcError, ServerOptions,
    serve, transport,
};
use futures::future::BoxFuture;
use secp256k1::{Keypair, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

struct NoAsks;

impl ClientHandler<HostProtocol> for NoAsks {
    fn handle(&self, _: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        Box::pin(async { Err(RpcError::Failed("unexpected ask".into())) })
    }
}

/// What the GUI does with the human key when the host prepares an agent.
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

fn edit(name: &str) -> AgentEdit {
    AgentEdit {
        name: name.into(),
        system_prompt: "Be helpful.".into(),
        harness: HarnessEdit {
            command: "goose".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn pushed_agents(
    events: &mut tokio::sync::broadcast::Receiver<ClientEvent<HostEvent>>,
    predicate: impl Fn(&AgentsSnapshot) -> bool,
) -> AgentsSnapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ClientEvent::Event {
                body: HostEvent::Agents(snapshot),
                ..
            } = events.recv().await.unwrap()
                && predicate(&snapshot)
            {
                return snapshot;
            }
        }
    })
    .await
    .expect("no agents event")
}

#[tokio::test(flavor = "multi_thread")]
async fn create_save_and_delete_over_rpc() {
    let state = tempfile::tempdir().unwrap();
    let host = AgentHost::start(HostConfig {
        info: HostInfo {
            id: "laptop".into(),
            name: "This laptop".into(),
            platform: "linux".into(),
            version: "test".into(),
        },
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
    let client = HostClient::<HostProtocol>::spawn(
        &tokio::runtime::Handle::current(),
        Arc::new(connector),
        Arc::new(NoAsks),
        HostClientOptions::default(),
    );
    let mut events = client.events();
    let mut state_watch = client.state();
    state_watch
        .wait_for(|s| matches!(s, buzz_host_rpc::ConnectionState::Connected { .. }))
        .await
        .unwrap();

    let Response::Agents(initial) = client.request(Request::Agents).await.unwrap() else {
        panic!("expected agents");
    };
    assert!(initial.agents.is_empty());
    assert!(initial.runtime_available);

    let human = Keypair::new(&Secp256k1::new(), &mut secp256k1::rand::rng());
    let owner = human.x_only_public_key().0.to_string();
    let prepare = || Request::CreatePrepare {
        request_id: "create-1".into(),
        relay_url: "wss://relay.example.com".into(),
        owner: owner.clone(),
    };
    let Response::Prepared { id, pubkey, .. } = client.request(prepare()).await.unwrap() else {
        panic!("expected prepared");
    };
    let Response::Prepared { pubkey: again, .. } = client.request(prepare()).await.unwrap() else {
        panic!("expected prepared");
    };
    assert_eq!(pubkey, again, "a retried prepare must reuse the key");

    // An attestation for a different key is refused, and nothing is saved.
    let stranger = SecretKey::new(&mut secp256k1::rand::rng())
        .x_only_public_key(&Secp256k1::new())
        .0
        .to_string();
    let refused = client
        .request(Request::CreateCommit {
            request_id: "create-1".into(),
            edit: edit("Scout"),
            auth: attest(&human, &stranger),
        })
        .await;
    assert!(matches!(refused, Err(RpcError::Failed(_))), "{refused:?}");

    let Response::Agents(created) = client
        .request(Request::CreateCommit {
            request_id: "create-1".into(),
            edit: edit("Scout"),
            auth: attest(&human, &pubkey),
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    let agent = &created.agents[0];
    assert_eq!(
        (agent.id.as_str(), agent.name.as_str()),
        (id.as_str(), "Scout")
    );
    assert_eq!(agent.status, ProcessStatus::Stopped);
    // Mission control reads how long it has been in that status.
    assert!(agent.status_since.is_some_and(|since| since > 0));
    assert!(
        state.path().join("keys").join(format!("{id}.key")).exists(),
        "the key is saved before the agent"
    );

    // Other GUIs learn about the change without asking.
    let pushed = pushed_agents(&mut events, |s| s.agents.len() == 1).await;
    assert!(pushed.seq > initial.seq);

    let Response::Agents(saved) = client
        .request(Request::Save {
            id: id.clone(),
            revision: agent.revision,
            edit: AgentEdit {
                workspace: agent.workspace.clone(),
                ..edit("Scout II")
            },
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    assert_eq!(saved.agents[0].name, "Scout II");

    let stale = client
        .request(Request::Save {
            id: id.clone(),
            revision: agent.revision,
            edit: AgentEdit {
                workspace: agent.workspace.clone(),
                ..edit("Lost update")
            },
        })
        .await;
    assert!(stale.is_err(), "a stale revision must not overwrite");
    assert_eq!(
        (saved.restarted, saved.restart_failures),
        (Some(0), Some(0)),
        "a stopped agent is not restarted"
    );

    // An agent's own effort and conversation context can be set, then
    // cleared back to its defaults, over the wire.
    let save = |revision: u64, effort, session_policy| Request::Save {
        id: id.clone(),
        revision,
        edit: AgentEdit {
            workspace: agent.workspace.clone(),
            effort,
            session_policy,
            ..edit("Scout II")
        },
    };
    let Response::Agents(set) = client
        .request(save(
            saved.agents[0].revision,
            Some(Some("high".into())),
            Some(Some(SessionPolicy::Channel)),
        ))
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    assert_eq!(set.agents[0].launch_effort.as_deref(), Some("high"));
    assert_eq!(set.agents[0].session_policy, Some(SessionPolicy::Channel));
    let Response::Agents(kept) = client
        .request(save(set.agents[0].revision, None, None))
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    assert_eq!(kept.agents[0].launch_effort.as_deref(), Some("high"));
    assert_eq!(kept.agents[0].session_policy, Some(SessionPolicy::Channel));
    let Response::Agents(cleared) = client
        .request(save(kept.agents[0].revision, Some(None), Some(None)))
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    let agent = &cleared.agents[0];
    assert_eq!(agent.launch_effort, None);
    assert_eq!(agent.session_policy, None);
    assert_eq!(agent.harness.command, "goose", "the harness is put back");
    assert_eq!(agent.name, "Scout II");

    let Response::Agents(deleted) = client
        .request(Request::Delete {
            id,
            revision: agent.revision,
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    assert!(deleted.agents.is_empty());
}
