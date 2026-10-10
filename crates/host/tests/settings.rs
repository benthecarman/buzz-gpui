//! Agent defaults, model catalogs and importing from an earlier Buzz, over
//! host-rpc as the GUI uses them.

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
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// The GUI: signs attestations with the human key and records each ask.
struct Gui {
    human: Keypair,
    asked: Mutex<Vec<String>>,
}

struct Answers(Arc<Gui>);

impl ClientHandler<HostProtocol> for Answers {
    fn handle(&self, ask: HostAsk) -> BoxFuture<'static, Result<HostAnswer, RpcError>> {
        let HostAsk::AttestAgent { agent_pubkey } = ask else {
            return Box::pin(async { Err(RpcError::Failed("unexpected ask".into())) });
        };
        self.0.asked.lock().unwrap().push(agent_pubkey.clone());
        let tag = attest(&self.0.human, &agent_pubkey);
        Box::pin(async move { Ok(HostAnswer::Attestation(tag)) })
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    host: Arc<AgentHost>,
    client: HostClient<HostProtocol>,
    gui: Arc<Gui>,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
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
    let gui = Arc::new(Gui {
        human: Keypair::new(&Secp256k1::new(), &mut secp256k1::rand::rng()),
        asked: Mutex::default(),
    });
    let client = HostClient::<HostProtocol>::spawn(
        &tokio::runtime::Handle::current(),
        Arc::new(connector),
        Arc::new(Answers(gui.clone())),
        HostClientOptions::default(),
    );
    client
        .state()
        .wait_for(|state| matches!(state, ConnectionState::Connected { .. }))
        .await
        .unwrap();
    Fixture {
        dir,
        host,
        client,
        gui,
    }
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn defaults_save_and_never_return_values() {
    let f = fixture().await;
    let Response::Defaults(initial) = f.client.request(Request::Defaults).await.unwrap() else {
        panic!("expected defaults");
    };
    assert_eq!(initial.harness, "buzz-agent");
    assert_eq!(initial.session_policy, Some(SessionPolicy::Thread));

    let save = |environment: BTreeMap<String, Option<String>>| Request::SaveDefaults {
        edit: DefaultsEdit {
            harness: "goose".into(),
            provider: "openai".into(),
            model: "gpt-5".into(),
            effort: "high".into(),
            session_policy: Some(SessionPolicy::Channel),
            environment,
        },
    };
    let response = f
        .client
        .request(save(BTreeMap::from([(
            "OPENAI_API_KEY".into(),
            Some("sk-never-returned".into()),
        )])))
        .await
        .unwrap();
    let Response::Defaults(saved) = &response else {
        panic!("expected defaults");
    };
    assert_eq!(
        (
            saved.harness.as_str(),
            saved.model.as_str(),
            saved.effort.as_str()
        ),
        ("goose", "gpt-5", "high")
    );
    assert_eq!(saved.session_policy, Some(SessionPolicy::Channel));
    assert_eq!(saved.environment_keys, vec!["OPENAI_API_KEY".to_string()]);
    assert!(!format!("{response:?}").contains("sk-never-returned"));
    let Response::Agents(agents) = f.client.request(Request::Agents).await.unwrap() else {
        panic!("expected agents");
    };
    assert!(!format!("{agents:?}").contains("sk-never-returned"));

    // Absent keys are kept; `None` removes.
    let Response::Defaults(kept) = f.client.request(save(BTreeMap::new())).await.unwrap() else {
        panic!("expected defaults");
    };
    assert_eq!(kept.environment_keys, vec!["OPENAI_API_KEY".to_string()]);
    let Response::Defaults(removed) = f
        .client
        .request(save(BTreeMap::from([("OPENAI_API_KEY".into(), None)])))
        .await
        .unwrap()
    else {
        panic!("expected defaults");
    };
    assert!(removed.environment_keys.is_empty());

    let refused = f
        .client
        .request(Request::SaveDefaults {
            edit: DefaultsEdit {
                harness: "claude".into(),
                ..Default::default()
            },
        })
        .await;
    assert!(matches!(refused, Err(RpcError::Failed(_))), "{refused:?}");
}

fn draft(command: &str, provider: &str, environment: &[(&str, &str)]) -> AgentEdit {
    AgentEdit {
        name: "Draft".into(),
        harness: HarnessEdit {
            command: command.into(),
            provider: provider.into(),
            ..Default::default()
        },
        environment: environment
            .iter()
            .map(|(key, value)| (key.to_string(), Some(value.to_string())))
            .collect(),
        ..Default::default()
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn models_come_from_the_harness() {
    let f = fixture().await;
    let bin = f.dir.path().join("bin");

    // Goose answers its ACP catalog method; the probe runs it with the
    // draft's own environment.
    let goose = bin.join("goose-acp");
    script(
        &goose,
        r#"read line
case "$line" in *supported-models*) ;; *) exit 1;; esac
printf '{"jsonrpc":"2.0","id":1,"result":{"providerId":"openai","models":["%s","gpt-5-mini"]}}\n' "$FAKE_MODEL"
"#,
    );
    let models = |edit: AgentEdit| Request::Models {
        id: None,
        revision: 0,
        edit,
    };
    let Response::Models(catalog) = f
        .client
        .request(models(draft(
            goose.to_str().unwrap(),
            "openai",
            &[("FAKE_MODEL", "gpt-5")],
        )))
        .await
        .unwrap()
    else {
        panic!("expected models");
    };
    let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["gpt-5", "gpt-5-mini"]);
    assert!(!catalog.model_overridden);

    let unconfigured = f
        .client
        .request(models(draft(goose.to_str().unwrap(), "", &[])))
        .await;
    assert!(matches!(unconfigured, Err(RpcError::Failed(_))));

    // Pi: the adapter, the CLI beside it, and Node.
    let pi = bin.join("pi");
    script(&bin.join("buzz-pi-acp"), "exit 0\n");
    script(&bin.join("node"), "exit 0\n");
    script(
        &pi,
        r#"if [ "$1" = "--version" ]; then echo 0.99.1; exit 0; fi
read line
echo '{"type":"extension_ui_request"}'
echo '{"id":"catalog","type":"response","command":"get_available_models","success":true,"data":{"models":[{"provider":"openai","id":"gpt-5"},{"provider":"anthropic","id":"claude-sonnet-4-5"}]}}'
"#,
    );
    let Response::Models(catalog) = f
        .client
        .request(models(draft(
            bin.join("buzz-pi-acp").to_str().unwrap(),
            "",
            &[],
        )))
        .await
        .unwrap()
    else {
        panic!("expected models");
    };
    let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["anthropic/claude-sonnet-4-5", "openai/gpt-5"]);

    let Err(RpcError::Failed(message)) = f
        .client
        .request(models(draft("buzz-agent", "databricks_v2", &[])))
        .await
    else {
        panic!("Buzz Agent lists models only from a Databricks workspace");
    };
    assert!(message.contains("Databricks"), "{message}");
}

fn key() -> (String, String) {
    let secret = SecretKey::new(&mut secp256k1::rand::rng());
    let pubkey = secret.x_only_public_key(&Secp256k1::new()).0.to_string();
    (secret.display_secret().to_string(), pubkey)
}

#[tokio::test(flavor = "multi_thread")]
async fn import_asks_the_gui_to_attest_unattested_agents() {
    let f = fixture().await;
    let ((alpha_secret, alpha), (beta_secret, beta)) = (key(), key());
    let library = f
        .dir
        .path()
        .join("legacy/xyz.block.buzz.app/agents/managed-agents.json");
    std::fs::create_dir_all(library.parent().unwrap()).unwrap();
    let records = serde_json::json!([
        {
            "pubkey": alpha, "name": "Alpha", "private_key_nsec": alpha_secret,
            "system_prompt": "Review code.", "auth_tag": attest(&f.gui.human, &alpha),
        },
        {
            "pubkey": beta, "name": "Beta", "private_key_nsec": beta_secret,
            "system_prompt": "Write docs.",
        },
    ]);
    std::fs::write(&library, records.to_string()).unwrap();
    let original = std::fs::read(&library).unwrap();

    let preview = || Request::ImportPreview {
        source: ImportSource::Installed,
        destination: "wss://relay.example.com".into(),
    };
    let Response::ImportPreview(listed) = f.client.request(preview()).await.unwrap() else {
        panic!("expected a preview");
    };
    assert!(!listed.token.is_empty());
    let by_name = |name: &str| {
        listed
            .candidates
            .iter()
            .find(|candidate| candidate.name == name)
            .unwrap()
            .clone()
    };
    let (a, b) = (by_name("Alpha"), by_name("Beta"));
    assert!(!a.needs_attestation && b.needs_attestation);
    assert!(!a.imported && !b.imported);

    // Without a GUI to sign, the unattested agent cannot be imported.
    let refused = f
        .host
        .request(Request::ImportCommit {
            token: listed.token.clone(),
            ids: vec![b.id.clone()],
        })
        .await;
    assert!(matches!(refused, Err(RpcError::Failed(_))), "{refused:?}");

    let Response::Agents(snapshot) = f
        .client
        .request(Request::ImportCommit {
            token: listed.token.clone(),
            ids: vec![a.id.clone(), b.id.clone()],
        })
        .await
        .unwrap()
    else {
        panic!("expected agents");
    };
    assert_eq!(*f.gui.asked.lock().unwrap(), vec![beta.clone()]);
    let mut names: Vec<&str> = snapshot.agents.iter().map(|a| a.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Alpha", "Beta"]);
    assert!(snapshot.agents.iter().all(|agent| !agent.enabled));

    // The attestation is saved with Beta; the old library is untouched and
    // the staged copy is gone.
    let saved =
        std::fs::read_to_string(f.dir.path().join("state/agent-controller/agents.json")).unwrap();
    assert!(saved.contains(&attest(&f.gui.human, &beta).replace('"', "\\\"")));
    assert_eq!(std::fs::read(&library).unwrap(), original);
    let staging = f.dir.path().join("state/import");
    assert_eq!(std::fs::read_dir(staging).unwrap().count(), 0);
    for (id, pubkey) in [(&a.id, &alpha), (&b.id, &beta)] {
        assert!(
            f.dir.path().join(format!("keys/{id}.key")).exists(),
            "{pubkey}"
        );
    }

    // A preview is used once; a fresh one marks both as imported.
    let reused = f
        .client
        .request(Request::ImportCommit {
            token: listed.token,
            ids: vec![a.id.clone()],
        })
        .await;
    assert!(reused.is_err());
    let Response::ImportPreview(again) = f.client.request(preview()).await.unwrap() else {
        panic!("expected a preview");
    };
    assert!(again.candidates.iter().all(|candidate| candidate.imported));
}

#[tokio::test(flavor = "multi_thread")]
async fn import_without_a_library_explains_itself() {
    let f = fixture().await;
    let missing = f
        .client
        .request(Request::ImportPreview {
            source: ImportSource::Development,
            destination: "wss://relay.example.com".into(),
        })
        .await;
    assert!(matches!(missing, Err(RpcError::Failed(_))), "{missing:?}");
}
