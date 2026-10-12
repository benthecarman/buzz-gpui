//! One inventory identity: buzz-app's `AgentCard`, `InventoryIdentityCard`
//! and `ManagedAgentActions`.
//!
//! "Individual agents" are tiles whose controls open in "Manage {name}";
//! the other groups are rows with their controls inline.

use super::import::{ImportDialog, source_label};
use super::inventory::{self, Action, Decision, Group, Row, Setup};
use super::relay::Definition;
use super::{
    AgentsPage, FocusTarget, Handover, Modal, OpenMenu, Outcome, View, model_line, process_label,
};
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::hosts::control::Status;
use crate::ui::overlay::{self, Align};
use buzz_host::api::{Agent, AgentAction, ImportSource, ProcessStatus};
use buzz_relay_client::PresenceState;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, Dialog, Icon, IconButton, IconButtonSize,
    IconName, Menu, MenuItem, MenuSeparator, MenuSize, Popover, Presence, Select, TypeStyled,
    tokens::radius,
};
use gpui_kit::base::SelectableText;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

fn presence(state: Option<&PresenceState>) -> Option<Presence> {
    state.map(|state| match state {
        PresenceState::Online => Presence::Online,
        PresenceState::Away => Presence::Away,
        PresenceState::Offline => Presence::Offline,
    })
}

impl AgentsPage {
    fn owner_key(cx: &App) -> Option<String> {
        match &cx.global::<AppState>().identity {
            IdentityState::Ready(identity) => Some(identity.pubkey_hex()),
            _ => None,
        }
    }

    /// The picture to show: the saved setup's, else what the relays say.
    fn picture(&self, row: &Row, setup: Option<&Setup>, cx: &App) -> Option<ImageSource> {
        let community = AppState::current(cx)?;
        let community = community.read(cx);
        let saved = setup.and_then(|setup| setup.agent.picture.clone());
        let url = match saved {
            Some(picture) => Some(picture).filter(|p| !p.is_empty()),
            None => row
                .avatar
                .clone()
                .or_else(|| {
                    community
                        .snapshot
                        .profiles
                        .get(&row.pubkey)
                        .and_then(|profile| profile.picture.clone())
                })
                .or_else(|| {
                    self.discovery
                        .read(cx)
                        .source_profile(&row.pubkey)
                        .and_then(|(_, profile)| profile.picture)
                }),
        }?;
        Some(crate::ui::media::avatar(&url, community.session.media()))
    }

    fn presence_of(&self, pubkey: &str, cx: &App) -> Option<Presence> {
        let community = AppState::current(cx)?;
        presence(community.read(cx).snapshot.presence.get(pubkey))
    }

    /// The card: a tile in "Individual agents", a row elsewhere.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn card(
        &self,
        view: &View,
        row: &Row,
        decision: Decision,
        community: Option<&str>,
        _nested: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let tile = decision.group == Group::LocalHere;
        let setups = row.local_setups(&view.destination);
        let managed = (setups.len() == 1).then(|| setups[0].clone());
        let name = row.display_name.clone();
        let archived = view.archived.contains(&row.pubkey);
        let pubkey = row.pubkey.clone();
        let discovery = self.discovery.read(cx);
        let run = discovery.runs.get(&pubkey).cloned();
        // Archive is per community: offer it only where this identity is
        // known.
        let archive_here = !view.destination.is_empty()
            && (row.known.contains(&view.destination)
                || row.setups.contains_key(&view.destination));
        let archivable = archive_here && discovery.may_archive(&pubkey);
        if archive_here {
            self.discovery
                .update(cx, |discovery, cx| discovery.ask_consent(&pubkey, cx));
        }
        let reveal = setups.iter().any(|setup| {
            self.imported.as_ref().is_some_and(|(host, id)| {
                self.host(setup.host)
                    .is_some_and(|h| h.entity_id() == *host)
                    && *id == setup.agent.id
            }) || setup.agent.error.is_some()
                || setup.agent.profile_pending
                || self.outcome(setup).is_some_and(|o| o.notice.is_some())
        });
        let restart = setups
            .iter()
            .any(|setup| super::restart_required(&setup.agent));
        let focused = self.focus_target(|target| match target {
            FocusTarget::Imported((host, id)) => setups.iter().any(|setup| {
                self.host(setup.host)
                    .is_some_and(|h| h.entity_id() == *host)
                    && setup.agent.id == *id
            }),
            FocusTarget::Card(key) => *key == pubkey,
            FocusTarget::Archived => false,
        });
        // An imported tile focuses "Review agent status"; anything else
        // focuses the card's menu.
        let review_focus =
            focused && tile && reveal && matches!(self.focus_at, Some(FocusTarget::Imported(_)));
        let mut avatar = Avatar::new(name.clone())
            .squircle()
            .size(if tile {
                AvatarSize::Px(80.)
            } else {
                AvatarSize::Px(32.)
            })
            .presence(self.presence_of(&pubkey, cx))
            .thinking(
                SharedString::from(format!("thinking-{pubkey}")),
                crate::agent_activity::observer::agent_thinking(&pubkey, cx),
            )
            .alt(name.clone());
        if let Some(picture) = self.picture(row, managed.as_ref(), cx) {
            avatar = avatar.image(picture);
        }
        let restart_badge = restart.then(|| {
            div()
                .flex()
                .items_center()
                .gap(rems(0.25))
                .px(rems(0.5))
                .rounded_full()
                .border_1()
                .border_color(c.border_warning)
                .bg(c.affordance_warning)
                .text_caption()
                .text_color(c.text_warning)
                .child(Icon::new(IconName::ArrowsClockwise).size(14.))
                .child("Restart required")
        });
        let archived_pill = archived.then(|| {
            div()
                .px(rems(0.5))
                .rounded_full()
                .border_1()
                .border_color(c.border_standard)
                .text_caption()
                .text_color(c.text_subtle)
                .child("Archived")
        });
        let label = div()
            .flex()
            .flex_col()
            .min_w_0()
            .when(tile, |this| this.items_center().w_full())
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_label_sm()
                    .when(tile, |this| this.text_center())
                    .truncate()
                    .child(name.clone()),
            )
            .children(restart_badge.map(|badge| div().mt(rems(0.25)).child(badge)))
            .children(archived_pill)
            .when_some(managed.as_ref().filter(|_| tile), |this, setup| {
                this.child(
                    div()
                        .w_full()
                        .mt(rems(0.25))
                        .text_caption()
                        .text_color(c.text_subtle)
                        .text_center()
                        .truncate()
                        .child(model_line(&setup.agent)),
                )
            });
        let menu = self.card_menu(
            row,
            &setups,
            archivable,
            archived,
            run.is_some(),
            tile,
            focused && !review_focus,
            window,
            cx,
        );
        let feedback = run.map(|run| {
            let retry_key = pubkey.clone();
            let action = run.archive;
            match run.error {
                None => div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(if action {
                        "Archiving…"
                    } else {
                        "Unarchiving…"
                    })
                    .into_any_element(),
                Some(error) => div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(0.5))
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_danger)
                            .child(format!(
                                "{} failed: {error}",
                                if action { "Archive" } else { "Unarchive" }
                            )),
                    )
                    .child(
                        Button::new(
                            SharedString::from(format!("archive-retry-{retry_key}")),
                            "Retry",
                        )
                        .size(ButtonSize::Sm)
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.archive(&retry_key, action, cx)),
                        ),
                    )
                    .into_any_element(),
            }
        });
        // An agent set up in the shown community opens its profile
        // (`onViewProfile`); otherwise the tile manages it.
        let view_profile = AppState::current(cx).is_some_and(|community| {
            let community = &community.read(cx).origin;
            setups
                .iter()
                .any(|setup| inventory::in_community(&setup.agent.relay_url, community))
        });
        let manage_key = pubkey.clone();
        let manage = cx.listener(move |this, event: &ClickEvent, window, cx| {
            this.menu = None;
            if view_profile {
                crate::ui::navigation::show_profile(
                    manage_key.clone(),
                    event.position(),
                    window,
                    cx,
                );
            } else {
                this.modal = Some(Modal::Manage(manage_key.clone()));
            }
            cx.notify();
        });
        if tile {
            let review_key = pubkey.clone();
            let content = div()
                .id(SharedString::from(format!("open-{pubkey}")))
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(rems(0.75))
                .px(rems(0.75))
                .pt(rems(2.))
                .pb(rems(1.))
                .rounded(radius::CONTAINER)
                .cursor_pointer()
                .hover(|style| style.bg(c.affordance_subtle_hover))
                .on_click(manage)
                .child(avatar)
                .child(label);
            div()
                .relative()
                .flex()
                .flex_col()
                .min_w_0()
                .min_h(rems(15.25))
                .rounded(radius::CONTAINER)
                .border_1()
                .border_color(c.border_standard)
                .child(content)
                .when(reveal, |this| {
                    this.child(
                        div().px(rems(1.)).pb(rems(1.)).child(
                            Button::new(
                                SharedString::from(format!("review-{review_key}")),
                                "Review agent status",
                            )
                            .size(ButtonSize::Sm)
                            .when(review_focus, |this| this.track_focus(&self.target_focus))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.modal = Some(Modal::Manage(review_key.clone()));
                                    cx.notify();
                                },
                            )),
                        ),
                    )
                })
                .children(feedback.map(|feedback| div().px(rems(1.)).pb(rems(1.)).child(feedback)))
                .child(div().absolute().top(rems(0.5)).right(rems(0.5)).child(menu))
                .when(focused, |this| this.child(self.reveal_on_focus()))
                .into_any_element()
        } else {
            let children =
                self.identity_children(view, row, decision, community, false, window, cx);
            div()
                .relative()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(rems(1.))
                .px(rems(1.))
                .py(rems(0.75))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.75))
                        .min_w_0()
                        .pr(rems(1.5))
                        .child(avatar)
                        .child(label),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_end()
                        .gap(rems(0.5))
                        .min_w_0()
                        .pr(rems(2.))
                        .children(children),
                )
                .children(feedback.map(|feedback| div().w_full().child(feedback)))
                .child(div().absolute().top(rems(0.5)).right(rems(0.5)).child(menu))
                .when(focused, |this| this.child(self.reveal_on_focus()))
                .into_any_element()
        }
    }

    /// A definition with no identity: "Profiles without identities".
    pub(super) fn profile_card(&self, profile: &Definition, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let mut avatar = Avatar::new(profile.name.clone())
            .squircle()
            .size(AvatarSize::Px(80.));
        if let Some(url) = &profile.avatar
            && let Some(community) = AppState::current(cx)
        {
            avatar = avatar.image(crate::ui::media::avatar(
                url,
                community.read(cx).session.media(),
            ));
        }
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(rems(0.75))
            .min_h(rems(15.25))
            .px(rems(0.75))
            .pt(rems(2.))
            .rounded(radius::CONTAINER)
            .border_1()
            .border_color(c.border_standard)
            .child(avatar)
            .child(
                div()
                    .w_full()
                    .text_label_sm()
                    .text_center()
                    .truncate()
                    .child(profile.name.clone()),
            )
            .child(
                div()
                    .mb(rems(1.5))
                    .text_caption()
                    .text_color(c.text_subtle)
                    .child("No linked identity"),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn card_menu(
        &self,
        row: &Row,
        setups: &[Setup],
        archivable: bool,
        archived: bool,
        archive_pending: bool,
        tile: bool,
        focused: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pubkey = row.pubkey.clone();
        let name = row.display_name.clone();
        let open = self.menu == Some(OpenMenu::Card(pubkey.clone()));
        let trigger = IconButton::new(
            SharedString::from(format!("agent-actions-{pubkey}")),
            IconName::DotsThree,
        )
        .size(IconButtonSize::Sm)
        .selected(open)
        .when(focused, |this| this.track_focus(&self.target_focus))
        .when(!open, |this| this.tooltip(format!("Actions for {name}")))
        .on_click(cx.listener({
            let pubkey = pubkey.clone();
            move |this, _, _, cx| this.set_menu(Some(OpenMenu::Card(pubkey.clone())), cx)
        }));
        let view_profile = AppState::current(cx).is_some_and(|community| {
            let community = &community.read(cx).origin;
            setups
                .iter()
                .any(|setup| inventory::in_community(&setup.agent.relay_url, community))
        });
        // With nothing to act on, the button shows the identity's npub
        // instead (buzz-app's "{name} identity details" popover).
        if setups.is_empty() && !archivable && !view_profile && !tile {
            let c = *cx.colors();
            let popover = open.then(|| {
                let details = Popover::new()
                    .title(format!("{name} identity details"))
                    .child(
                        div()
                            .mt(rems(0.75))
                            .text_mono()
                            .text_color(c.text_subtle)
                            .child(SelectableText::new(
                                SharedString::from(format!("identity-npub-{pubkey}")),
                                SharedString::from(inventory::npub(&pubkey)),
                            )),
                    );
                overlay::dropdown(window, Align::End, details, Self::dismiss(cx))
            });
            return div()
                .relative()
                .occlude()
                .child(trigger)
                .children(popover)
                .into_any_element();
        }
        let menu = open.then(|| {
            let item = |id: SharedString, label: SharedString| {
                MenuItem::new(id, label).size(MenuSize::Default)
            };
            let mut menu = Menu::new().size(MenuSize::Wide);
            if view_profile {
                let key = pubkey.clone();
                menu = menu.child(item("view-profile".into(), "View profile".into()).on_click(
                    cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.menu = None;
                        crate::ui::navigation::show_profile(
                            key.clone(),
                            event.position(),
                            window,
                            cx,
                        );
                        cx.notify();
                    }),
                ));
            }
            if tile {
                let key = pubkey.clone();
                menu = menu.child(item("manage".into(), "Manage agent".into()).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.modal = Some(Modal::Manage(key.clone()));
                        cx.notify();
                    }),
                ));
            }
            if archivable {
                let key = pubkey.clone();
                let running = setups
                    .iter()
                    .any(|setup| setup.agent.status == ProcessStatus::Running);
                let shown = name.clone();
                menu = menu.child(
                    item(
                        "archive".into(),
                        if archived {
                            "Unarchive agent"
                        } else {
                            "Archive agent"
                        }
                        .into(),
                    )
                    .icon(if archived {
                        IconName::ArchiveOff
                    } else {
                        IconName::Archive
                    })
                    .icon_size(14.)
                    .disabled(archive_pending)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        if archived {
                            this.archive(&key, false, cx);
                        } else {
                            this.modal = Some(Modal::Archive {
                                pubkey: key.clone(),
                                name: shown.clone(),
                                running,
                            });
                        }
                        cx.notify();
                    })),
                );
            }
            if tile || archivable || view_profile {
                menu = menu.child(MenuSeparator::new());
            }
            if setups.is_empty() {
                menu = menu
                        .child(
                            item("edit".into(), "Edit".into())
                                .icon(IconName::PencilSimple)
                                .icon_size(14.)
                                .disabled(true),
                        )
                        .child(
                            item(
                                "edit-note".into(),
                                if row.local.is_none() && row.relay.is_some()
                                    || !row.sources.is_empty()
                                {
                                    "Import this identity to edit in Foundation."
                                } else {
                                    "No linked identity to edit."
                                }
                                .into(),
                            )
                            .disabled(true),
                        );
            }
            let several = setups.len() > 1;
            for (index, setup) in setups.iter().enumerate() {
                let Some(host) = self.host(setup.host).cloned() else {
                    continue;
                };
                let connected = matches!(
                    host.read(cx).state,
                    buzz_host_rpc::ConnectionState::Connected { .. }
                );
                let label = |verb: &str| -> SharedString {
                    if several {
                        format!("{verb} {}", setup.agent.name).into()
                    } else {
                        verb.to_string().into()
                    }
                };
                let edit = {
                    let host = host.clone();
                    let agent = setup.agent.clone();
                    let name = name.clone();
                    cx.listener(move |this, _, window, cx| {
                        this.edit_agent(host.clone(), agent.clone(), name.clone(), None, window, cx)
                    })
                };
                let duplicate = {
                    let host = host.clone();
                    let agent = setup.agent.clone();
                    cx.listener(move |this, _, window, cx| {
                        this.create_dialog(
                            Some(host.clone()),
                            super::create::Kind::Duplicate(Box::new(agent.clone())),
                            window,
                            cx,
                        );
                    })
                };
                let share = {
                    let host = host.clone();
                    let id = setup.agent.id.clone();
                    cx.listener(move |this, _, window, cx| {
                        this.set_menu(None, cx);
                        super::share::open_agent(host.clone(), id.clone(), window, cx);
                    })
                };
                let launch = {
                    let host = host.clone();
                    let id = setup.agent.id.clone();
                    let value = !setup.agent.start_on_app_launch;
                    cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        host.update(cx, |host, cx| {
                            host.write(
                                buzz_host::api::Request::SetStartOnLaunch {
                                    id: id.clone(),
                                    value,
                                },
                                Default::default(),
                                cx,
                            )
                            .detach()
                        });
                        cx.notify();
                    })
                };
                let delete = {
                    let host = host.clone();
                    let id = setup.agent.id.clone();
                    cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.modal = Some(Modal::Delete {
                            host: host.clone(),
                            id: id.clone(),
                            busy: false,
                            error: None,
                        });
                        cx.notify();
                    })
                };
                let mut edit_item = item(format!("edit-{index}").into(), label("Edit"))
                    .icon(IconName::PencilSimple)
                    .icon_size(14.)
                    .disabled(!connected)
                    .on_click(edit);
                if several {
                    edit_item = edit_item.trailing(div().text_caption().child(format!(
                                "{} · {}",
                                setup.agent.relay_url,
                                inventory::npub(&setup.agent.pubkey)
                                    .chars()
                                    .take(16)
                                    .collect::<String>()
                            )));
                }
                menu = menu
                    .child(edit_item)
                    .child(
                        item(format!("duplicate-{index}").into(), label("Duplicate"))
                            .icon(IconName::Copy)
                            .icon_size(14.)
                            .disabled(!connected)
                            .on_click(duplicate),
                    )
                    .child(
                        item(format!("share-{index}").into(), label("Share"))
                            .disabled(!connected)
                            .on_click(share),
                    )
                    // The profile panel that holds this switch in buzz-app
                    // is not part of this app.
                    .child(
                        item(format!("launch-{index}").into(), label("Start on launch"))
                            .icon(IconName::Lightning)
                            .icon_size(14.)
                            .checked(setup.agent.start_on_app_launch)
                            .disabled(!connected)
                            .on_click(launch),
                    )
                    .child(MenuSeparator::new())
                    .child(
                        item(format!("delete-{index}").into(), label("Delete"))
                            .icon(IconName::Trash)
                            .icon_size(14.)
                            .danger()
                            .disabled(!connected)
                            .on_click(delete),
                    );
            }
            overlay::dropdown(window, Align::End, menu, Self::dismiss(cx))
        });
        div()
            .relative()
            .occlude()
            .child(trigger)
            .children(menu)
            .into_any_element()
    }

    pub(super) fn outcome(&self, setup: &Setup) -> Option<&Outcome> {
        let host = self.host(setup.host)?;
        self.outcomes
            .get(&(host.entity_id(), setup.agent.id.clone()))
    }

    /// Start or Stop from the card, with `useManagedAgentActions`'s notice
    /// when it fails.
    pub(super) fn act(
        &mut self,
        host: Entity<HostConnection>,
        agent: Agent,
        action: AgentAction,
        cx: &mut Context<Self>,
    ) {
        let owner = Self::owner_key(cx);
        let id = agent.id.clone();
        let task = host.update(cx, |host, cx| host.act(&id, action, owner, None, cx));
        let version = host.read(cx).control.action_versions.get(&id).copied();
        let key = (host.entity_id(), id.clone());
        self.outcomes.insert(
            key.clone(),
            Outcome {
                status: agent.status,
                version,
                checking: false,
                notice: None,
            },
        );
        cx.spawn(async move |this, cx| {
            let Err(failure) = task.await else {
                return;
            };
            let current = |this: &Self, cx: &App| {
                host.read(cx).control.action_versions.get(&id).copied() == version
                    && this.outcomes.contains_key(&key)
            };
            let refresh = this.update(cx, |this, cx| {
                if !current(this, cx) {
                    return None;
                }
                if let Some(outcome) = this.outcomes.get_mut(&key) {
                    outcome.checking = true;
                }
                cx.notify();
                Some(host.update(cx, |host, cx| host.refresh(cx)))
            });
            let Ok(Some(refresh)) = refresh else {
                return;
            };
            refresh.await;
            let _ = this.update(cx, |this, cx| {
                if host.read(cx).control.action_versions.get(&id).copied() != version {
                    return;
                }
                this.outcomes.remove(&key);
                let state = host.read(cx);
                let Some(now) = state.agent(&id).cloned() else {
                    return;
                };
                let succeeded = match action {
                    AgentAction::Start | AgentAction::Restart => now.status == ProcessStatus::Running,
                    AgentAction::Stop => now.status == ProcessStatus::Stopped && !now.enabled,
                };
                if now.error.is_none() && !succeeded {
                    let verb = if action == AgentAction::Stop { "stop" } else { "start" };
                    let notice = if state.status() == Status::Ready {
                        let reason = failure.reason.map(|r| format!(" {r}")).unwrap_or_default();
                        format!("The agent didn't {verb}.{reason} Try again.")
                    } else {
                        format!(
                            "We couldn't confirm whether the agent {}. Refresh status before trying again.",
                            if action == AgentAction::Stop { "stopped" } else { "started" }
                        )
                    };
                    this.outcomes.insert(
                        key,
                        Outcome {
                            status: now.status,
                            version,
                            checking: false,
                            notice: Some(notice.into()),
                        },
                    );
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Archive or unarchive an identity in the connected community.
    pub(super) fn archive(&mut self, pubkey: &str, archive: bool, cx: &mut Context<Self>) {
        let name = self
            .view(cx)
            .rows
            .get(pubkey)
            .map(|row| row.display_name.clone())
            .unwrap_or_else(|| "Agent".into());
        let community = self.discovery.read(cx).community_name(cx);
        let page = cx.entity().downgrade();
        let key = pubkey.to_string();
        self.discovery.update(cx, |discovery, cx| {
            discovery.request_archive(
                pubkey,
                archive,
                move |cx| {
                    let _ = page.update(cx, |this, cx| {
                        // The card moved; focus follows it to its new place.
                        this.focus(if archive {
                            FocusTarget::Archived
                        } else {
                            FocusTarget::Card(key.clone())
                        });
                        this.show_toast(
                            format!(
                                "{name} {} in {community}",
                                if archive { "archived" } else { "unarchived" }
                            )
                            .into(),
                            archive.then_some(key),
                            cx,
                        )
                    });
                },
                cx,
            )
        });
    }

    /// One setup's process controls (`ManagedAgentActions`).
    fn managed_actions(
        &self,
        host: &Entity<HostConnection>,
        agent: &Agent,
        show_community: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let state = host.read(cx);
        let status = state.status();
        let busy = state.control.busy;
        let block = state.launch_block(agent);
        let can_stop = state.can_stop(&agent.id);
        let remote = state.remote.is_some().then(|| state.label.clone());
        let key = (host.entity_id(), agent.id.clone());
        let outcome = self.outcomes.get(&key).cloned();
        let imported = self.imported.as_ref() == Some(&key);
        let quiet = |text: String| div().text_body_sm().text_color(c.text_subtle).child(text);
        let text = |text: String| div().text_body_sm().child(text);
        let stoppable_now = matches!(agent.status, ProcessStatus::Stopped | ProcessStatus::Failed);
        let start = {
            let host = host.clone();
            let agent = agent.clone();
            cx.listener(move |this, _, _, cx| {
                this.act(host.clone(), agent.clone(), AgentAction::Start, cx)
            })
        };
        let stop = {
            let host = host.clone();
            let agent = agent.clone();
            cx.listener(move |this, _, _, cx| {
                this.act(host.clone(), agent.clone(), AgentAction::Stop, cx)
            })
        };
        let publish = {
            let host = host.clone();
            let id = agent.id.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                host.update(cx, |host, cx| {
                    host.write(
                        buzz_host::api::Request::PublishProfile { id: id.clone() },
                        crate::hosts::control::Lane {
                            credential_write: true,
                            ..Default::default()
                        },
                        cx,
                    )
                    .detach()
                })
            }
        };
        let use_here = {
            let pubkey = agent.pubkey.clone();
            let host = host.clone();
            let id = agent.id.clone();
            cx.listener(move |this, _, _, cx| {
                this.modal = Some(Modal::Handover {
                    pubkey: pubkey.clone(),
                    action: Handover::Use,
                    setup: Some((host.clone(), id.clone())),
                    source: None,
                    pending: false,
                    error: None,
                });
                cx.notify();
            })
        };
        let id =
            |part: &str| SharedString::from(format!("{part}-{}-{}", host.entity_id(), agent.id));
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.25))
                    .when(show_community, |this| {
                        this.child(quiet(match &remote {
                            Some(label) => format!("{} · {label}", agent.relay_url),
                            None => agent.relay_url.clone(),
                        }))
                    })
                    .child(text(format!(
                        "{}{}",
                        if status == Status::Error {
                            "Last known: "
                        } else {
                            ""
                        },
                        process_label(agent.status)
                    ))),
            )
            .when(imported && !agent.enabled, |this| {
                this.child(text(format!(
                    "Imported, not started. {}",
                    if agent.configured {
                        "Start it when you are ready."
                    } else {
                        "Choose Use here to set up this identity in a community."
                    }
                )))
            })
            .when(!agent.configured, |this| {
                this.child(
                    div().flex().child(
                        Button::new(id("use-here"), "Use here")
                            .disabled(busy || status != Status::Ready)
                            .on_click(use_here),
                    ),
                )
            })
            .when(agent.start_on_app_launch, |this| {
                this.child(quiet("Starts with this app.".into()))
            })
            .children(agent.error.clone().map(text))
            .when(outcome.as_ref().is_some_and(|o| o.checking), |this| {
                this.child(text("Checking agent status…".into()))
            })
            .when_some(
                outcome
                    .as_ref()
                    .filter(|o| !o.checking && agent.error.is_none())
                    .and_then(|o| o.notice.clone()),
                |this, notice| this.child(div().text_body_sm().child(notice)),
            )
            .when(agent.profile_pending, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(rems(0.5))
                        .child(text(
                            "Settings saved. Profile publication is pending.".into(),
                        ))
                        .child(
                            Button::new(id("retry-profile"), "Retry profile")
                                .disabled(busy || status != Status::Ready)
                                .on_click(publish),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(rems(0.5))
                    .when(stoppable_now, |this| {
                        this.child(
                            Button::new(
                                id("start"),
                                if agent.status == ProcessStatus::Failed {
                                    "Retry start"
                                } else {
                                    "Start"
                                },
                            )
                            .prominent()
                            .size(ButtonSize::Sm)
                            .disabled(block.is_some())
                            .on_click(start),
                        )
                    })
                    .child(
                        Button::new(id("stop"), "Stop")
                            .size(ButtonSize::Sm)
                            .disabled(!can_stop)
                            .on_click(stop),
                    ),
            )
            .when_some(
                block.filter(|_| agent.status != ProcessStatus::Running),
                |this, block| this.child(quiet(block)),
            )
    }

    /// The card's controls (`InventoryIdentityCard` children).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn identity_children(
        &self,
        view: &View,
        row: &Row,
        decision: Decision,
        community: Option<&str>,
        tile: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let c = *cx.colors();
        let mut children: Vec<AnyElement> = Vec::new();
        let section = community.unwrap_or_default();
        for setup in row.local_setups(&view.destination) {
            let Some(host) = self.host(setup.host).cloned() else {
                continue;
            };
            let show = row.setups.get(section) != Some(&setup);
            children.push(
                self.managed_actions(&host, &setup.agent, show, cx)
                    .into_any_element(),
            );
        }
        for setup in &row.unconfigured {
            children.push(
                div()
                    .text_body_sm()
                    .child(format!(
                        "Earlier import needs setup: {}. Connect to the intended community and \
                         choose Use here. It stays stopped.",
                        if setup.agent.relay_url.is_empty() {
                            "No saved community"
                        } else {
                            &setup.agent.relay_url
                        }
                    ))
                    .into_any_element(),
            );
        }
        let sources = row.import_sources();
        let needs_source = row.local.is_none() && sources.len() > 1;
        let source = if sources.len() == 1 {
            sources.first().copied()
        } else {
            self.sources
                .get(&row.pubkey)
                .copied()
                .filter(|source| sources.contains(source))
        };
        let source_host = row
            .sources
            .iter()
            .find(|(_, s)| Some(*s) == source)
            .and_then(|(host, _)| self.host(*host).cloned());
        let local_host = row
            .local
            .as_ref()
            .and_then(|(host, _)| self.host(*host).cloned());
        let control_host = local_host.clone().or(source_host.clone());
        let (busy, ready, local_actions, import_available) = control_host
            .as_ref()
            .map(|host| {
                let host = host.read(cx);
                (
                    host.control.busy,
                    host.status() == Status::Ready,
                    host.agents.local_inventory_actions,
                    host.agents.import_available != Some(false),
                )
            })
            .unwrap_or((false, false, false, false));
        let mut actions: Vec<AnyElement> = Vec::new();
        if needs_source {
            let key = row.pubkey.clone();
            let open = self.menu == Some(OpenMenu::Source(key.clone()));
            let mut menu = Menu::new().size(MenuSize::Wide);
            for value in &sources {
                let value = *value;
                let key = key.clone();
                menu = menu.child(
                    MenuItem::new(source_label(value), source_label(value))
                        .checked(source == Some(value))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sources.insert(key.clone(), value);
                            this.menu = None;
                            cx.notify();
                        })),
                );
            }
            let toggle = key.clone();
            actions.push(
                div()
                    .relative()
                    .min_w(rems(12.))
                    .child(
                        Select::new(SharedString::from(format!("source-{key}")))
                            .placeholder("Choose an installation")
                            .when_some(source, |this, source| this.value(source_label(source)))
                            .open(open)
                            .disabled(busy || !ready)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let menu = OpenMenu::Source(toggle.clone());
                                this.menu = (this.menu != Some(menu.clone())).then_some(menu);
                                cx.notify();
                            })),
                    )
                    .when(open, |this| {
                        this.child(overlay::dropdown(
                            window,
                            Align::Start,
                            menu,
                            Self::dismiss(cx),
                        ))
                    })
                    .into_any_element(),
            );
        }
        if decision.action == Action::Import {
            let pubkey = row.pubkey.clone();
            let name = row.display_name.clone();
            let host = source_host.clone();
            actions.push(
                Button::new(SharedString::from(format!("import-{pubkey}")), "Import")
                    .prominent()
                    .size(ButtonSize::Sm)
                    .tooltip(
                        "Bring this agent into this app with its existing identity and key. It \
                         stays stopped in your chosen community until you start it.",
                    )
                    .disabled(
                        busy || !ready || !import_available || (needs_source && source.is_none()),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let (Some(host), Some(source)) = (host.clone(), source) {
                            this.open_import(
                                host,
                                pubkey.clone(),
                                name.clone(),
                                source,
                                window,
                                cx,
                            );
                        }
                    }))
                    .into_any_element(),
            );
        }
        if decision.action == Action::Use {
            let pubkey = row.pubkey.clone();
            let setup = row
                .unconfigured
                .iter()
                .find(|setup| inventory::origin(&setup.agent.relay_url) == view.destination)
                .or(row.unconfigured.first())
                .and_then(|setup| Some((self.host(setup.host)?.clone(), setup.agent.id.clone())));
            actions.push(
                Button::new(SharedString::from(format!("use-{pubkey}")), "Use here")
                    .size(ButtonSize::Sm)
                    .disabled(busy || !ready || decision.blocked.is_some())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.modal = Some(Modal::Handover {
                            pubkey: pubkey.clone(),
                            action: Handover::Use,
                            setup: setup.clone(),
                            source: None,
                            pending: false,
                            error: None,
                        });
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if let Some(blocked) = decision.blocked {
                actions.push(div().text_body_sm().child(blocked).into_any_element());
            }
        }
        let clone_button = |this: &Self, cx: &mut Context<Self>| {
            (row.local.is_some() || !sources.is_empty()).then(|| {
                let pubkey = row.pubkey.clone();
                // Clone reads the setup the card shows.
                let setup = row
                    .local_setups(&view.destination)
                    .first()
                    .and_then(|setup| {
                        Some((this.host(setup.host)?.clone(), setup.agent.id.clone()))
                    });
                let from_source = source_host.clone().zip(source);
                let local = row.local.is_some();
                Button::new(SharedString::from(format!("clone-{pubkey}")), "Clone")
                    .size(ButtonSize::Sm)
                    .tooltip(
                        "Create a new agent from this agent’s name and instructions, with a new \
                         identity and key. Memories and history are not copied.",
                    )
                    .disabled(
                        busy || !ready
                            || (local && !local_actions)
                            || view.destination.is_empty()
                            || (needs_source && source.is_none()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.modal = Some(Modal::Handover {
                            pubkey: pubkey.clone(),
                            action: Handover::Clone,
                            setup: if local { setup.clone() } else { None },
                            source: if local { None } else { from_source.clone() },
                            pending: false,
                            error: None,
                        });
                        cx.notify();
                    }))
                    .into_any_element()
            })
        };
        if tile || decision.action == Action::Clone {
            actions.extend(clone_button(self, cx));
        }
        if decision.group == Group::Relay {
            let discovery = self.discovery.read(cx);
            if row.known.contains(&view.destination) {
                let removal = discovery.removals.get(&row.pubkey).cloned();
                let pubkey = row.pubkey.clone();
                let name = row.display_name.clone();
                actions.push(
                    Button::new(SharedString::from(format!("remove-{pubkey}")), "Remove")
                        .destructive()
                        .size(ButtonSize::Sm)
                        .loading(removal.as_ref().is_some_and(|r| r.running))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.modal = Some(Modal::Remove {
                                pubkey: pubkey.clone(),
                                name: name.clone(),
                            });
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                if let Some(error) = removal.and_then(|r| r.error) {
                    actions.push(
                        div()
                            .text_body_sm()
                            .text_color(c.text_danger)
                            .child(error)
                            .into_any_element(),
                    );
                }
            } else if !view.destination.is_empty() {
                actions.push(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child("Switch to this community to remove this agent.")
                        .into_any_element(),
                );
            }
        }
        if tile {
            children.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(0.5))
                    .children(actions)
                    .into_any_element(),
            );
        } else {
            children.extend(actions);
        }
        if decision.action == Action::Wait
            && let Some(blocked) = decision.blocked
        {
            children.push(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(blocked)
                    .into_any_element(),
            );
        }
        // "Identity & sources".
        let open = self.details.contains(&row.pubkey);
        let toggle_key = row.pubkey.clone();
        let toggle = cx.listener(move |this, _, _, cx| {
            if !this.details.remove(&toggle_key) {
                this.details.insert(toggle_key.clone());
            }
            cx.notify();
        });
        let summary = if tile {
            div()
                .id(SharedString::from(format!("details-{}", row.pubkey)))
                .flex()
                .items_center()
                .gap(rems(0.25))
                .cursor_pointer()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(
                    Icon::new(if open {
                        IconName::CaretDown
                    } else {
                        IconName::CaretRight
                    })
                    .size(14.),
                )
                .child("Identity & sources")
                .on_click(toggle)
                .into_any_element()
        } else {
            IconButton::new(
                SharedString::from(format!("details-{}", row.pubkey)),
                if open {
                    IconName::CaretUp
                } else {
                    IconName::CaretDown
                },
            )
            .size(IconButtonSize::Sm)
            .tooltip(format!("Details for {}", row.display_name))
            .on_click(toggle)
            .into_any_element()
        };
        let mut details = div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .min_w_0()
            .child(summary);
        if open {
            let mut body = div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .min_w_0()
                .text_body_sm()
                .text_color(c.text_subtle);
            for community in row
                .known
                .iter()
                .filter(|c| !c.is_empty() && !row.setups.contains_key(*c))
            {
                body = body.child(community.clone());
            }
            if !sources.is_empty() {
                body = body.child(
                    sources
                        .iter()
                        .map(|source| source_label(*source))
                        .collect::<Vec<_>>()
                        .join(" · "),
                );
            }
            if !tile && decision.action != Action::Clone {
                body =
                    body.children(clone_button(self, cx).map(|button| div().flex().child(button)));
            }
            body = body.child(div().text_mono().child(inventory::npub(&row.pubkey)));
            details = details.child(body);
        }
        children.push(
            details
                .when(!tile && open, |this| this.w_full())
                .into_any_element(),
        );
        children
    }

    /// "Manage {name}": every setup's controls and the identity actions.
    pub(super) fn manage_dialog(
        &self,
        view: &View,
        row: &Row,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let decision = view.decide(row);
        let children = self.identity_children(view, row, decision, None, true, window, cx);
        Dialog::new("manage-agent", format!("Manage {}", row.display_name))
            .on_close(cx.listener(|this, _, _, cx| this.close_modal(cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .min_w_0()
                    .children(children),
            )
            .into_any_element()
    }

    pub(super) fn open_import(
        &mut self,
        host: Entity<HostConnection>,
        pubkey: String,
        name: String,
        source: ImportSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let destination = self.discovery.read(cx).destination();
        let dialog = cx.new(|cx| {
            ImportDialog::single(host.clone(), pubkey, name, source, destination, window, cx)
        });
        let host_id = host.entity_id();
        let subscriptions = [
            cx.subscribe(&dialog, |this, _, _: &super::Dismissed, cx| {
                this.close_modal(cx)
            }),
            cx.subscribe(&dialog, move |this, _, super::import::Imported(id), cx| {
                this.imported = Some((host_id, id.clone()));
                this.focus(FocusTarget::Imported((host_id, id.clone())));
                this.close_modal(cx);
            }),
        ];
        self.menu = None;
        self.modal = Some(Modal::Import {
            dialog,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// "Repair team import from another installation".
    pub(super) fn repair(
        &self,
        host: &Entity<HostConnection>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let host_id = host.entity_id();
        let open = self.repair_open.contains(&host_id);
        let toggle_host = host.clone();
        let header = div()
            .id(SharedString::from(format!("repair-{host_id}")))
            .flex()
            .items_center()
            .justify_between()
            .py(rems(0.5))
            .px(rems(0.5))
            .mx(rems(-0.5))
            .rounded(radius::CONTROL)
            .cursor_pointer()
            .hover(|this| this.bg(c.affordance_subtle_hover))
            .child(
                div()
                    .text_label()
                    .child("Repair team import from another installation"),
            )
            .child(
                Icon::new(if open {
                    IconName::CaretUp
                } else {
                    IconName::CaretDown
                })
                .size(16.)
                .color(c.text_subtle),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if !this.repair_open.remove(&host_id) {
                    this.repair_open.insert(host_id);
                    if !this.repairs.contains_key(&host_id) {
                        let destination = this.discovery.read(cx).destination();
                        let host = toggle_host.clone();
                        let dialog =
                            cx.new(|cx| ImportDialog::repair(host, destination, window, cx));
                        let cloned = cx.subscribe_in(
                            &dialog,
                            window,
                            |this, _, super::import::Cloned(settings), window, cx| {
                                let seed = super::draft::Seed {
                                    name: settings.name.clone(),
                                    system_prompt: settings.system_prompt.clone(),
                                };
                                this.create_dialog(
                                    None,
                                    super::create::Kind::Clone(seed),
                                    window,
                                    cx,
                                );
                            },
                        );
                        this.repairs.insert(host_id, (dialog, cloned));
                    }
                }
                cx.notify();
            }));
        div()
            .flex()
            .flex_col()
            .child(header)
            .when(open, |this| {
                this.children(self.repairs.get(&host_id).map(|(dialog, _)| dialog.clone()))
            })
            .into_any_element()
    }
}
