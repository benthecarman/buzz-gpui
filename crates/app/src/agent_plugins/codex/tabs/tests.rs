//! buzz-app's `tabs.test.tsx`: the choices the settings offer, what a save
//! checks, and the catalog's own server.

use super::*;
use crate::agent_plugins::codex::config::Effort;
use crate::agent_plugins::codex::rpc::testing::{FakeCodex, gate, until};
use core::prelude::v1::test;

fn models() -> Vec<Model> {
    let effort = |name: &str| Effort {
        reasoning_effort: name.into(),
        description: String::new(),
    };
    vec![
        Model {
            model: "model-a".into(),
            display_name: "Model A".into(),
            description: "Fast".into(),
            is_default: true,
            default_reasoning_effort: "low".into(),
            supported_reasoning_efforts: vec![effort("low"), effort("high")],
        },
        Model {
            model: "model-b".into(),
            display_name: "Model B".into(),
            default_reasoning_effort: "medium".into(),
            supported_reasoning_efforts: vec![effort("medium")],
            ..Default::default()
        },
    ]
}

fn labels(options: &Options) -> Vec<&str> {
    options.iter().map(|(_, label)| label.as_str()).collect()
}

#[test]
fn offers_codex_models_and_the_selected_models_thinking() {
    let models = models();
    let draft = Config::default();
    assert_eq!(
        labels(&model_options(&draft, Some(&models))),
        ["Codex default · Model A", "Model A", "Model B"]
    );
    // Before the catalog loads, the default has no name.
    assert_eq!(labels(&model_options(&draft, None)), ["Codex default"]);
    let selected = selected_model(&draft, &models);
    assert_eq!(selected.map(|model| model.model.as_str()), Some("model-a"));
    assert_eq!(
        labels(&effort_options(&draft, selected)),
        ["Model default", "Low", "High"]
    );
    let b = Config {
        model: "model-b".into(),
        effort: "medium".into(),
        ..Config::default()
    };
    let selected = selected_model(&b, &models);
    assert_eq!(
        labels(&model_options(&b, Some(&models))),
        ["Codex default", "Model A", "Model B"]
    );
    assert_eq!(
        labels(&effort_options(&b, selected)),
        ["Model default", "Medium"]
    );
}

#[test]
fn keeps_saved_choices_codex_no_longer_offers() {
    let models = models();
    let retired = Config {
        model: "retired-model".into(),
        effort: "xhigh".into(),
        ..Config::default()
    };
    let options = model_options(&retired, Some(&models));
    assert_eq!(
        labels(&options),
        [
            "Codex default",
            "retired-model (unavailable)",
            "Model A",
            "Model B"
        ]
    );
    assert_eq!(options[1].0, "retired-model");
    let selected = selected_model(&retired, &models);
    assert!(selected.is_none());
    // Without a model to offer efforts, the saved one still shows.
    assert_eq!(
        labels(&effort_options(&retired, selected)),
        ["Model default", "Extra high (unavailable)"]
    );
}

#[test]
fn saves_only_a_valid_workspace_and_restores_the_default_when_cleared() {
    let relative = Config {
        workspace: "relative".into(),
        ..Config::default()
    };
    assert_eq!(
        validate(&relative),
        Err("Enter an absolute workspace path or ~/.buzz.".into())
    );
    let custom = Config {
        workspace: " /tmp/codex ".into(),
        instructions: "Custom instructions".into(),
        ..Config::default()
    };
    assert_eq!(
        validate(&custom).map(|config| config.workspace),
        Ok("/tmp/codex".into())
    );
    let cleared = Config {
        workspace: "   ".into(),
        ..Config::default()
    };
    assert_eq!(validate(&cleared), Ok(Config::default()));
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_the_account_and_models_then_ends_its_server() {
    let fake = FakeCodex::default();
    fake.state().models = serde_json::json!([
        { "model": "model-a", "displayName": "Model A", "isDefault": true },
    ]);
    let catalog = catalog(fake.spawner(), Signal::default()).await.unwrap();
    assert_eq!(catalog.account, "test@example.com");
    assert_eq!(catalog.models[0].display_name, "Model A");
    assert_eq!(fake.state().ended, 1);
    // Its server starts where the host starts it, not in a workspace.
    assert_eq!(fake.spawned("app-server")[0].cwd, None);

    fake.state().account = serde_json::json!({ "account": { "type": "apiKey" } });
    let catalog = super::catalog(fake.spawner(), Signal::default())
        .await
        .unwrap();
    assert_eq!(catalog.account, "apiKey");
    fake.state().account = serde_json::json!({ "account": null });
    assert_eq!(
        super::catalog(fake.spawner(), Signal::default()).await,
        Err(SIGN_IN.to_string())
    );
    assert_eq!(fake.state().ended, 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn closes_a_server_returned_after_the_tab_went_away() {
    let fake = FakeCodex::default();
    let (started, began) = tokio::sync::oneshot::channel();
    let (release, open) = gate();
    fake.state().hold_server =
        Some(crate::agent_plugins::codex::rpc::testing::Held { started, release });
    let signal = Signal::default();
    let checking = tokio::spawn(catalog(fake.spawner(), signal.clone()));
    began.await.unwrap();
    signal.abort("Codex check closed");
    open.send(()).unwrap();
    assert!(checking.await.unwrap().is_err());
    until(|| fake.state().ended == 1).await;
    assert!(fake.calls("account/read").is_empty());
}

#[test]
fn says_where_codex_stands() {
    let mut check = CatalogCheck {
        host: Host {
            spawn: None,
            label: "This computer".into(),
            remote: false,
        },
        data: None,
        error: String::new(),
        checking: true,
        signal: Signal::default(),
        _task: None,
    };
    assert_eq!(status_line(&check), "Checking Codex…");
    check.checking = false;
    assert_eq!(status_line(&check), "Codex needs attention.");
    check.data = Some(Catalog {
        models: vec![],
        account: "me@example.com".into(),
    });
    assert_eq!(status_line(&check), "Codex is ready (me@example.com).");
}
