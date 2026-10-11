//! The profile panel's agent sections (`ProfileAgentRuntime.tsx`,
//! `ProfileAgentActions.tsx`, `ProfileInstances.tsx`,
//! `ProfileAgentIdentity.tsx`, `ProfilePublicMetadata.tsx`,
//! `ProfileAgentArchive.tsx`, `ProfileAgentDelete.tsx`,
//! `ProfileHarnessLog.tsx`, `InstanceProfilePanel.tsx`).
//!
//! Profile metadata never grants control: the local sections show only an
//! agent a host of the viewer's runs in this community, and owner-only
//! actions follow the verified NIP-OA owner. The relay and the host check
//! every request again.

use super::{ProfilePanel, Section};
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::ui::agents::relay::{self as agent_relay, Consent};
use crate::ui::agents::{EditDialog, EditorDismissed, OpenHarnesses};
use buzz_host::api::{Agent, AgentAction, ProcessStatus, Request, Response, RestartChange};
use buzz_relay_client::members::{Change, MemberChange};
use buzz_ui::{
    ActiveTheme, AlertDialog, Button, ButtonSize, ButtonVariant, Icon, IconName, Surface,
    TypeStyled,
    tokens::{radius, space},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// How often an open harness log is read again.
const LOG_POLL: Duration = Duration::from_secs(30);

/// `runtimeLabels`.
fn runtime_label(agent_type: &str) -> &str {
    match agent_type {
        "goose" => "Goose",
        "claude-code" => "Claude Code",
        "codex-acp" => "Codex",
        "aider" => "Aider",
        other => other,
    }
}

/// Public display metadata (`PublicAgentMetadata`): never ownership or
/// permission evidence.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PublicMetadata {
    agent_type: String,
    capabilities: Vec<String>,
}

#[derive(Default)]
pub(super) enum MetadataState {
    #[default]
    Loading,
    Ready(Option<PublicMetadata>),
    Failed,
}

/// The agent sections' own state.
#[derive(Default)]
pub(super) struct AgentState {
    /// The exact instance this panel shows (`buzz:agent-instance:`).
    pub instance: Option<String>,
    log: Option<Entity<HarnessLog>>,
    editor: Option<(Entity<EditDialog>, Vec<Subscription>)>,
    diagnostics_open: bool,
    instances_open: bool,
    metadata: MetadataState,
    metadata_task: Option<Task<()>>,
    /// `None` while the archive consent check runs.
    consent: Option<Result<Option<Consent>, ()>>,
    consent_task: Option<Task<()>>,
    archive_pending: Option<bool>,
    archive_result: Option<(String, bool)>,
    archive_confirm: bool,
    delete_confirm: bool,
    deleting: bool,
    delete_error: Option<String>,
}

/// An instance link (`instanceTarget`): one local record of an agent key,
/// for the viewer in one community.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstanceTarget {
    pub community_origin: String,
    pub id: String,
    pub pubkey: String,
    pub viewer: String,
}

const INSTANCE_PREFIX: &str = "buzz:agent-instance:";

/// `instanceTarget`. This app opens instances in place, so only tests
/// build these links; the panel still opens them.
#[cfg(test)]
pub fn instance_target(target: &InstanceTarget) -> String {
    let json = serde_json::to_string(target).expect("targets serialize");
    format!(
        "{INSTANCE_PREFIX}{}",
        buzz_relay_client::kit::encode_component(&json)
    )
}

/// `parseInstanceTarget`.
pub fn parse_instance_target(target: &str) -> Option<InstanceTarget> {
    if target.len() > 4096 {
        return None;
    }
    let encoded = target.strip_prefix(INSTANCE_PREFIX)?;
    let json = decode_component(encoded)?;
    let value: InstanceTarget = serde_json::from_str(&json).ok()?;
    let hex =
        |key: &str| key.len() == 64 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let origin = buzz_relay_client::RelayOrigin::parse(&value.community_origin).ok()?;
    (!value.id.trim().is_empty()
        && value.id.len() <= 256
        && hex(&value.pubkey)
        && hex(&value.viewer)
        && origin.http() == value.community_origin)
        .then_some(value)
}

/// `decodeURIComponent`: `None` for a malformed escape or non-UTF-8.
fn decode_component(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn identity(cx: &App) -> Option<Arc<buzz_identity::Identity>> {
    match &cx.global::<AppState>().identity {
        IdentityState::Ready(identity) => Some(identity.clone()),
        _ => None,
    }
}

/// `agentProcessLabel`, with "Last known: " while status is unconfirmed.
fn process_line(host: &Entity<HostConnection>, agent: &Agent, cx: &App) -> String {
    let error = host.read(cx).status() == crate::hosts::control::Status::Error;
    format!(
        "{}{}",
        if error { "Last known: " } else { "" },
        crate::ui::agents::process_label(agent.status)
    )
}

/// `Change`: what a restart would apply.
pub(super) fn diff_change(change: &RestartChange) -> (Option<String>, String) {
    let value = |value: &Value| match value {
        Value::Null => "null".to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mask = |value: &Option<String>| value.clone().unwrap_or_else(|| "••••".into());
    match change {
        RestartChange::Value { before, after } => (Some(value(before)), value(after)),
        RestartChange::Masked { before, after } => (Some(mask(before)), mask(after)),
        RestartChange::Text {
            before_chars,
            after_chars,
        } => (
            None,
            format!(
                "{} chars → {} chars",
                before_chars.unwrap_or(0),
                after_chars.unwrap_or(0)
            ),
        ),
        RestartChange::Added => (None, "added".into()),
        RestartChange::Removed => (None, "removed".into()),
    }
}

/// `publicAgentMetadata` over the agent's newest 10100 and its verified
/// owner's 30177 for it.
fn public_metadata(
    events: &[nostr::prelude::Event],
    pubkey: &str,
    owner: Option<&str>,
) -> Option<PublicMetadata> {
    let newest = |kind: u16, author: &str, d: Option<&str>| {
        events
            .iter()
            .filter(|event| event.kind.as_u16() == kind && event.pubkey.to_hex() == author)
            .filter(|event| {
                d.is_none_or(|d| {
                    event.tags.iter().any(|tag| {
                        let tag = tag.as_slice();
                        tag.first().map(String::as_str) == Some("d")
                            && tag.get(1).map(String::as_str) == Some(d)
                    })
                })
            })
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| b.id.to_hex().cmp(&a.id.to_hex()))
            })
    };
    let object = |content: &str| {
        serde_json::from_str::<Value>(content)
            .ok()
            .filter(Value::is_object)
    };
    let strings = |value: &Value| {
        value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    };
    let respond_to = |value: &Value| {
        value
            .as_str()
            .is_some_and(|value| ["owner-only", "allowlist", "anyone"].contains(&value))
    };
    if let Some(owner) = owner
        && let Some(policy) = newest(30177, owner, Some(pubkey))
    {
        // A malformed winning owner policy reserves the coordinate.
        let body = object(&policy.content)?;
        let parallelism = body["parallelism"]
            .as_u64()
            .is_some_and(|n| n <= u32::MAX as u64);
        let texts = [
            "persona_id",
            "system_prompt",
            "model",
            "provider",
            "persona_source_version",
        ]
        .iter()
        .all(|key| body[*key].is_null() || body[*key].is_string());
        let allowlist = body.get("respond_to_allowlist").is_none_or(&strings);
        return (body["name"].is_string()
            && parallelism
            && respond_to(&body["respond_to"])
            && texts
            && allowlist)
            .then(|| PublicMetadata {
                agent_type: "agent".into(),
                capabilities: Vec::new(),
            });
    }
    let latest = newest(10100, pubkey, None)?;
    let body = object(&latest.content).unwrap_or_else(|| json!({}));
    for key in ["capabilities", "channels", "channel_ids"] {
        if body[key].is_array() && !strings(&body[key]) {
            return None;
        }
    }
    if !body["owner_pubkey"].is_null() && !body["owner_pubkey"].is_string() {
        return None;
    }
    if !body["respond_to"].is_null() && !respond_to(&body["respond_to"]) {
        return None;
    }
    if body
        .get("respond_to_allowlist")
        .is_some_and(|value| !strings(value))
    {
        return None;
    }
    Some(PublicMetadata {
        agent_type: body["agent_type"].as_str().unwrap_or("agent").to_string(),
        capabilities: body["capabilities"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

impl ProfilePanel {
    /// The one local record this panel shows, if exactly one matches
    /// (`exactProfileAgent`).
    pub(super) fn exact_agent(&self, cx: &App) -> Option<(Entity<HostConnection>, Agent)> {
        let mut matches = self.runtime_agents(cx).into_iter().filter(|(_, agent)| {
            self.agent
                .instance
                .as_ref()
                .is_none_or(|id| *id == agent.id)
        });
        match (matches.next(), matches.next()) {
            (Some(found), None) => Some(found),
            _ => None,
        }
    }

    /// Reads the public metadata and the archive consent once the profile
    /// (and its verified owner) is known.
    pub(super) fn load_agent(&mut self, cx: &mut Context<Self>) {
        self.load_metadata(cx);
        self.check_consent(cx);
    }

    fn load_metadata(&mut self, cx: &mut Context<Self>) {
        let Some(identity) = identity(cx) else {
            return;
        };
        let origin = self.community.read(cx).origin.clone();
        let (pubkey, owner) = (self.pubkey.clone(), self.verified_owner(cx));
        let http = agent_relay::lane(&origin, identity);
        let mut filters = vec![json!({"kinds": [10100], "authors": [pubkey], "limit": 1})];
        if let Some(owner) = &owner {
            filters.push(json!({"kinds": [30177], "authors": [owner], "#d": [pubkey], "limit": 1}));
        }
        let filters: Vec<buzz_relay_client::socket::Filter> = filters
            .into_iter()
            .filter_map(|filter| match filter {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .collect();
        self.agent.metadata = MetadataState::Loading;
        let read = crate::runtime::Runtime::handle(cx)
            .spawn(async move { http.query(&filters).await.map_err(|e| e.to_string()) });
        self.agent.metadata_task = Some(cx.spawn(async move |this, cx| {
            let events = read.await.unwrap_or_else(|_| Err("cancelled".into()));
            let _ = this.update(cx, |this, cx| {
                this.agent.metadata = match events {
                    Ok(events) => MetadataState::Ready(public_metadata(
                        &events,
                        &this.pubkey,
                        owner.as_deref(),
                    )),
                    Err(_) => MetadataState::Failed,
                };
                cx.notify();
            });
        }));
    }

    pub(super) fn metadata_failed(&self) -> bool {
        matches!(self.agent.metadata, MetadataState::Failed)
    }

    pub(super) fn retry_metadata(&mut self, cx: &mut Context<Self>) {
        self.load_metadata(cx);
    }

    /// `archives.consent`: whether, and how, the viewer may archive this
    /// identity. A render guard only; the request checks again.
    fn check_consent(&mut self, cx: &mut Context<Self>) {
        let Some(identity) = identity(cx) else {
            return;
        };
        let community = self.community.read(cx);
        let Some(authority) = community
            .snapshot
            .relay
            .as_ref()
            .map(|relay| relay.authority.clone())
        else {
            return;
        };
        let http = agent_relay::lane(&community.origin, identity.clone());
        let (viewer, target) = (identity.pubkey_hex(), self.pubkey.clone());
        self.agent.consent = None;
        let check = crate::runtime::Runtime::handle(cx)
            .spawn(async move { agent_relay::consent(&http, &authority, &viewer, &target).await });
        self.agent.consent_task = Some(cx.spawn(async move |this, cx| {
            let consent = check.await.unwrap_or_else(|_| Err("cancelled".into()));
            let _ = this.update(cx, |this, cx| {
                this.agent.consent = Some(consent.map_err(drop));
                cx.notify();
            });
        }));
    }

    fn consent(&self) -> Option<&Consent> {
        self.agent.consent.as_ref()?.as_ref().ok()?.as_ref()
    }

    /// Consent through the verified owner's attestation.
    fn owner_consent(&self) -> bool {
        self.consent().is_some_and(|consent| consent.auth.is_some())
    }

    fn archived(&self, cx: &App) -> Option<bool> {
        match crate::ui::archives::status(&self.community, cx)? {
            crate::ui::archives::Status::Ready => Some(crate::ui::archives::hides(
                &self.community,
                &self.pubkey,
                cx,
            )),
            _ => None,
        }
    }

    /// Archive or unarchive the identity (`archives.request`). Resolves
    /// whether the relay confirmed it.
    fn archive(&mut self, archive: bool, cx: &mut Context<Self>) -> Task<bool> {
        let Some(identity) = identity(cx) else {
            return Task::ready(false);
        };
        let community = self.community.read(cx);
        let Some(authority) = community
            .snapshot
            .relay
            .as_ref()
            .map(|relay| relay.authority.clone())
        else {
            return Task::ready(false);
        };
        let http = agent_relay::lane(&community.origin, identity.clone());
        let target = self.pubkey.clone();
        self.agent.archive_pending = Some(archive);
        self.agent.archive_result = None;
        cx.notify();
        let request = crate::runtime::Runtime::handle(cx).spawn(async move {
            agent_relay::request_archive(&http, &identity, &authority, &target, archive, None).await
        });
        cx.spawn(async move |this, cx| {
            let result = request.await.unwrap_or_else(|_| Err("cancelled".into()));
            this.update(cx, |this, cx| {
                this.agent.archive_pending = None;
                let ok = result.is_ok();
                this.agent.archive_result = Some(match result {
                    Ok(_) => (
                        if archive {
                            "Archived on this relay".into()
                        } else {
                            "Unarchived on this relay".into()
                        },
                        false,
                    ),
                    Err(error) => (
                        format!(
                            "{} failed: {error}",
                            if archive { "Archive" } else { "Unarchive" }
                        ),
                        true,
                    ),
                });
                crate::ui::archives::ensure(&this.community, true, cx);
                cx.notify();
                ok
            })
            .unwrap_or(false)
        })
    }

    /// `ProfileAgentDelete`: every relay effect first (channels, then the
    /// archive), the local record and key last, so any earlier failure
    /// leaves Delete retryable.
    fn delete(&mut self, cx: &mut Context<Self>) {
        let Some((host, agent)) = self.exact_agent(cx).filter(|(_, a)| !a.deployed_remote) else {
            return;
        };
        if self.agent.deleting {
            return;
        }
        self.agent.deleting = true;
        self.agent.delete_error = None;
        let session = self.community.read(cx).session.clone();
        let pubkey = self.pubkey.clone();
        let listed: Vec<String> = self
            .community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .filter(|channel| channel.members.contains(&pubkey))
            .map(|channel| channel.id.clone())
            .collect();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let removed = async {
                let mut channels = session.member_channels(&pubkey).await?;
                for id in listed {
                    if !channels.contains(&id) {
                        channels.push(id);
                    }
                }
                for channel in channels {
                    let state = session.channel_state(&channel).await?;
                    let Some(role) = state.roles.get(&pubkey).copied() else {
                        continue;
                    };
                    let change = MemberChange {
                        pubkey: pubkey.clone(),
                        expected: role,
                        change: Change::Remove,
                    };
                    session.change_member(&channel, change).await?;
                }
                Ok::<(), String>(())
            }
            .await;
            if let Err(error) = removed {
                let _ = this.update(cx, |this, cx| {
                    this.agent.deleting = false;
                    this.agent.delete_error = Some(error);
                    cx.notify();
                });
                return;
            }
            let archived = this.update(cx, |this, cx| {
                if this.archived(cx) == Some(true) {
                    Task::ready(true)
                } else {
                    this.archive(true, cx)
                }
            });
            let Ok(archived) = archived else {
                return;
            };
            if !archived.await {
                let _ = this.update(cx, |this, cx| {
                    this.agent.deleting = false;
                    cx.notify();
                });
                return;
            }
            let request = Request::Delete {
                id: agent.id.clone(),
                revision: agent.revision,
            };
            let deleted = cx.update(|cx| {
                host.update(cx, |host, cx| {
                    host.write(request, crate::hosts::control::Lane::default(), cx)
                })
            });
            let deleted = deleted.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.agent.deleting = false;
                match deleted {
                    Ok(_) => {
                        if let Some(close) = this.close.clone() {
                            close(window, cx);
                        }
                    }
                    Err(failure) => {
                        this.agent.delete_error = Some(if failure.message.is_empty() {
                            "Failed to delete agent.".into()
                        } else {
                            failure.message
                        })
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Start, Stop or Restart (`ProfileAgentActions`).
    fn act(&mut self, action: AgentAction, cx: &mut Context<Self>) {
        let Some((host, agent)) = self.exact_agent(cx) else {
            return;
        };
        let viewer = self.community.read(cx).session.viewer().to_string();
        let blocked = match action {
            AgentAction::Stop => !host.read(cx).can_stop(&agent.id),
            _ => host.read(cx).launch_block(&agent).is_some(),
        };
        if blocked {
            return;
        }
        let task = host.update(cx, |host, cx| {
            host.act(&agent.id, action, Some(viewer), None, cx)
        });
        cx.spawn(async move |_, _| {
            let _ = task.await;
        })
        .detach();
    }

    /// Opens the agent editor over the panel (`AgentEditor`).
    fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((host, agent)) = self.exact_agent(cx) else {
            return;
        };
        let name = agent.name.clone();
        let editor = cx.new(|cx| EditDialog::new(host, agent, name, None, None, window, cx));
        let subscriptions = vec![
            cx.subscribe(&editor, |this, _, _: &EditorDismissed, cx| {
                this.agent.editor = None;
                cx.notify();
            }),
            cx.subscribe_in(&editor, window, |this, _, _: &OpenHarnesses, window, cx| {
                this.agent.editor = None;
                window.dispatch_action(Box::new(crate::ui::settings::OpenHostSettings), cx);
                cx.notify();
            }),
        ];
        self.agent.editor = Some((editor, subscriptions));
        cx.notify();
    }

    /// Opens the harness log in place (`ProfileHarnessLog`).
    fn open_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((host, agent)) = self.exact_agent(cx) else {
            return;
        };
        let community = self.community.read(cx).origin.http();
        let name = self.name(cx);
        let log = cx.new(|cx| HarnessLog::new(host, agent, community, name, window, cx));
        cx.subscribe(&log, |this, _, _: &LogClosed, cx| {
            this.agent.log = None;
            cx.notify();
        })
        .detach();
        self.agent.log = Some(log);
        cx.notify();
    }

    /// The log replaces the panel's contents while it is open.
    pub(super) fn log_view(&self) -> Option<AnyElement> {
        self.agent.log.clone().map(|log| log.into_any_element())
    }

    pub(super) fn editor_view(&self, window: &mut Window) -> Option<AnyElement> {
        let (editor, _) = self.agent.editor.as_ref()?;
        Some(crate::ui::overlay::modal(window, editor.clone(), None).into_any_element())
    }

    /// "Back to profile" for an instance, and its unavailable state
    /// (`InstanceDetails`): no private content before the exact record and
    /// a verified owner match.
    pub(super) fn instance_header(&self, cx: &mut Context<Self>) -> Option<(AnyElement, bool)> {
        self.agent.instance.as_ref()?;
        let back = Button::new("profile-instance-back", "Back to profile")
            .size(ButtonSize::Sm)
            .variant(ButtonVariant::Ghost)
            .on_click(cx.listener(|this, _, _, cx| {
                this.agent.instance = None;
                cx.notify();
            }));
        let valid = self.is_owner(cx) && self.exact_agent(cx).is_some();
        let mut header = div()
            .flex()
            .flex_col()
            .items_start()
            .gap(space::S2)
            .child(back);
        if !valid {
            let loading = !self.settled
                || self.runtime_agents(cx).first().is_some_and(|(host, _)| {
                    host.read(cx).status() == crate::hosts::control::Status::Loading
                });
            header = header.child(div().child(if loading { "Loading…" } else { "Unavailable" }));
            if !loading {
                header = header.child(
                    Button::new("profile-instance-retry", "Retry")
                        .size(ButtonSize::Sm)
                        .on_click(cx.listener(|this, _, _, cx| {
                            for (host, _) in this.runtime_agents(cx) {
                                host.update(cx, |host, cx| host.refresh(cx).detach());
                            }
                            this.load(cx);
                        })),
                );
            }
        }
        Some((header.into_any_element(), valid))
    }

    /// The Info tab's agent sections before the activity preview: the
    /// local agent, its actions, archive and delete.
    pub(super) fn agent_runtime_items(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let _ = window;
        let c = *cx.colors();
        let mut items = Vec::new();
        if let Some((host, agent)) = self.exact_agent(cx) {
            let state = host.read(cx);
            let ready = state.status() == crate::hosts::control::Status::Ready;
            let busy = state.control.busy;
            let unique = self
                .runtime_agents(cx)
                .iter()
                .filter(|(_, other)| other.pubkey == agent.pubkey)
                .count()
                == 1;
            let drift = agent
                .running_revision
                .is_some_and(|running| running != agent.revision);
            let facts: Vec<(&str, String)> = [
                ("Harness", agent.harness.command.clone()),
                ("Provider", agent.harness.provider.clone()),
                ("Model", agent.harness.model.clone()),
                ("Workspace", agent.workspace.clone()),
            ]
            .into_iter()
            .filter(|(_, value)| !value.is_empty())
            .collect();
            let mut section = div()
                .flex()
                .flex_col()
                .gap(space::S3)
                .child(div().text_body().child("Local agent"))
                .child(div().child(process_line(&host, &agent, cx)));
            if let Some(running) = agent.running_revision.filter(|_| drift) {
                section = section.child(div().child(format!(
                    "Saved revision {} is not running yet (running revision {running}). \
                     Restart from Agents to apply.",
                    agent.revision
                )));
            }
            if !facts.is_empty() {
                section = section
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child("Saved settings; environment overrides may apply."),
                    )
                    .child(rows(facts, cx));
            }
            if self.is_owner(cx) && unique && ready {
                section = section.child(
                    div().child(
                        Button::new("profile-agent-instructions", "Agent instructions")
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Subtle)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.edit(window, cx))),
                    ),
                );
            }
            if !agent.diagnostics.is_empty() {
                let open = self.agent.diagnostics_open;
                section = section.child(disclosure(
                    "profile-diagnostics",
                    "Host diagnostics".into(),
                    open,
                    cx.listener(|this, _, _, cx| {
                        this.agent.diagnostics_open = !this.agent.diagnostics_open;
                        cx.notify();
                    }),
                    div().text_mono().child(agent.diagnostics.join("\n")),
                    cx,
                ));
            }
            items.push(section.into_any_element());
            items.push(self.actions(&host, &agent, cx));
        }
        items.extend(self.archive_items(cx));
        items
    }

    /// Start, Stop, Restart and their notes (`ProfileAgentActions`).
    fn actions(
        &self,
        host: &Entity<HostConnection>,
        agent: &Agent,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let state = host.read(cx);
        let block = state.launch_block(agent);
        let can_stop = state.can_stop(&agent.id);
        let error_status = state.status() == crate::hosts::control::Status::Error;
        let (busy, error) = (state.control.busy, state.control.error.clone());
        let button =
            |id: &'static str, label: &'static str, disabled: bool, action: AgentAction| {
                Button::new(id, label)
                    .size(ButtonSize::Sm)
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| this.act(action, cx)))
            };
        let mut buttons = div().flex().flex_wrap().gap(space::S2);
        if matches!(agent.status, ProcessStatus::Stopped | ProcessStatus::Failed) {
            buttons = buttons.child(button(
                "profile-agent-start",
                if agent.status == ProcessStatus::Failed {
                    "Retry start"
                } else {
                    "Start"
                },
                block.is_some(),
                AgentAction::Start,
            ));
        }
        buttons = buttons
            .child(button(
                "profile-agent-stop",
                "Stop",
                !can_stop,
                AgentAction::Stop,
            ))
            .child(button(
                "profile-agent-restart",
                "Restart",
                block.is_some(),
                AgentAction::Restart,
            ));
        let host = host.clone();
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(space::S2)
            .child(buttons)
            .children(
                block.map(|block| div().text_body_sm().text_color(c.text_subtle).child(block)),
            )
            .children(
                agent
                    .error
                    .clone()
                    .map(|error| div().text_color(c.text_danger).child(error)),
            )
            .children(error.map(|error| div().text_color(c.text_danger).child(error)))
            .when(error_status, |this| {
                this.child(div().text_body_sm().text_color(c.text_subtle).child(
                    "Showing the last host snapshot. Current process state and durable \
                             enabled intent are unconfirmed.",
                ))
                .child(
                    Button::new("profile-agent-retry-status", "Retry status")
                        .size(ButtonSize::Sm)
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            host.update(cx, |host, cx| host.refresh_status(cx).detach())
                        }),
                )
            })
            .when(busy, |this| {
                this.child(div().child("Waiting for the host to confirm…"))
            })
            .into_any_element()
    }

    /// Visibility, Archive or Unarchive, and the owner's Delete
    /// (`ProfileAgentArchive`).
    fn archive_items(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = *cx.colors();
        if !self.known_agent(cx) {
            return Vec::new();
        }
        crate::ui::archives::ensure(&self.community, false, cx);
        let mut items = Vec::new();
        let state = self.archived(cx);
        if state == Some(true) {
            items.push(
                div()
                    .id("profile-archived")
                    .flex()
                    .items_center()
                    .gap(space::S2)
                    .text_body_sm()
                    .child(div().text_color(c.text_subtle).child("Visibility"))
                    .child("Archived")
                    .child(
                        div()
                            .id("profile-archived-info")
                            .tooltip(buzz_ui::Tooltip::text(
                                "Archived agents do not appear in search, autocomplete, or \
                                 member-add flows in this space. You can unarchive them at any \
                                 time.",
                            ))
                            .child(Icon::new(IconName::Info).size(16.)),
                    )
                    .into_any_element(),
            );
        }
        let pending = self.agent.archive_pending;
        let archived = match pending {
            Some(archive) => !archive,
            None => state == Some(true),
        };
        if self.consent().is_some() && (pending.is_some() || state.is_some()) {
            let label = match (pending, archived) {
                (Some(_), true) => "Unarchiving…",
                (Some(_), false) => "Archiving…",
                (None, true) => "Unarchive agent",
                (None, false) => "Archive agent",
            };
            let busy = self.agent.deleting
                || self
                    .exact_agent(cx)
                    .is_some_and(|(host, _)| host.read(cx).control.busy);
            items.push(
                div()
                    .child(
                        Button::new("profile-archive", label)
                            .size(ButtonSize::Sm)
                            .loading(pending.is_some())
                            .disabled(busy && pending.is_none())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if archived {
                                    this.archive(false, cx).detach();
                                } else {
                                    this.agent.archive_confirm = true;
                                    cx.notify();
                                }
                            })),
                    )
                    .into_any_element(),
            );
        }
        let deletable = self.owner_consent()
            && self
                .exact_agent(cx)
                .is_some_and(|(_, agent)| !agent.deployed_remote);
        if deletable {
            items.push(
                div()
                    .child(
                        Button::new("profile-delete", "Delete agent")
                            .size(ButtonSize::Sm)
                            .destructive()
                            .loading(self.agent.deleting)
                            .disabled(!self.agent.deleting && pending.is_some())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.agent.delete_confirm = true;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        if let Some(error) = &self.agent.delete_error {
            items.push(
                div()
                    .text_body_sm()
                    .text_color(c.text_danger)
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        if let Some((text, failed)) = &self.agent.archive_result {
            items.push(
                div()
                    .text_body_sm()
                    .when(*failed, |this| this.text_color(c.text_danger))
                    .child(text.clone())
                    .into_any_element(),
            );
        }
        items
    }

    /// The archive and delete confirmations, over the panel.
    pub(super) fn agent_dialogs(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let c = *cx.colors();
        if self.agent.archive_confirm {
            let name = self.name(cx);
            let community = self.community.read(cx).name();
            let running = self
                .exact_agent(cx)
                .is_some_and(|(_, agent)| agent.status == ProcessStatus::Running);
            let dialog = AlertDialog::new(format!("Archive {name}?"))
                .description(format!(
                    "{name} will be hidden from search, autocomplete, and member-add flows in \
                     {community}. You can unarchive it at any time.{}",
                    if running {
                        " It keeps running until you stop it."
                    } else {
                        ""
                    }
                ))
                .action(
                    Button::new("profile-archive-cancel", "Cancel")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.agent.archive_confirm = false;
                            cx.notify();
                        })),
                )
                .action(
                    Button::new("profile-archive-confirm", "Archive agent")
                        .prominent()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.agent.archive_confirm = false;
                            this.archive(true, cx).detach();
                        })),
                );
            return Some(crate::ui::overlay::modal(window, dialog, None).into_any_element());
        }
        if self.agent.delete_confirm {
            let bullet = |text: &'static str| {
                div()
                    .flex()
                    .gap(space::S2)
                    .child("•")
                    .child(div().flex_1().child(text))
            };
            let dialog = AlertDialog::new("Delete this agent?")
                .description("Deleting this agent stops and removes the agent from this community.")
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(space::S1)
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(bullet(
                            "Removes the local management record and saved agent key",
                        ))
                        .child(bullet("Removes the agent from every channel it belongs to"))
                        .child(bullet(
                            "Archives the agent's identity on the relay so it no longer appears \
                             in member lists or mention suggestions",
                        ))
                        .child(bullet(
                            "Stops any local agent process before deleting the record",
                        )),
                )
                .child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child("Archive this agent if you want to hide it instead of removing it."),
                )
                .action(
                    Button::new("profile-delete-cancel", "Cancel")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.agent.delete_confirm = false;
                            cx.notify();
                        })),
                )
                .action(
                    Button::new("profile-delete-confirm", "Delete agent")
                        .destructive()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.agent.delete_confirm = false;
                            this.delete(cx);
                        })),
                );
            return Some(crate::ui::overlay::modal(window, dialog, None).into_any_element());
        }
        None
    }

    /// "Managed by", after the public key (`ProfileAgentIdentity`).
    pub(super) fn agent_identity_items(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut items = Vec::new();
        if self.known_agent(cx)
            && let Some(owner) = self.verified_owner(cx)
        {
            items.push(self.managed_by(owner, cx));
        }
        items
    }

    /// The agent type and capabilities (`ProfilePublicMetadata`), after
    /// NIP-05.
    pub(super) fn agent_metadata_items(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut items = Vec::new();
        if let MetadataState::Ready(Some(metadata)) = &self.agent.metadata {
            items.push(self.copy_field_shown(
                "Agent type",
                metadata.agent_type.clone(),
                runtime_label(&metadata.agent_type).to_string(),
                cx,
            ));
            let capabilities = metadata.capabilities.join(", ");
            if !capabilities.is_empty() {
                items.push(self.copy_field_shown(
                    "Capabilities",
                    capabilities.clone(),
                    capabilities,
                    cx,
                ));
            }
        }
        items
    }

    /// "Managed by" and the owner's name, which opens their profile.
    fn managed_by(&self, owner: String, cx: &mut Context<Self>) -> AnyElement {
        let viewer = self.community.read(cx).session.viewer().to_string();
        let shown = self
            .community
            .read(cx)
            .snapshot
            .profiles
            .get(&owner)
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| super::short_key(&owner));
        let name = if owner == viewer {
            format!("{shown} (you)")
        } else {
            shown
        };
        self.session(cx).want_profiles(vec![owner.clone()]);
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(space::S2)
            .child(div().text_body().child("Managed by"))
            .child(
                Button::new("profile-owner", name)
                    .size(ButtonSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .on_click(move |event, window, cx| {
                        crate::ui::navigation::show_profile(
                            owner.clone(),
                            event.position(),
                            window,
                            cx,
                        )
                    }),
            )
            .into_any_element()
    }

    /// "Instances" (`ProfileInstances`): this key's local records here.
    pub(super) fn instances(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let agents = self.runtime_agents(cx);
        let known = self.known_agent(cx);
        if !known && agents.is_empty() {
            return None;
        }
        let status = agents
            .first()
            .map(|(host, _)| host.read(cx).status())
            .or_else(|| {
                cx.try_global::<crate::hosts::Hosts>()
                    .and_then(|hosts| hosts.hosts.first().cloned())
                    .map(|host| host.read(cx).status())
            })?;
        use crate::hosts::control::Status;
        let mut section = div()
            .flex()
            .flex_col()
            .gap(space::S2)
            .child(div().text_body().child("Instances"));
        match status {
            Status::Loading => section = section.child(div().child("Loading managed agents…")),
            Status::Error if agents.is_empty() => {
                let hosts: Vec<_> = cx
                    .try_global::<crate::hosts::Hosts>()
                    .map(|hosts| hosts.hosts.clone())
                    .unwrap_or_default();
                section = section
                    .child(
                        div()
                            .text_color(c.text_danger)
                            .child("Could not refresh managed agents."),
                    )
                    .child(
                        div().child(
                            Button::new("profile-instances-retry", "Retry agents")
                                .size(ButtonSize::Sm)
                                .on_click(move |_, _, cx| {
                                    for host in &hosts {
                                        host.update(cx, |host, cx| host.refresh(cx).detach());
                                    }
                                }),
                        ),
                    );
            }
            _ if agents.is_empty() => {
                section = section.child(div().child("Not managed on this device."));
            }
            _ => {
                let count = agents.len();
                let label = format!(
                    "{count} {}",
                    if count == 1 { "instance" } else { "instances" }
                );
                let mut list = div().flex().flex_col().gap(space::S1);
                let owner = self.is_owner(cx);
                for (_, agent) in agents {
                    let archived = self.archived(cx) == Some(true);
                    let current = self.agent.instance.as_deref() == Some(agent.id.as_str());
                    let note = if archived {
                        " Archived"
                    } else if current {
                        " Current"
                    } else {
                        ""
                    };
                    let id = agent.id.clone();
                    let name: AnyElement = if owner {
                        Button::new(
                            SharedString::from(format!("profile-instance-{}", agent.id)),
                            agent.name.clone(),
                        )
                        .size(ButtonSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.agent.instance = Some(id.clone());
                            this.tab = Section::Info;
                            cx.notify();
                        }))
                        .into_any_element()
                    } else {
                        div().child(agent.name.clone()).into_any_element()
                    };
                    list = list.child(div().flex().items_center().child(name).child(note));
                }
                section = section.child(disclosure(
                    "profile-instances",
                    label.into(),
                    self.agent.instances_open,
                    cx.listener(|this, _, _, cx| {
                        this.agent.instances_open = !this.agent.instances_open;
                        cx.notify();
                    }),
                    list,
                    cx,
                ));
            }
        }
        Some(section.into_any_element())
    }

    /// The Runtime tab's "Harness log" entry.
    pub(super) fn log_entry(
        &self,
        agent: &Agent,
        ready: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = agent;
        div()
            .p(space::S4)
            .child(
                Button::new("profile-harness-log", "Harness log")
                    .size(ButtonSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .icon(IconName::FileText)
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| this.open_log(window, cx))),
            )
            .into_any_element()
    }

    /// The Runtime tab's model rows, each with Edit (`AgentEditor`).
    pub(super) fn model_settings(&self, agent: &Agent, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let blocked = self.exact_agent(cx).is_none_or(|(host, _)| {
            let host = host.read(cx);
            host.control.busy || host.status() != crate::hosts::control::Status::Ready
        });
        let model = |value: &Option<String>, env: &Option<String>| match env {
            Some(env) => format!("Set by environment ({env})"),
            None => value.clone().unwrap_or_else(|| "—".into()),
        };
        let row = |id: &'static str, label: &'static str, value: String, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_end()
                .justify_between()
                .gap(space::S3)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().text_body_sm().text_color(c.text_subtle).child(label))
                        .child(div().text_mono().child(value)),
                )
                .child(
                    Button::new(id, "Edit")
                        .size(ButtonSize::Sm)
                        .tooltip(format!("Edit {label}"))
                        .disabled(blocked)
                        .on_click(cx.listener(|this, _, window, cx| this.edit(window, cx))),
                )
        };
        div()
            .flex()
            .flex_col()
            .gap(space::S3)
            .child(div().text_body().child("Model settings"))
            .child(row(
                "profile-edit-model",
                "Model",
                model(&agent.launch_model, &agent.launch_model_env),
                cx,
            ))
            .child(row(
                "profile-edit-provider",
                "Provider",
                model(&agent.launch_provider, &agent.launch_provider_env),
                cx,
            ))
            .into_any_element()
    }
}

/// Label and value rows (`Rows`).
fn rows(rows: Vec<(&str, String)>, cx: &App) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(space::S3)
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(label.to_string()),
                )
                .child(div().text_mono().child(value))
        }))
}

/// A `<details>` disclosure.
fn disclosure(
    id: &'static str,
    summary: SharedString,
    open: bool,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    body: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let c = cx.colors();
    div()
        .flex()
        .flex_col()
        .gap(space::S2)
        .child(
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(space::S1)
                .cursor_pointer()
                .text_body_sm()
                .text_color(c.text_subtle)
                .on_click(toggle)
                .child(
                    Icon::new(if open {
                        IconName::CaretDown
                    } else {
                        IconName::CaretRight
                    })
                    .size(14.),
                )
                .child(summary),
        )
        .when(open, |this| this.child(body))
        .into_any_element()
}

/// The log view asks the panel to close it.
pub struct LogClosed;

/// "Harness log" (`ProfileHarnessLog`): read when it opens and every 30
/// seconds while the window shows, each read owner-authorized.
pub struct HarnessLog {
    host: Entity<HostConnection>,
    agent: Agent,
    community: String,
    name: SharedString,
    result: Option<Result<String, ()>>,
    pending: bool,
    _poll: Task<()>,
}

impl EventEmitter<LogClosed> for HarnessLog {}

impl HarnessLog {
    fn new(
        host: Entity<HostConnection>,
        agent: Agent,
        community: String,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                let visible = this.update_in(cx, |this, window, cx| {
                    if window.is_window_active() || this.result.is_none() {
                        this.read(cx);
                    }
                });
                if visible.is_err() {
                    return;
                }
                cx.background_executor().timer(LOG_POLL).await;
            }
        });
        Self {
            host,
            agent,
            community,
            name,
            result: None,
            pending: false,
            _poll: poll,
        }
    }

    /// `LogChallenge`, the owner's proof of it, then `ReadLog`.
    fn read(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let Some(identity) = identity(cx) else {
            self.result = Some(Err(()));
            return;
        };
        self.pending = true;
        let (id, pubkey, relay_url) = (
            self.agent.id.clone(),
            self.agent.pubkey.clone(),
            self.agent.relay_url.clone(),
        );
        let community = self.community.clone();
        let challenge = self.host.update(cx, |host, cx| {
            host.request(
                Request::LogChallenge {
                    id: id.clone(),
                    pubkey: pubkey.clone(),
                    relay_url: relay_url.clone(),
                },
                cx,
            )
        });
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            let content = async {
                let Ok(Response::Nonce(nonce)) = challenge.await else {
                    return Err(());
                };
                let signature = identity
                    .harness_log_proof(&community, &id, &pubkey, &relay_url, &nonce)
                    .map_err(drop)?;
                let read = cx.update(|cx| {
                    host.update(cx, |host, cx| {
                        host.request(
                            Request::ReadLog {
                                id,
                                pubkey,
                                relay_url,
                                nonce,
                                signature,
                            },
                            cx,
                        )
                    })
                });
                match read.await {
                    Ok(Response::Log(content)) => Ok(content),
                    _ => Err(()),
                }
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                this.result = Some(content);
                cx.notify();
            });
        })
        .detach();
    }

    fn file_name(&self) -> String {
        let key = &self.agent.pubkey;
        let tail = key.len().saturating_sub(6);
        format!(
            "{} · {}…{}.log",
            self.name,
            &key[..key.len().min(8)],
            &key[tail..]
        )
    }
}

impl Render for HarnessLog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let content = match &self.result {
            Some(Ok(content)) => Some(content.clone()),
            _ => None,
        };
        let copy = content.clone().filter(|content| !content.is_empty());
        let body: AnyElement = match &self.result {
            None => div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .children((0..4).map(|row| {
                    div()
                        .h(px(12.))
                        .w(relative(if row % 2 == 0 { 0.8 } else { 0.6 }))
                        .rounded(radius::CONTROL)
                        .bg(c.border_standard)
                }))
                .into_any_element(),
            Some(Err(())) => div()
                .text_color(c.text_danger)
                .child("Could not read harness log.")
                .into_any_element(),
            Some(Ok(content)) => div()
                .p(space::S3)
                .rounded(radius::CONTROL)
                .bg(c.surface_inset)
                .text_mono()
                .whitespace_normal()
                .child(if content.trim().is_empty() {
                    "No log output yet.".to_string()
                } else {
                    content.clone()
                })
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S2)
                    .child(
                        Button::new("harness-log-back", "Back to profile")
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Ghost)
                            .icon(IconName::CaretLeft)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(LogClosed))),
                    )
                    .child(div().text_body().child("Harness log")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(space::S2)
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .truncate()
                            .child(self.file_name()),
                    )
                    .child(
                        Button::new("harness-log-copy", "Copy log")
                            .size(ButtonSize::Sm)
                            .variant(ButtonVariant::Ghost)
                            .icon(IconName::Copy)
                            .disabled(copy.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(content) = &copy {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        content.clone(),
                                    ));
                                }
                            }),
                    ),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use nostr::prelude::FinalizeEvent;

    #[test]
    fn instance_targets_round_trip_and_validate() {
        let target = InstanceTarget {
            community_origin: "https://relay.example".into(),
            id: "agent-1".into(),
            pubkey: "a".repeat(64),
            viewer: "b".repeat(64),
        };
        let link = instance_target(&target);
        assert!(link.starts_with("buzz:agent-instance:"));
        assert_eq!(parse_instance_target(&link), Some(target.clone()));
        let bad = InstanceTarget {
            community_origin: "https://relay.example/path".into(),
            ..target
        };
        assert_eq!(parse_instance_target(&instance_target(&bad)), None);
        assert_eq!(parse_instance_target("buzz:agent-instance:%7B%7D"), None);
    }

    #[test]
    fn restart_changes_read_like_the_reference() {
        let value = RestartChange::Value {
            before: json!("gpt-4"),
            after: json!(null),
        };
        assert_eq!(diff_change(&value), (Some("gpt-4".into()), "null".into()));
        let masked = RestartChange::Masked {
            before: None,
            after: Some("sk-…1234".into()),
        };
        assert_eq!(
            diff_change(&masked),
            (Some("••••".into()), "sk-…1234".into())
        );
        let text = RestartChange::Text {
            before_chars: Some(10),
            after_chars: None,
        };
        assert_eq!(diff_change(&text), (None, "10 chars → 0 chars".into()));
    }

    #[test]
    fn public_metadata_prefers_the_owner_policy() {
        let keys = nostr::prelude::Keys::generate();
        let owner = nostr::prelude::Keys::generate();
        let agent = keys.public_key().to_hex();
        let event =
            |keys: &nostr::prelude::Keys, kind: u16, content: &str, tags: Vec<Vec<String>>| {
                nostr::prelude::EventBuilder::new(nostr::prelude::Kind::from(kind), content)
                    .tags(
                        tags.into_iter()
                            .map(|tag| nostr::prelude::Tag::parse(tag).unwrap()),
                    )
                    .finalize(keys)
                    .unwrap()
            };
        let legacy = event(
            &keys,
            10100,
            r#"{"agent_type":"goose","capabilities":["code","chat"]}"#,
            vec![],
        );
        assert_eq!(
            public_metadata(std::slice::from_ref(&legacy), &agent, None),
            Some(PublicMetadata {
                agent_type: "goose".into(),
                capabilities: vec!["code".into(), "chat".into()],
            })
        );
        let policy = event(
            &owner,
            30177,
            r#"{"name":"Scout","parallelism":1,"respond_to":"anyone"}"#,
            vec![vec!["d".into(), agent.clone()]],
        );
        let owner_key = owner.public_key().to_hex();
        assert_eq!(
            public_metadata(&[legacy.clone(), policy], &agent, Some(&owner_key)),
            Some(PublicMetadata {
                agent_type: "agent".into(),
                capabilities: Vec::new(),
            })
        );
        let malformed = event(&owner, 30177, "{}", vec![vec!["d".into(), agent.clone()]]);
        assert_eq!(
            public_metadata(&[legacy, malformed], &agent, Some(&owner_key)),
            None
        );
    }
}
