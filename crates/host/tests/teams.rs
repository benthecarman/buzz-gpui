//! Team requests over host RPC: capture, preview, team member creation
//! and team instructions, and how a host from before teams answers them.

use buzz_host::api::*;
use buzz_host::credentials::FileCredentials;
use buzz_host::{AgentHost, HostConfig, RpcHandler};
use buzz_host_rpc::{
    ClientHandler, EventSource, HostClient, HostClientOptions, HostHandler, HostInfo, Peer,
    Protocol, RpcError, ServerOptions, serve, transport,
};
use futures::future::BoxFuture;
use secp256k1::{Keypair, Secp256k1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const RELAY: &str = "wss://relay.example.com";
const COMMUNITY: &str = "https://relay.example.com";

struct NoAsks;

impl ClientHandler<HostProtocol> for NoAsks {
    fn handle(&self, _: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        Box::pin(async { Err(RpcError::Failed("unexpected ask".into())) })
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

fn edit(name: &str) -> AgentEdit {
    AgentEdit {
        name: name.into(),
        system_prompt: format!("{name} helps."),
        harness: HarnessEdit {
            command: "goose".into(),
            ..Default::default()
        },
        ..Default::default()
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

async fn connect<P: Protocol, H: HostHandler<P>>(
    handler: Arc<H>,
    asks: Arc<dyn ClientHandler<HostProtocol>>,
) -> HostClient<HostProtocol> {
    let (connector, listener) = transport::memory();
    tokio::spawn(serve::<P, _>(listener, handler, ServerOptions::default()));
    let client = HostClient::<HostProtocol>::spawn(
        &tokio::runtime::Handle::current(),
        Arc::new(connector),
        asks,
        HostClientOptions::default(),
    );
    client
        .state()
        .wait_for(|s| matches!(s, buzz_host_rpc::ConnectionState::Connected { .. }))
        .await
        .unwrap();
    client
}

async fn create(
    client: &HostClient<HostProtocol>,
    human: &Keypair,
    request: &str,
    name: &str,
) -> Agent {
    let owner = human.x_only_public_key().0.to_string();
    let Response::Prepared { id, pubkey, .. } = client
        .request(Request::CreatePrepare {
            request_id: request.into(),
            relay_url: RELAY.into(),
            owner,
        })
        .await
        .unwrap()
    else {
        panic!("expected prepared");
    };
    let Response::Agents(snapshot) = client
        .request(Request::CreateCommit {
            request_id: request.into(),
            edit: edit(name),
            auth: attest(human, &pubkey),
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    snapshot.agents.into_iter().find(|a| a.id == id).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn captures_previews_imports_and_instructs_a_team() {
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
    let client = connect::<HostProtocol, _>(Arc::new(RpcHandler(host)), Arc::new(NoAsks)).await;
    let human = Keypair::new(&Secp256k1::new(), &mut secp256k1::rand::rng());
    let owner = human.x_only_public_key().0.to_string();
    let scout = create(&client, &human, "create-1", "Scout").await;
    let writer = create(&client, &human, "create-2", "Writer").await;

    // Capture: the members' current saved settings, portable only.
    let Response::TeamSnapshot(captured) = client
        .request(Request::TeamCapture {
            team: TeamMeta {
                name: "Crew".into(),
                description: Some("Two agents".into()),
                instructions: Some("Pair up.".into()),
            },
            members: vec![scout.pubkey.clone(), writer.pubkey.clone()],
            community: COMMUNITY.into(),
        })
        .await
        .unwrap()
    else {
        panic!("expected a team snapshot");
    };
    assert_eq!(captured.format, "buzz-team-snapshot");
    assert_eq!(captured.team.instructions.as_deref(), Some("Pair up."));
    assert_eq!(captured.members.len(), 2);
    let member = &captured.members[0];
    assert_eq!(member.definition.name, "Scout");
    assert_eq!(member.definition.runtime.as_deref(), Some("goose"));
    assert_eq!(member.memory.level, "none");
    assert_eq!(member.profile.display_name, "Scout");
    let unknown = client
        .request(Request::TeamCapture {
            team: TeamMeta {
                name: "Crew".into(),
                ..Default::default()
            },
            members: vec!["0".repeat(64)],
            community: COMMUNITY.into(),
        })
        .await
        .unwrap_err();
    assert_eq!(unknown.to_string(), "A team member is unavailable");

    // Preview: the file's JSON comes back validated.
    let content = serde_json::to_string(&captured).unwrap();
    let Response::TeamSnapshot(previewed) = client
        .request(Request::TeamPreview { content })
        .await
        .unwrap()
    else {
        panic!("expected a team snapshot");
    };
    assert_eq!(previewed, captured);
    let invalid = client
        .request(Request::TeamPreview {
            content: r#"{"format":"buzz-agent-snapshot"}"#.into(),
        })
        .await
        .unwrap_err();
    assert_eq!(invalid.to_string(), "Invalid team snapshot JSON");

    // Import: a fresh identity bound to the team; a retried request
    // finds the agent it already created.
    let request = uuid::Uuid::new_v4().to_string();
    let prepare = || Request::TeamMemberPrepare {
        request_id: request.clone(),
        relay_url: RELAY.into(),
        owner: owner.clone(),
    };
    let Response::TeamMemberPrepared { id, pubkey, saved } =
        client.request(prepare()).await.unwrap()
    else {
        panic!("expected a prepared member");
    };
    assert!(!saved);
    let bundle = BundleMember {
        team: "team-1".into(),
        member: captured.members[0].clone(),
        instructions: "Pair up.".into(),
        keep_allowlist: false,
    };
    let with_memory = BundleMember {
        member: MemberSnapshot {
            memory: MemberMemory {
                level: "core".into(),
                entries: vec![MemoryEntry {
                    slug: "core".into(),
                    body: "Remember.".into(),
                }],
            },
            ..bundle.member.clone()
        },
        ..bundle.clone()
    };
    let refused = client
        .request(Request::TeamMemberCommit {
            request_id: request.clone(),
            edit: edit("Scout"),
            auth: attest(&human, &pubkey),
            bundle: Box::new(with_memory),
        })
        .await
        .unwrap_err();
    assert_eq!(
        refused.to_string(),
        "Memory import is unavailable; export Team only"
    );
    let Response::Agents(imported) = client
        .request(Request::TeamMemberCommit {
            request_id: request.clone(),
            edit: edit("Scout"),
            auth: attest(&human, &pubkey),
            bundle: Box::new(bundle),
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    let copy = imported.agents.iter().find(|a| a.id == id).unwrap();
    assert_ne!(copy.pubkey, scout.pubkey, "a copy has its own identity");
    assert!(copy.profile_pending);
    let Response::TeamMemberPrepared {
        id: again,
        saved: true,
        ..
    } = client.request(prepare()).await.unwrap()
    else {
        panic!("expected the saved member");
    };
    assert_eq!(again, id);

    // Instructions apply only while the owner's team lists the member.
    let teams = |members: Vec<String>| {
        BTreeMap::from([(
            "team-1".to_string(),
            TeamCatalogEntry {
                created_at: 1_700_000_000,
                event_id: "a".repeat(64),
                members,
            },
        )])
    };
    let instruct = |teams| Request::TeamInstructions {
        id: copy.id.clone(),
        revision: copy.revision,
        instructions: "Ship it.".into(),
        team: "team-1".into(),
        community: COMMUNITY.into(),
        owner: owner.clone(),
        teams,
    };
    let stale = client.request(instruct(teams(vec![]))).await.unwrap_err();
    assert_eq!(
        stale.to_string(),
        "Team no longer contains this member; refresh before deploying"
    );
    let Response::Agents(instructed) = client
        .request(instruct(teams(vec![copy.pubkey.clone()])))
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    let instructed = instructed.agents.iter().find(|a| a.id == copy.id).unwrap();
    assert!(instructed.revision > copy.revision);
}

/// The host protocol before teams: it does not know their requests.
struct Old;

#[derive(Clone, Debug, Serialize, Deserialize)]
enum OldRequest {
    Agents,
}

impl Protocol for Old {
    type ClientRequest = OldRequest;
    type ClientResponse = Response;
    type HostRequest = HostAsk;
    type HostResponse = HostAnswer;
    type Event = HostEvent;
}

struct OldHost(tokio::sync::broadcast::Sender<(u64, HostEvent)>);

impl HostHandler<Old> for OldHost {
    fn info(&self) -> HostInfo {
        info()
    }

    fn events(&self) -> EventSource<Old> {
        self.0.subscribe()
    }

    fn handle(
        &self,
        _: Peer<Old>,
        _: OldRequest,
    ) -> BoxFuture<'static, Result<Response, RpcError>> {
        Box::pin(async { Ok(Response::Agents(AgentsSnapshot::default())) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_before_teams_refuses_team_requests_and_stays_connected() {
    let old = Arc::new(OldHost(tokio::sync::broadcast::channel(4).0));
    let client = connect::<Old, _>(old, Arc::new(NoAsks)).await;
    for request in [
        Request::TeamPreview {
            content: "{}".into(),
        },
        Request::TeamCapture {
            team: TeamMeta::default(),
            members: vec![],
            community: COMMUNITY.into(),
        },
        Request::TeamMemberCommit {
            request_id: "r".into(),
            edit: edit("Scout"),
            auth: String::new(),
            bundle: Box::default(),
        },
    ] {
        let error = client.request(request).await.unwrap_err();
        assert!(
            error.to_string().contains("does not support this request"),
            "{error}"
        );
    }
    // Known requests still work on the same connection.
    assert!(matches!(
        client.request(Request::Agents).await,
        Ok(Response::Agents(_))
    ));
}
