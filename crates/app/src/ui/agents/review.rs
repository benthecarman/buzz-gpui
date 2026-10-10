//! Agent-requested updates: buzz-app's `AgentUpdateReview` and
//! `parseAgentManagementRequest`.
//!
//! An agent can ask its owner, in an owner-addressed activity frame, to
//! change its own settings. The owner reviews the change in the Edit
//! dialog, pre-filled; nothing is written without a save. A request counts
//! only from an agent that is a member of the channel it names.

use super::draft::{self, Draft};
use super::edit::EditDialog;
use super::editor::Dismissed;
use crate::community::{AppState, Community};
use crate::hosts::{HostConnection, Hosts};
use buzz_host::api::{Agent, HarnessOption};
use buzz_ui::{Button, ButtonSize, Toast, ToastTone};
use gpui_kit::*;
use serde_json::Value;
use std::collections::VecDeque;

const QUEUE_LIMIT: usize = 200;

/// A request, as an agent sends it.
#[derive(Clone, Debug, PartialEq)]
pub enum ManagementRequest {
    Create {
        request_id: String,
        channel_id: String,
    },
    Update {
        request_id: String,
        channel_id: String,
        agent_name: String,
        display_name: Option<String>,
        system_prompt: Option<String>,
        runtime: Option<String>,
        provider: Option<String>,
        model: Option<String>,
    },
}

impl ManagementRequest {
    fn request_id(&self) -> &str {
        match self {
            Self::Create { request_id, .. } | Self::Update { request_id, .. } => request_id,
        }
    }

    fn channel_id(&self) -> &str {
        match self {
            Self::Create { channel_id, .. } | Self::Update { channel_id, .. } => channel_id,
        }
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
}

/// Parses only the narrow, secret-free contract; anything else is ignored.
pub fn parse(plaintext: &str) -> Option<ManagementRequest> {
    let envelope: Value = serde_json::from_str(plaintext).ok()?;
    if envelope.get("kind")?.as_str()? != "agent_management_request" {
        return None;
    }
    let payload = envelope.get("payload")?.as_object()?;
    let request = payload.get("request")?.as_object()?;
    if payload.get("type")?.as_str()? != "agent_management_request" {
        return None;
    }
    let request_id = text(payload.get("requestId"))?;
    let only = |allowed: &[&str]| request.keys().all(|key| allowed.contains(&key.as_str()));
    let channel_id = text(request.get("channelId"))?;
    match payload.get("action")?.as_str()? {
        "create" => {
            if !only(&["channelId", "displayName", "systemPrompt"]) {
                return None;
            }
            text(request.get("displayName"))?;
            text(request.get("systemPrompt"))?;
            Some(ManagementRequest::Create {
                request_id,
                channel_id,
            })
        }
        "update" => {
            if !only(&[
                "channelId",
                "agentName",
                "displayName",
                "systemPrompt",
                "runtime",
                "provider",
                "model",
            ]) {
                return None;
            }
            let update = ManagementRequest::Update {
                request_id,
                channel_id,
                agent_name: text(request.get("agentName"))?,
                display_name: text(request.get("displayName")),
                system_prompt: text(request.get("systemPrompt")),
                runtime: text(request.get("runtime")),
                provider: text(request.get("provider")),
                model: text(request.get("model")),
            };
            match &update {
                ManagementRequest::Update {
                    display_name: None,
                    system_prompt: None,
                    runtime: None,
                    provider: None,
                    model: None,
                    ..
                } => None,
                _ => Some(update),
            }
        }
        _ => None,
    }
}

/// `requestedDraft`: the agent's settings with the requested changes.
pub fn requested_draft(
    agent: &Agent,
    request: &ManagementRequest,
    options: &[HarnessOption],
) -> Draft {
    let current = draft::from_agent(agent);
    let ManagementRequest::Update {
        display_name,
        system_prompt,
        runtime,
        provider,
        model,
        ..
    } = request
    else {
        return current;
    };
    let option = runtime.as_ref().and_then(|runtime| {
        options.iter().find(|option| {
            let executable = option
                .command
                .replace('\\', "/")
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            option.available
                && (option.command == *runtime
                    || executable == *runtime
                    || draft::kind(&option.command).as_deref() == Some(runtime.as_str()))
        })
    });
    Draft {
        name: display_name.clone().unwrap_or(current.name.clone()),
        system_prompt: system_prompt
            .clone()
            .unwrap_or(current.system_prompt.clone()),
        command: option
            .map(|option| option.command.clone())
            .or(runtime.clone())
            .unwrap_or(current.command.clone()),
        args: option
            .map(|option| serde_json::to_string(&option.default_args).unwrap_or_default())
            .unwrap_or(current.args.clone()),
        provider: provider.clone().unwrap_or(current.provider.clone()),
        model: model.clone().unwrap_or(current.model.clone()),
        ..current
    }
}

struct Pending {
    community: WeakEntity<Community>,
    agent: String,
    request: ManagementRequest,
}

/// The requests waiting for review, oldest first.
#[derive(Default)]
pub struct Reviews {
    queue: VecDeque<Pending>,
    seen: VecDeque<String>,
    /// The editor showing the head request.
    editor: Option<(String, Entity<EditDialog>, Subscription)>,
}

impl Global for Reviews {}

type Frame = (buzz_relay_client::RelayOrigin, String, String);

static FRAMES: std::sync::OnceLock<futures::channel::mpsc::UnboundedSender<Frame>> =
    std::sync::OnceLock::new();

/// Delivers decrypted activity frames to [`frame`] on the UI thread.
fn init(cx: &mut App) {
    let (sender, mut frames) = futures::channel::mpsc::unbounded::<Frame>();
    if FRAMES.set(sender).is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some((origin, agent, plaintext)) = frames.next().await {
            cx.update(|cx| {
                let community = cx
                    .global::<AppState>()
                    .communities
                    .iter()
                    .find(|community| community.read(cx).origin == origin)
                    .cloned();
                if let Some(community) = community {
                    frame(&community, &agent, &plaintext, cx);
                }
            });
        }
    })
    .detach();
}

/// An owner-addressed activity envelope arrived for the viewer in the
/// community at `origin`. Only management requests are kept.
pub fn observed(
    identity: &buzz_identity::Identity,
    event: &nostr::prelude::Event,
    origin: &buzz_relay_client::RelayOrigin,
) {
    if event.kind.as_u16() != 24200 {
        return;
    }
    let Ok(plaintext) = identity.decrypt_agent_frame(event) else {
        return;
    };
    if parse(&plaintext).is_some()
        && let Some(sender) = FRAMES.get()
    {
        let _ = sender.unbounded_send((origin.clone(), event.pubkey.to_hex(), plaintext));
    }
}

/// An activity frame the viewer received from `agent` in `community`.
pub fn frame(community: &Entity<Community>, agent: &str, plaintext: &str, cx: &mut App) {
    let Some(request) = parse(plaintext) else {
        return;
    };
    let reviews = cx.default_global::<Reviews>();
    if reviews.seen.iter().any(|id| id == request.request_id()) {
        return;
    }
    reviews.seen.push_back(request.request_id().to_string());
    if reviews.seen.len() > QUEUE_LIMIT {
        reviews.seen.pop_front();
    }
    reviews.queue.push_back(Pending {
        community: community.downgrade(),
        agent: agent.to_string(),
        request,
    });
    // Keep the request under review; drop the oldest of the rest.
    if reviews.queue.len() > QUEUE_LIMIT {
        reviews.queue.remove(1);
    }
    cx.refresh_windows();
}

fn dismiss(cx: &mut App) {
    let reviews = cx.default_global::<Reviews>();
    reviews.queue.pop_front();
    reviews.editor = None;
    cx.refresh_windows();
}

/// The agent's personal setups that match the request.
fn matches(
    agent: &str,
    name: &str,
    origin: &str,
    cx: &App,
) -> Vec<(Entity<HostConnection>, Agent)> {
    let target = name.trim().to_lowercase();
    cx.global::<Hosts>()
        .hosts
        .iter()
        .flat_map(|host| {
            host.read(cx)
                .agents
                .agents
                .iter()
                .filter(|setup| {
                    setup.configured
                        && setup.pubkey == agent
                        && setup.name.trim().to_lowercase() == target
                        && super::inventory::origin(&setup.relay_url) == origin
                })
                .map(|setup| (host.clone(), setup.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn toast(title: &'static str, description: String, dismissable: bool) -> Toast {
    let toast = Toast::new("agent-review", title)
        .description(description)
        .tone(ToastTone::Warning);
    if dismissable {
        toast.on_dismiss(|_, _, cx| dismiss(cx))
    } else {
        toast
    }
}

/// The head request: a toast or the pre-filled editor.
pub fn render(window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    if FRAMES.get().is_none() {
        init(cx);
    }
    let reviews = cx.try_global::<Reviews>()?;
    let pending = reviews.queue.front()?;
    let Some(community) = pending.community.upgrade() else {
        dismiss(cx);
        return None;
    };
    let request = pending.request.clone();
    let agent = pending.agent.clone();
    let origin = community.read(cx).origin.http();
    let snapshot = community.read(cx).snapshot.clone();
    // Membership: the requester must be in the channel it names.
    let channel = snapshot
        .channels
        .iter()
        .find(|channel| channel.id == request.channel_id());
    let membership = match channel {
        Some(channel) => Some(channel.members.contains(&agent)),
        None if snapshot.channels.is_empty() => None,
        None => Some(false),
    };
    if membership == Some(false) {
        dismiss(cx);
        return None;
    }
    let right = window.viewport_size().width - px(16.) - px(384.);
    let place = |element: AnyElement| {
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(point(right, px(72.)))
                .child(element),
        )
        .with_priority(3)
        .into_any_element()
    };
    if membership.is_none() {
        let retry_community = community.clone();
        return Some(place(
            toast(
                "Could not verify request membership",
                "Retry the channel roster read before reviewing this request.".into(),
                true,
            )
            .action(
                Button::new("review-retry", "Retry")
                    .size(ButtonSize::Sm)
                    .on_click(move |_, _, cx| {
                        retry_community.read(cx).session.retry();
                    }),
            )
            .into_any_element(),
        ));
    }
    match &request {
        ManagementRequest::Create { .. } => Some(place(
            toast(
                "Agent creation needs attention",
                "Agent-requested creation is not supported yet. Create the agent yourself from \
                 Agents."
                    .into(),
                true,
            )
            .into_any_element(),
        )),
        ManagementRequest::Update { agent_name, .. } => {
            let hosts = cx
                .try_global::<crate::hosts::Hosts>()
                .map(|hosts| hosts.hosts.clone())
                .unwrap_or_default();
            if hosts
                .iter()
                .any(|host| host.read(cx).status() == crate::hosts::control::Status::Error)
            {
                return Some(place(
                    toast(
                        "Could not load personal agents",
                        "Refresh local agents before reviewing this request.".into(),
                        false,
                    )
                    .action(
                        Button::new("review-refresh", "Retry")
                            .size(ButtonSize::Sm)
                            .on_click(move |_, _, cx| {
                                for host in &hosts {
                                    host.update(cx, |host, cx| host.refresh(cx).detach());
                                }
                            }),
                    )
                    .into_any_element(),
                ));
            }
            let found = matches(&agent, agent_name, &origin, cx);
            let [(host, setup)] = found.as_slice() else {
                return Some(place(
                    toast(
                        "Agent update needs attention",
                        if found.len() > 1 {
                            "More than one personal agent has that name. Rename one, then ask \
                             again."
                                .into()
                        } else {
                            format!("No personal agent named {agent_name} was found.")
                        },
                        true,
                    )
                    .into_any_element(),
                ));
            };
            let key = request.request_id().to_string();
            let reviews = cx.global::<Reviews>();
            let editor = match &reviews.editor {
                Some((id, editor, _)) if *id == key => editor.clone(),
                _ => {
                    let options = host.read(cx).harness_options().unwrap_or_default();
                    let initial = requested_draft(setup, &request, &options);
                    let (host, setup) = (host.clone(), setup.clone());
                    let name = setup.name.clone();
                    let editor = cx.new(|cx| {
                        EditDialog::new(
                            host,
                            setup,
                            name,
                            Some(initial),
                            Some("Requested by an agent. Review every field before saving.".into()),
                            window,
                            cx,
                        )
                    });
                    let subscription = cx.subscribe(&editor, |_, _: &Dismissed, cx| dismiss(cx));
                    cx.default_global::<Reviews>().editor =
                        Some((key, editor.clone(), subscription));
                    editor
                }
            };
            Some(crate::ui::overlay::modal(window, editor, None).into_any_element())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Agent, HarnessOption, ManagementRequest, parse, requested_draft};

    #[test]
    fn only_the_narrow_contract_parses() {
        let frame = |request: &str| {
            format!(
                r#"{{"kind":"agent_management_request","payload":{{"type":"agent_management_request","action":"update","requestId":"r1","request":{request}}}}}"#
            )
        };
        let update = frame(r#"{"channelId":"c","agentName":"Scout","model":"gpt-5"}"#);
        assert!(matches!(
            parse(&update),
            Some(ManagementRequest::Update { model: Some(_), .. })
        ));
        // No change, or an unknown field, is no request.
        let empty = frame(r#"{"channelId":"c","agentName":"Scout"}"#);
        assert_eq!(parse(&empty), None);
        let secret = frame(r#"{"channelId":"c","agentName":"Scout","apiKey":"x"}"#);
        assert_eq!(parse(&secret), None);
        assert_eq!(parse(r#"{"kind":"batch"}"#), None);
    }

    #[test]
    fn a_requested_runtime_takes_its_default_arguments() {
        let agent = Agent {
            name: "Scout".into(),
            harness: buzz_host::api::HarnessSettings {
                command: "goose".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let request = ManagementRequest::Update {
            request_id: "r".into(),
            channel_id: "c".into(),
            agent_name: "Scout".into(),
            display_name: Some("Scout 2".into()),
            system_prompt: None,
            runtime: Some("hermes".into()),
            provider: None,
            model: None,
        };
        let options = vec![HarnessOption {
            command: "/opt/hermes-acp".into(),
            available: true,
            default_args: vec!["--acp".into()],
            ..Default::default()
        }];
        let draft = requested_draft(&agent, &request, &options);
        assert_eq!(draft.name, "Scout 2");
        assert_eq!(draft.command, "/opt/hermes-acp");
        assert_eq!(draft.args, r#"["--acp"]"#);
    }
}
