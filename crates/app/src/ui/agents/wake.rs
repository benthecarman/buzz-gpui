//! Mention wake: buzz-app's `bindAgentMentions` and `prepareMention`
//! (`src/features/agents/{mention-wake,control}.ts`) and
//! `AgentWakeNotice`.
//!
//! A message that mentions the viewer's own stopped agent starts it, and
//! the agent replays the channel from just before the message. A start
//! already waiting or starting gets the mention attached instead.

use crate::community::{AppState, IdentityState};
use crate::hosts::Hosts;
use crate::hosts::control::Status;
use crate::ui::toasts::{self, Notice};
use buzz_host::api::{AgentAction, ProcessStatus, Request, Response};
use buzz_relay_client::RelayOrigin;
use gpui_kit::*;

/// The Unix time to replay from for a message sent now.
pub fn floor() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().saturating_sub(1))
}

/// A message to the community at `origin` mentioned `recipients`.
pub fn sent(origin: &RelayOrigin, recipients: &[String], floor: u64, cx: &mut App) {
    if recipients.is_empty() || cx.try_global::<Hosts>().is_none() {
        return;
    }
    let owner = match &cx.global::<AppState>().identity {
        IdentityState::Ready(identity) => Some(identity.pubkey_hex()),
        _ => return,
    };
    let community = origin.http();
    let hosts = cx.global::<Hosts>().hosts.clone();
    for host in hosts {
        let mentioned = host.read(cx).agents.agents.iter().any(|agent| {
            agent.configured
                && recipients.contains(&agent.pubkey)
                && super::inventory::origin(&agent.relay_url) == community
        });
        if !mentioned {
            continue;
        }
        let recipients = recipients.to_vec();
        let community = community.clone();
        let owner = owner.clone();
        let refresh = host.update(cx, |host, cx| host.refresh(cx));
        cx.spawn(async move |cx| {
            refresh.await;
            let ready = host.read_with(cx, |host, _| host.status() == Status::Ready);
            if !ready {
                cx.update(|cx| {
                    show(
                        "Message sent, but local agents could not be read. Open Agents to retry."
                            .into(),
                        cx,
                    )
                });
                return;
            }
            let agents = host.read_with(cx, |host, _| {
                host.agents
                    .agents
                    .iter()
                    .filter(|agent| {
                        agent.configured
                            && recipients.contains(&agent.pubkey)
                            && super::inventory::origin(&agent.relay_url) == community
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let mut failures: Vec<String> = Vec::new();
            for agent in agents {
                let pending = host.read_with(cx, |host, _| {
                    host.control.pending_launch.as_deref() == Some(agent.id.as_str())
                });
                if agent.status == ProcessStatus::Running && !pending {
                    continue;
                }
                if matches!(
                    agent.status,
                    ProcessStatus::Waiting | ProcessStatus::Starting
                ) || pending
                {
                    let attach = host.update(cx, |host, cx| {
                        host.request(
                            Request::AttachMention {
                                id: agent.id.clone(),
                                revision: agent.revision,
                                replay_floor: floor,
                            },
                            cx,
                        )
                    });
                    if attach.await.is_err() {
                        failures.push(format!(
                            "{}'s pending launch could not confirm replay of this mention. Open \
                             Agents to check its status.",
                            agent.name
                        ));
                    }
                    continue;
                }
                let start = host.update(cx, |host, cx| {
                    host.act(
                        &agent.id,
                        AgentAction::Start,
                        owner.clone(),
                        Some(floor),
                        cx,
                    )
                });
                match start.await {
                    Ok(Response::Agents(snapshot)) => {
                        let started = snapshot.agents.iter().find(|a| a.id == agent.id);
                        if started.map(|a| a.status) != Some(ProcessStatus::Running) {
                            failures.push(format!(
                                "{} could not start. {}",
                                agent.name,
                                started.and_then(|a| a.error.clone()).unwrap_or_else(|| {
                                    "Open Agents to check its status.".into()
                                })
                            ));
                        }
                    }
                    _ => {
                        let error = host.read_with(cx, |host, _| host.control.error.clone());
                        failures.push(format!(
                            "{} could not start. {}",
                            agent.name,
                            error.unwrap_or_else(|| "Open Agents to retry.".into())
                        ));
                    }
                }
            }
            if !failures.is_empty() {
                cx.update(|cx| show(format!("Message sent, but {}", failures.join(" ")), cx));
            }
        })
        .detach();
    }
}

/// "Agent could not start", buzz-app's `AgentWakeNotice`.
fn show(message: String, cx: &mut App) {
    toasts::show(
        Notice::error("agent-wake", "Agent could not start").description(message),
        cx,
    );
}
