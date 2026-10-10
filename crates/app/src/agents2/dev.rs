//! A development-only agent type, "Echo", for trying Agents2 without a
//! real type: it answers every mention with the mention's text. Debug
//! builds register it when `BUZZ_DEV_AGENTS2` is set; release builds never
//! include it.

use super::Agent;
use super::attention::{AttentionValue, Interest};
use super::registry::{AgentType, TypeDefaults, register_type};
use super::runtime::{Delivery, Trigger};
use buzz_host::api::AppAgentEvent;
use gpui_kit::{App, AppContext, SharedString, Task};
use serde_json::json;
use std::sync::Arc;

struct Echo;

impl AgentType for Echo {
    fn id(&self) -> &'static str {
        "echo"
    }

    fn title(&self) -> SharedString {
        "Echo".into()
    }

    fn description(&self) -> Option<SharedString> {
        Some("Development only: answers every mention with its text.".into())
    }

    fn defaults(&self) -> TypeDefaults {
        TypeDefaults {
            config: json!({ "prefix": "echo: " }),
            attention: [(
                "interest/default".to_owned(),
                AttentionValue::Interest(Interest {
                    instructions: "Answer every mention.".into(),
                }),
            )]
            .into(),
        }
    }

    fn summary(&self, agent: &Agent) -> Option<String> {
        agent
            .config
            .get("prefix")
            .and_then(|prefix| prefix.as_str())
            .map(|prefix| format!("Replies \u{201c}{}\u{2026}\u{201d}", prefix.trim()))
    }

    fn runs(&self) -> bool {
        true
    }

    fn run(&self, delivery: Delivery, cx: &mut App) -> Task<Result<(), String>> {
        let Trigger::Mention { event } = delivery.trigger else {
            return Task::ready(Ok(()));
        };
        let Some(channel) = event.tag("h").map(str::to_owned).or(delivery.channel) else {
            return Task::ready(Ok(()));
        };
        let prefix = delivery
            .config
            .get("prefix")
            .and_then(|prefix| prefix.as_str())
            .unwrap_or("echo: ")
            .to_owned();
        let agent = delivery.agent;
        cx.background_spawn(async move {
            agent
                .publish(AppAgentEvent {
                    kind: 9,
                    content: format!("{prefix}{}", event.content),
                    tags: vec![
                        vec!["h".into(), channel],
                        vec!["e".into(), event.id.clone(), String::new(), "reply".into()],
                        vec!["p".into(), event.pubkey.clone()],
                    ],
                })
                .await
                .map(drop)
        })
    }
}

pub(super) fn init(cx: &mut App) {
    if std::env::var_os("BUZZ_DEV_AGENTS2").is_some() {
        register_type("buzz.dev", Arc::new(Echo), cx).expect("the echo type is valid");
    }
}
