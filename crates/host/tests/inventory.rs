//! The inventory, Use here, clone, start and model requests a GUI sends
//! besides plain agent settings.

use buzz_host::api::*;
use buzz_host::credentials::FileCredentials;
use buzz_host::{AgentHost, HostConfig, RpcHandler};
use buzz_host_rpc::{
    ClientHandler, ConnectionState, HostClient, HostClientOptions, HostInfo, RpcError,
    ServerOptions, serve, transport,
};
use futures::future::BoxFuture;
use secp256k1::{Keypair, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

struct Gui(Keypair);

fn sign(human: &Keypair, text: &str) -> String {
    let digest: [u8; 32] = Sha256::digest(text).into();
    Secp256k1::new()
        .sign_schnorr_no_aux_rand(&digest, human)
        .to_string()
}

fn attest(human: &Keypair, agent: &str) -> String {
    serde_json::to_string(&[
        "auth",
        &human.x_only_public_key().0.to_string(),
        "",
        &sign(human, &format!("nostr:agent-auth:{agent}:")),
    ])
    .unwrap()
}

impl ClientHandler<HostProtocol> for Gui {
    fn handle(&self, ask: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        let answer = match ask {
            HostAsk::AttestAgent { agent_pubkey } => {
                Ok(HostAnswer::Attestation(attest(&self.0, &agent_pubkey)))
            }
            HostAsk::OpenUrl { .. } => Err(RpcError::Failed("no browser".into())),
        };
        Box::pin(async move { answer })
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    human: Keypair,
    client: HostClient<HostProtocol>,
    _host: Arc<AgentHost>,
}

fn key() -> (String, String) {
    let secret = SecretKey::new(&mut secp256k1::rand::rng());
    let pubkey = secret.x_only_public_key(&Secp256k1::new()).0.to_string();
    (secret.display_secret().to_string(), pubkey)
}

/// A host whose earlier Buzz install holds `records`.
async fn fixture(records: serde_json::Value) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let library = dir
        .path()
        .join("legacy/xyz.block.buzz.app/agents/managed-agents.json");
    std::fs::create_dir_all(library.parent().unwrap()).unwrap();
    std::fs::write(&library, records.to_string()).unwrap();
    let host = AgentHost::start(HostConfig {
        info: HostInfo {
            id: "test".into(),
            name: "Test".into(),
            platform: "linux".into(),
            version: "test".into(),
        },
        state_dir: dir.path().join("state"),
        workspace: dir.path().join("workspace"),
        credentials: Arc::new(FileCredentials::new(dir.path().join("keys")).unwrap()),
        watch_interval: Duration::from_millis(100),
        legacy_dir: Some(dir.path().join("legacy")),
    })
    .unwrap();
    let (connector, listener) = transport::memory();
    tokio::spawn(serve(
        listener,
        Arc::new(RpcHandler(host.clone())),
        ServerOptions::default(),
    ));
    let human = Keypair::new(&Secp256k1::new(), &mut secp256k1::rand::rng());
    let client = HostClient::<HostProtocol>::spawn(
        &tokio::runtime::Handle::current(),
        Arc::new(connector),
        Arc::new(Gui(human)),
        HostClientOptions::default(),
    );
    client
        .state()
        .wait_for(|state| matches!(state, ConnectionState::Connected { .. }))
        .await
        .unwrap();
    Fixture {
        dir,
        human,
        client,
        _host: host,
    }
}

async fn agents(f: &Fixture) -> AgentsSnapshot {
    let Response::Agents(snapshot) = f.client.request(Request::Agents).await.unwrap() else {
        panic!("expected agents");
    };
    snapshot
}

#[tokio::test(flavor = "multi_thread")]
async fn the_snapshot_lists_harnesses_and_old_buzz_identities() {
    let (secret, pubkey) = key();
    let f = fixture(serde_json::json!([{
        "pubkey": pubkey, "name": "Scout", "private_key_nsec": secret,
        "system_prompt": "Find things.",
    }]))
    .await;
    let snapshot = agents(&f).await;
    let options = snapshot.harness_options.as_ref().expect("harness options");
    assert_eq!(options[0].label, "Buzz Agent");
    assert!(snapshot.claude_setup.is_some());
    assert!(snapshot.agent_defaults.is_some());
    assert_eq!(snapshot.import_available, Some(true));
    assert!(snapshot.local_inventory_actions);
    assert_eq!(
        snapshot
            .default_settings
            .as_ref()
            .map(|d| d.harness.as_str()),
        Some("buzz-agent")
    );
    let parked: Vec<_> = snapshot
        .parked
        .iter()
        .map(|row| (row.pubkey.as_str(), row.name.as_str(), row.sources.clone()))
        .collect();
    assert_eq!(
        parked,
        [(pubkey.as_str(), "Scout", vec![ImportSource::Installed])]
    );

    let Response::Library(library) = f.client.request(Request::Library).await.unwrap() else {
        panic!("expected the library");
    };
    assert_eq!(library.identities[0].pubkey, pubkey);
    assert_eq!(library.identities[0].name, "Scout");

    let Response::CloneSettings(settings) = f
        .client
        .request(Request::CloneSettings {
            source: ImportSource::Installed,
            pubkey: pubkey.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected clone settings");
    };
    assert_eq!(settings.name, "Scout");
    assert_eq!(settings.system_prompt, "Find things.");
}

#[tokio::test(flavor = "multi_thread")]
async fn use_here_checks_the_owners_signature() {
    let (secret, pubkey) = key();
    let f = {
        let human = Keypair::new(&Secp256k1::new(), &mut secp256k1::rand::rng());
        let f = fixture(serde_json::json!([{
            "pubkey": pubkey, "name": "Scout", "private_key_nsec": secret,
            "system_prompt": "Find things.", "auth_tag": attest(&human, &pubkey),
        }]))
        .await;
        Fixture { human, ..f }
    };
    let Response::ImportPreview(preview) = f
        .client
        .request(Request::ImportPreview {
            source: ImportSource::Installed,
            destination: "wss://first.example.com".into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected a preview");
    };
    let candidate = preview.candidates[0].clone();
    let Response::Agents(imported) = f
        .client
        .request(Request::ImportCommit {
            token: preview.token,
            ids: vec![candidate.id.clone()],
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    let agent = imported.agents[0].clone();
    assert!(agent.configured, "{agent:?}");

    let relay = "wss://relay.example.com";
    let owner = f.human.x_only_public_key().0.to_string();
    let resolution = |signature: String| CommunityResolution {
        pubkey: pubkey.clone(),
        relay_url: relay.into(),
        owner: owner.clone(),
        signature,
    };
    let forged = f
        .client
        .request(Request::UseHere {
            id: agent.id.clone(),
            resolution: resolution("00".repeat(64)),
        })
        .await;
    assert!(matches!(forged, Err(RpcError::Failed(_))), "{forged:?}");

    // A signed resolution passes; an identity already set up elsewhere is
    // cloned rather than moved.
    let signed = sign(&f.human, &format!("nostr:agent-community:{pubkey}:{relay}"));
    let Err(RpcError::Failed(message)) = f
        .client
        .request(Request::UseHere {
            id: agent.id.clone(),
            resolution: resolution(signed),
        })
        .await
    else {
        panic!("expected a refusal");
    };
    assert!(message.contains("Clone it"), "{message}");

    let Response::CloneSettings(settings) = f
        .client
        .request(Request::LocalCloneSettings {
            id: agent.id.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected clone settings");
    };
    assert_eq!(settings.name, "Scout");
}

#[tokio::test(flavor = "multi_thread")]
async fn replaced_creates_and_stray_mentions_are_refused() {
    let f = fixture(serde_json::json!([])).await;
    let owner = f.human.x_only_public_key().0.to_string();
    let Response::Prepared { pubkey, saved, .. } = f
        .client
        .request(Request::CreatePrepare {
            request_id: uuid::Uuid::new_v4().to_string(),
            relay_url: "wss://relay.example.com".into(),
            owner: owner.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected prepared");
    };
    assert!(!saved);
    let Response::Prepared { .. } = f
        .client
        .request(Request::CreatePrepare {
            request_id: "not-the-same".into(),
            relay_url: "wss://relay.example.com".into(),
            owner: owner.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("expected prepared");
    };
    // A replaced request cannot commit.
    let stale = f
        .client
        .request(Request::CreateCommit {
            request_id: "stale".into(),
            edit: AgentEdit {
                name: "Scout".into(),
                harness: HarnessEdit {
                    command: "goose".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            auth: attest(&f.human, &pubkey),
        })
        .await;
    assert!(stale.is_err());

    let attach = f
        .client
        .request(Request::AttachMention {
            id: "missing".into(),
            revision: 1,
            replay_floor: 1,
        })
        .await;
    assert!(matches!(attach, Err(RpcError::Failed(_))), "{attach:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn model_lookups_take_turns_by_ticket() {
    let f = fixture(serde_json::json!([])).await;
    let Response::Ticket(first) = f.client.request(Request::ModelsBegin).await.unwrap() else {
        panic!("expected a ticket");
    };
    let busy = f.client.request(Request::ModelsBegin).await;
    assert!(matches!(busy, Err(RpcError::Failed(_))), "{busy:?}");
    f.client
        .request(Request::ModelsCancel { ticket: first })
        .await
        .unwrap();
    let Response::Ticket(second) = f.client.request(Request::ModelsBegin).await.unwrap() else {
        panic!("expected a ticket");
    };
    assert!(second > first);

    // Buzz Agent without a workspace has nothing to sign in to.
    let refused = f
        .client
        .request(Request::ModelsRun {
            ticket: second,
            request: ModelRequest {
                edit: Some(AgentEdit {
                    name: "Draft".into(),
                    harness: HarnessEdit {
                        command: "buzz-agent".into(),
                        provider: "databricks_v2".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
                action: ModelAction::Refresh,
                ..Default::default()
            },
        })
        .await;
    assert!(matches!(refused, Err(RpcError::Failed(_))), "{refused:?}");

    // Only ports of sign-ins this host opened are forwarded.
    let stray = f
        .client
        .request(Request::SignInCallback {
            port: 8020,
            target: "/?code=x".into(),
        })
        .await;
    assert!(matches!(stray, Err(RpcError::Failed(_))), "{stray:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_sign_in_is_unconfirmed_without_claude() {
    let f = fixture(serde_json::json!([])).await;
    let Response::ClaudeAuth(status) = f.client.request(Request::ClaudeAuth).await.unwrap() else {
        panic!("expected a status");
    };
    if buzz_agent_controller_claude_missing() {
        assert_eq!(status, None);
    }
    let _ = &f.dir;
}

/// The test machine may have Claude Code installed; only assert without it.
fn buzz_agent_controller_claude_missing() -> bool {
    std::env::var_os("PATH")
        .is_none_or(|path| !std::env::split_paths(&path).any(|dir| dir.join("claude").exists()))
}

/// A GUI's host starts nothing before sign-in, and naming the signed-in key
/// restores agents from the host's runtime, not its own thread.
#[tokio::test(flavor = "multi_thread")]
async fn a_guis_host_waits_for_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let host = AgentHost::start_for_owner(HostConfig {
        info: HostInfo {
            id: "test".into(),
            name: "Test".into(),
            platform: "linux".into(),
            version: "test".into(),
        },
        state_dir: dir.path().join("state"),
        workspace: dir.path().join("workspace"),
        credentials: Arc::new(FileCredentials::new(dir.path().join("keys")).unwrap()),
        watch_interval: Duration::from_millis(100),
        legacy_dir: None,
    })
    .unwrap();
    let start = || Request::Action {
        id: "missing".into(),
        action: AgentAction::Start,
        replay_floor: None,
        owner: None,
    };
    let refused = host.request(start()).await;
    assert!(
        matches!(&refused, Err(RpcError::Failed(message)) if message.contains("identity")),
        "{refused:?}"
    );
    let (_, viewer) = key();
    host.set_owner(viewer);
    tokio::time::sleep(Duration::from_millis(200)).await;
    // The host thread survived the sign-in and still answers.
    assert!(matches!(
        host.request(Request::Agents).await,
        Ok(Response::Agents(_))
    ));
    let after = host.request(start()).await;
    assert!(
        matches!(&after, Err(RpcError::Failed(message)) if !message.contains("identity")),
        "{after:?}"
    );
}
