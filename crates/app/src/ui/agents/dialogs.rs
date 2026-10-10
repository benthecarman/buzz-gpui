//! The page's small dialogs: Delete (`AgentDeleteDialog`), Archive
//! (`AgentArchiveDialog`), relay-only Remove (`RelayAgentRemove`), and the
//! Use here and Clone handover (`LocalInventoryAction`).

use super::create::Kind;
use super::draft::Seed;
use super::{AgentsPage, Handover, Modal};
use crate::community::{AppState, IdentityState};
use crate::hosts::control::{Lane, Status};
use buzz_host::api::{CommunityResolution, Request, Response};
use buzz_ui::{ActiveTheme, AlertDialog, Button, Dialog, Surface, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

impl AgentsPage {
    pub(super) fn delete_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let Some(Modal::Delete {
            host,
            id,
            busy,
            error,
        }) = &self.modal
        else {
            return None;
        };
        let state = host.read(cx);
        let agent = state.agent(id)?.clone();
        let blocked = *busy || state.control.busy || state.status() != Status::Ready;
        let shown = state
            .control
            .error
            .clone()
            .map(SharedString::from)
            .or(error.clone());
        let retry = state.status() == Status::Error;
        let refresh = host.clone();
        Some(
            AlertDialog::new(format!("Delete {}?", agent.name))
                .description(
                    "This stops the agent and removes its settings and app credential from this \
                     device. Its relay identity and past messages remain visible. This cannot be \
                     undone here.",
                )
                .children(
                    shown.map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
                )
                .when(retry, |this| {
                    this.child(
                        div().flex().child(
                            Button::new("delete-retry-status", "Retry status")
                                .surface(Surface::Floating)
                                .on_click(move |_, _, cx| {
                                    refresh.update(cx, |host, cx| host.refresh(cx).detach())
                                }),
                        ),
                    )
                })
                .action(
                    Button::new("cancel-delete", "Cancel")
                        .surface(Surface::Floating)
                        .disabled(*busy)
                        .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                )
                .action(
                    Button::new("confirm-delete", "Delete agent")
                        .destructive()
                        .loading(*busy)
                        .disabled(blocked)
                        .on_click(cx.listener(|this, _, _, cx| this.delete_agent(cx))),
                )
                .into_any_element(),
        )
    }

    fn delete_agent(&mut self, cx: &mut Context<Self>) {
        let Some(Modal::Delete { host, id, busy, .. }) = &mut self.modal else {
            return;
        };
        let Some(revision) = host.read(cx).agent(id).map(|agent| agent.revision) else {
            self.modal = None;
            return;
        };
        *busy = true;
        let request = Request::Delete {
            id: id.clone(),
            revision,
        };
        let response = host.update(cx, |host, cx| host.write(request, Lane::default(), cx));
        cx.spawn(async move |this, cx| {
            let result = response.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(_) => this.modal = None,
                    Err(failure) => {
                        if let Some(Modal::Delete { busy, error: e, .. }) = &mut this.modal {
                            *busy = false;
                            *e = Some(if failure.message.is_empty() {
                                "Could not confirm deletion.".into()
                            } else {
                                failure.message.into()
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn archive_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let Some(Modal::Archive {
            pubkey,
            name,
            running,
        }) = &self.modal
        else {
            return None;
        };
        let community = self.discovery.read(cx).community_name(cx);
        let key = pubkey.clone();
        Some(
            AlertDialog::new(format!("Archive {name}?"))
                .description(format!(
                    "This hides {name} from search, mentions, and Add member in {community}. It \
                     won’t remove the agent from its channels or delete its saved setup. You can \
                     unarchive it later."
                ))
                .when(*running, |this| {
                    this.child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child("This agent is running. Archiving won’t stop it."),
                    )
                })
                .action(
                    Button::new("cancel-archive", "Cancel")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                )
                .action(
                    Button::new("confirm-archive", "Archive agent")
                        .prominent()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_modal(cx);
                            this.archive(&key, true, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    pub(super) fn remove_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let Some(Modal::Remove { pubkey, name }) = &self.modal else {
            return None;
        };
        let key = pubkey.clone();
        let bullet = |text: &'static str| {
            div()
                .flex()
                .gap(rems(0.5))
                .child("•")
                .child(div().flex_1().child(text))
        };
        Some(
            AlertDialog::new(format!("Remove {name}?"))
                .description(
                    "Removing this agent cleans it up in this community. It does not delete the \
                     agent's key.",
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.25))
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(bullet(
                            "Archives the agent so it no longer appears in member lists or \
                             mention suggestions. This needs you to own its profile or \
                             administer this community.",
                        ))
                        .child(bullet(
                            "Deletes your saved record of this agent in this community.",
                        ))
                        .child(bullet(
                            "Tries to remove the agent from every channel it belongs to.",
                        )),
                )
                .action(
                    Button::new("cancel-remove", "Cancel")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
                )
                .action(
                    Button::new("confirm-remove", "Remove agent")
                        .destructive()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.close_modal(cx);
                            this.discovery
                                .update(cx, |discovery, cx| discovery.remove(&key, cx));
                        })),
                )
                .into_any_element(),
        )
    }

    /// "Set up agent here" or "Review agent to clone".
    pub(super) fn handover_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let Some(Modal::Handover {
            action,
            setup,
            source,
            pending,
            error,
            ..
        }) = &self.modal
        else {
            return None;
        };
        let destination = self.discovery.read(cx).destination();
        let owner = matches!(cx.global::<AppState>().identity, IdentityState::Ready(_));
        let use_here = *action == Handover::Use;
        let host = setup
            .as_ref()
            .map(|(host, _)| host.clone())
            .or(source.as_ref().map(|(host, _)| host.clone()));
        let disabled = host.as_ref().is_none_or(|host| {
            let host = host.read(cx);
            host.control.busy || host.status() != Status::Ready
        });
        let actionable = setup.is_some() || source.is_some();
        let body =
            if actionable {
                div()
                .flex()
                .flex_col()
                .gap(rems(0.75))
                .child(div().child(if use_here {
                    "Finish setting up this older incomplete import in this community using its \
                     existing key. It stays stopped. To copy an agent already configured \
                     elsewhere, use Clone."
                } else {
                    "Review the saved name and instructions before creating a new agent. The \
                     original agent does not change."
                }))
                .when(use_here && !destination.is_empty(), |this| {
                    this.child(div().child(format!("Destination: {destination}")))
                })
                .when(use_here && (destination.is_empty() || !owner), |this| {
                    this.child(div().child(
                        "Connect to the destination community to use this identity there.",
                    ))
                })
                .child(
                    div().flex().child(
                        Button::new(
                            "handover-act",
                            if use_here { "Use here" } else { "Review clone" },
                        )
                        .surface(Surface::Floating)
                        .loading(*pending)
                        .disabled(disabled || *pending || destination.is_empty() || !owner)
                        .on_click(cx.listener(|this, _, window, cx| this.hand_over(window, cx))),
                    ),
                )
                .children(error.clone().map(|error| div().text_color(c.text_danger).child(error)))
            } else {
                div().child(
                    "This app cannot set up this agent yet. Import it first. If it is already \
                 imported, update and restart the desktop app.",
                )
            };
        let busy = *pending;
        Some(
            Dialog::new(
                "handover",
                if use_here {
                    "Set up agent here"
                } else {
                    "Review agent to clone"
                },
            )
            .description(if use_here {
                "Set up the imported agent in this community. It will not start yet."
            } else {
                "Create a new agent from the saved name and instructions. The new agent gets a \
                 new key and does not join any channels automatically."
            })
            .on_close(cx.listener(move |this, _, _, cx| {
                if !busy {
                    this.close_modal(cx)
                }
            }))
            .child(body)
            .footer(
                Button::new("handover-close", "Close")
                    .surface(Surface::Floating)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.close_modal(cx))),
            )
            .into_any_element(),
        )
    }

    fn hand_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Modal::Handover {
            pubkey,
            action,
            setup,
            source,
            pending,
            error,
        }) = &mut self.modal
        else {
            return;
        };
        let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
            return;
        };
        let identity = identity.clone();
        let destination = self.discovery.read(cx).destination();
        *pending = true;
        *error = None;
        let action = *action;
        let pubkey = pubkey.clone();
        let (setup, source) = (setup.clone(), source.clone());
        let failure = |reason: String| -> SharedString {
            if reason.is_empty() {
                "Could not complete this action. Refresh and try again.".into()
            } else {
                reason.into()
            }
        };
        let task: Task<Result<Option<Seed>, SharedString>> = match (action, setup, source) {
            (Handover::Use, Some((host, id)), _) => {
                let relay = buzz_relay_client::RelayOrigin::parse(&destination)
                    .map(|origin| origin.websocket().to_string())
                    .unwrap_or_default();
                let signature = match identity.resolve_agent_community(&pubkey, &relay) {
                    Ok(signature) => signature,
                    Err(_) => {
                        *pending = false;
                        *error = Some("Use here is unavailable.".into());
                        return cx.notify();
                    }
                };
                let request = Request::UseHere {
                    id,
                    resolution: CommunityResolution {
                        pubkey: pubkey.clone(),
                        relay_url: relay,
                        owner: identity.pubkey_hex(),
                        signature,
                    },
                };
                let write = host.update(cx, |host, cx| {
                    host.write(
                        request,
                        Lane {
                            credential_write: true,
                            ..Default::default()
                        },
                        cx,
                    )
                });
                cx.spawn(async move |_, _| {
                    write
                        .await
                        .map(|_| None)
                        .map_err(|f| failure(f.reason.unwrap_or_default()))
                })
            }
            (Handover::Clone, setup, source) => {
                let request = match (setup.as_ref(), source.as_ref()) {
                    (Some((_, id)), _) => Some(Request::LocalCloneSettings { id: id.clone() }),
                    (None, Some((_, source))) => Some(Request::CloneSettings {
                        source: *source,
                        pubkey: pubkey.clone(),
                    }),
                    _ => None,
                };
                let host = setup.map(|(host, _)| host).or(source.map(|(host, _)| host));
                match (request, host) {
                    (Some(request), Some(host)) => {
                        let read = host.update(cx, |host, cx| host.request(request, cx));
                        cx.spawn(async move |_, _| match read.await {
                            Ok(Response::CloneSettings(settings)) => Ok(Some(Seed {
                                name: settings.name,
                                system_prompt: settings.system_prompt,
                            })),
                            Ok(_) => Err(failure(String::new())),
                            Err(buzz_host_rpc::RpcError::Failed(reason)) => Err(failure(reason)),
                            Err(_) => Err(failure(String::new())),
                        })
                    }
                    _ => Task::ready(Err(
                        "Cloning this agent is unavailable in this app version.".into(),
                    )),
                }
            }
            (Handover::Use, None, _) => Task::ready(Err("Use here is unavailable.".into())),
        };
        let _ = window;
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(None) => this.close_modal(cx),
                    Ok(Some(seed)) => {
                        this.modal = None;
                        this.create_dialog(None, Kind::Clone(seed), window, cx);
                    }
                    Err(message) => {
                        if let Some(Modal::Handover { pending, error, .. }) = &mut this.modal {
                            *pending = false;
                            *error = Some(message);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
