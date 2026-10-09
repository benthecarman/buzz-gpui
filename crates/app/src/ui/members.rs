//! Channel members (ui.md §4.2, `ChannelMembersDialog.tsx`): the roster
//! grouped by role with presence, adding people and agents, and role
//! changes and removal for channel admins.

use super::message::display_name;
use super::modal::{Dismissed, error, group_label, note};
use super::overlay::{self, Align, Dismiss};
use crate::community::Community;
use buzz_relay_client::lifecycle::{ChannelState, MemberRole};
use buzz_relay_client::members::{self, Change, MemberChange, Person};
use buzz_relay_client::{ChannelSummary, ChannelType, PeoplePage, PresenceState};
use buzz_ui::{
    ActiveTheme, AlertDialog, Avatar, AvatarSize, Button, ButtonSize, Dialog, IconButton,
    IconButtonSize, IconName, Menu, MenuItem, MenuSeparator, Presence, SearchField, Spinner,
    Surface, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(180);

pub struct MembersDialog {
    community: Entity<Community>,
    channel: ChannelSummary,
    state: Option<Result<ChannelState, String>>,
    loading: bool,
    input: Entity<InputState>,
    /// People found for the typed text, with the text they answer.
    found: Option<(String, Result<PeoplePage, String>)>,
    searching: Option<Task<()>>,
    /// People being added or changed.
    busy: HashSet<String>,
    error: Option<SharedString>,
    menu: Option<String>,
    confirm: Option<MemberChange>,
    _subscription: Subscription,
}

impl EventEmitter<Dismissed> for MembersDialog {}

impl MembersDialog {
    pub fn new(
        community: Entity<Community>,
        channel: ChannelSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search members"));
        input.update(cx, |state, cx| state.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search(window, cx);
                }
            });
        let mut dialog = Self {
            community,
            channel,
            state: None,
            loading: false,
            input,
            found: None,
            searching: None,
            busy: HashSet::new(),
            error: None,
            menu: None,
            confirm: None,
            _subscription: subscription,
        };
        dialog.refresh(window, cx);
        dialog
    }

    fn viewer(&self, cx: &App) -> String {
        self.community.read(cx).session.viewer().to_string()
    }

    /// Re-reads the roster and roles from the relay.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let state = session.channel_state(&channel).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Ok(state) = &state {
                    let people: Vec<String> = state.roles.keys().cloned().collect();
                    this.community.update(cx, |community, _| {
                        community.session.want_profiles(people.clone());
                        community.set_presence_extra(people);
                    });
                }
                this.state = Some(state);
                this.loading = false;
                this.update_placeholder(window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn update_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = if self.can_add(cx) {
            "Add people and agents"
        } else {
            "Search members"
        };
        self.input.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx);
        });
    }

    fn can_add(&self, cx: &App) -> bool {
        let viewer = self.viewer(cx);
        let member = matches!(&self.state, Some(Ok(state)) if state.role(&viewer).is_some());
        let summary = self.summary(cx);
        members::can_add_members(&summary, member)
    }

    /// The channel as the sidebar knows it now.
    fn summary(&self, cx: &App) -> ChannelSummary {
        self.community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .find(|c| c.id == self.channel.id)
            .cloned()
            .unwrap_or_else(|| self.channel.clone())
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().trim().to_string();
        if query.is_empty() || !self.can_add(cx) {
            self.searching = None;
            self.found = None;
            cx.notify();
            return;
        }
        let session = self.community.read(cx).session.clone();
        self.searching = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let page = session.search_members(&query, 1).await;
            let _ = this.update(cx, |this, cx| {
                this.found = Some((query, page));
                this.searching = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn add(&mut self, pubkey: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.busy.insert(pubkey.clone()) {
            return;
        }
        self.error = None;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.add_member(&channel, &pubkey).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy.remove(&pubkey);
                match result {
                    Ok(()) => this.refresh(window, cx),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(change) = self.confirm.take() else {
            return;
        };
        let pubkey = change.pubkey.clone();
        self.busy.insert(pubkey.clone());
        self.error = None;
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.change_member(&channel, change).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy.remove(&pubkey);
                match result {
                    Ok(()) => this.refresh(window, cx),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss_menu(cx: &mut Context<Self>) -> Dismiss {
        let weak = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.menu = None;
                cx.notify();
            });
        })
    }

    fn avatar(&self, pubkey: &str, agent: bool, cx: &App) -> Avatar {
        let community = self.community.read(cx);
        let snapshot = &community.snapshot;
        let mut avatar = Avatar::new(display_name(&snapshot.profiles, pubkey))
            .size(AvatarSize::Px(32.))
            .presence(snapshot.presence.get(pubkey).map(|state| match state {
                PresenceState::Online => Presence::Online,
                PresenceState::Away => Presence::Away,
                PresenceState::Offline => Presence::Offline,
            }));
        if agent {
            avatar = avatar.squircle();
        }
        if let Some(picture) = snapshot
            .profiles
            .get(pubkey)
            .and_then(|p| p.picture.clone())
        {
            avatar = avatar.image(super::media::avatar(&picture, community.session.media()));
        }
        avatar
    }

    fn member_row(
        &self,
        pubkey: &str,
        role: MemberRole,
        state: &ChannelState,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let viewer = self.viewer(cx);
        let profiles = self.community.read(cx).snapshot.profiles.clone();
        let agent = role == MemberRole::Bot || profiles.get(pubkey).is_some_and(|p| p.is_agent);
        let mut name = display_name(&profiles, pubkey).to_string();
        if pubkey == viewer {
            name.push_str(" (you)");
        }
        let manage = state.can_manage_member(&viewer, pubkey);
        let menu_open = self.menu.as_deref() == Some(pubkey);
        let busy = self.busy.contains(pubkey);
        let key = pubkey.to_string();
        let menu = menu_open.then(|| {
            let change = |change: Change| MemberChange {
                pubkey: key.clone(),
                expected: role,
                change,
            };
            let item = |id: &'static str, label: String, next: MemberChange| {
                MenuItem::new(id, label).on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.confirm = Some(next.clone());
                    cx.notify();
                }))
            };
            let mut menu = Menu::new();
            for (id, next, label) in [
                ("make-admin", Change::Admin, "Make admin"),
                ("make-member", Change::Member, "Make member"),
            ] {
                let target = match next {
                    Change::Admin => MemberRole::Admin,
                    _ => MemberRole::Member,
                };
                if target != role {
                    menu = menu.child(item(id, label.to_string(), change(next)));
                }
            }
            menu = menu.child(MenuSeparator::new()).child(
                item(
                    "remove",
                    "Remove from channel".into(),
                    change(Change::Remove),
                )
                .danger(),
            );
            overlay::dropdown(window, Align::End, menu, Self::dismiss_menu(cx))
        });
        let id = SharedString::from(format!("member-{pubkey}"));
        div()
            .id(id)
            .group("member-row")
            .flex()
            .items_center()
            .gap(rems(0.75))
            .min_h(rems(2.75))
            .px(rems(1.))
            .py(rems(0.25))
            .rounded(px(11.))
            .hover(|s| s.bg(c.interaction(Surface::Floating).row_hover))
            .child(self.avatar(pubkey, agent, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_body_sm()
                    .truncate()
                    .child(name),
            )
            .when(busy, |this| this.child(Spinner::new().size(16.)))
            .when(manage && !busy, |this| {
                this.child(
                    div()
                        .relative()
                        .child(
                            IconButton::new(
                                SharedString::from(format!("member-actions-{pubkey}")),
                                IconName::DotsThree,
                            )
                            .size(IconButtonSize::Sm)
                            .surface(Surface::Floating)
                            .tooltip("Member actions")
                            .on_click(cx.listener({
                                let key = key.clone();
                                move |this, _, _, cx| {
                                    this.menu = Some(key.clone());
                                    cx.notify();
                                }
                            })),
                        )
                        .children(menu),
                )
            })
            .into_any_element()
    }

    fn candidate_row(&self, person: &Person, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let busy = self.busy.contains(&person.pubkey);
        let key = person.pubkey.clone();
        div()
            .id(SharedString::from(format!("candidate-{}", person.pubkey)))
            .flex()
            .items_center()
            .gap(rems(0.75))
            .min_h(rems(2.75))
            .px(rems(1.))
            .py(rems(0.25))
            .rounded(px(11.))
            .hover(|s| s.bg(c.interaction(Surface::Floating).row_hover))
            .child(self.avatar(&person.pubkey, person.profile.is_agent, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_body_sm()
                    .truncate()
                    .child(person.profile.name.clone()),
            )
            .child(
                Button::new(
                    SharedString::from(format!("add-{}", person.pubkey)),
                    if busy { "Adding…" } else { "Add" },
                )
                .prominent()
                .size(ButtonSize::Xs)
                .disabled(busy)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.add(key.clone(), window, cx)),
                ),
            )
            .into_any_element()
    }

    fn confirmation(&self, change: &MemberChange, cx: &mut Context<Self>) -> AnyElement {
        let profiles = self.community.read(cx).snapshot.profiles.clone();
        let name = display_name(&profiles, &change.pubkey);
        let (title, description, action) = match change.change {
            Change::Remove => (
                format!("Remove {name} from channel"),
                format!("{name} loses access to this channel's messages."),
                format!("Remove {name}"),
            ),
            next => {
                let role = if next == Change::Admin {
                    "admin"
                } else {
                    "member"
                };
                (
                    "Change member role?".to_string(),
                    format!(
                        "Change this member’s role from {} to {role}. {}",
                        change.expected.as_str(),
                        if next == Change::Admin {
                            "Admins can manage this channel and its members."
                        } else {
                            "This changes their authority in this channel."
                        }
                    ),
                    format!("Make {role}"),
                )
            }
        };
        let destructive = change.change == Change::Remove;
        AlertDialog::new(title)
            .description(description)
            .action(
                Button::new("member-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    })),
            )
            .action(
                Button::new("member-confirm", action)
                    .map(|b| {
                        if destructive {
                            b.destructive()
                        } else {
                            b.prominent()
                        }
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
            )
            .into_any_element()
    }
}

impl Render for MembersDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(change) = self.confirm.clone() {
            return self.confirmation(&change, cx);
        }
        let c = *cx.colors();
        let viewer = self.viewer(cx);
        let summary = self.summary(cx);
        let profiles = self.community.read(cx).snapshot.profiles.clone();
        let query = self.input.read(cx).value().trim().to_lowercase();
        let can_add = self.can_add(cx);

        let mut list = div().flex().flex_col().gap(rems(0.75));
        if !can_add {
            let reason = if summary.channel_type == Some(ChannelType::Dm) {
                "DM membership cannot be changed here."
            } else if summary.archived {
                "Archived channels cannot add members."
            } else {
                "Join this channel to add people and agents."
            };
            list = list.child(note(reason, cx));
        }
        if let Some(Err(message)) = &self.state {
            list = list.child(div().px(rems(1.)).child(error(message.clone(), cx)));
        }
        if let Some(message) = &self.error {
            list = list.child(div().px(rems(1.)).child(error(message.clone(), cx)));
        }
        match &self.state {
            None => {
                list = list.child(
                    div()
                        .flex()
                        .justify_center()
                        .py(rems(2.))
                        .child(Spinner::new().size(24.)),
                )
            }
            Some(Ok(state)) => {
                let state = state.clone();
                let matches = |key: &str| {
                    query.is_empty()
                        || members::match_tier(&display_name(&profiles, key), &query).is_some()
                };
                let groups = [
                    ("Owners", vec![MemberRole::Owner]),
                    ("Admins", vec![MemberRole::Admin]),
                    (
                        "Members",
                        vec![MemberRole::Member, MemberRole::Guest, MemberRole::Unknown],
                    ),
                    ("Agents", vec![MemberRole::Bot]),
                ];
                let mut shown = 0;
                for (label, roles) in groups {
                    let mut keys: Vec<(&String, MemberRole)> = state
                        .roles
                        .iter()
                        .filter(|(key, role)| {
                            let agent = **role == MemberRole::Bot
                                || profiles.get(*key).is_some_and(|p| p.is_agent);
                            let in_group = if label == "Agents" {
                                agent
                            } else {
                                !agent && roles.contains(role)
                            };
                            in_group && matches(key)
                        })
                        .map(|(key, role)| (key, *role))
                        .collect();
                    if keys.is_empty() && label != "Members" {
                        continue;
                    }
                    keys.sort_by_key(|(key, _)| {
                        (**key != viewer, display_name(&profiles, key).to_lowercase())
                    });
                    shown += keys.len();
                    let mut section = div()
                        .flex()
                        .flex_col()
                        .child(group_label(format!("{label} · {}", keys.len()), cx));
                    for (key, role) in keys {
                        section = section.child(self.member_row(key, role, &state, window, cx));
                    }
                    list = list.child(section);
                }
                if shown == 0 {
                    list = list.child(note(
                        if query.is_empty() {
                            "No members to show."
                        } else {
                            "No members match your search."
                        },
                        cx,
                    ));
                }
                if can_add && !query.is_empty() {
                    let mut section = div()
                        .flex()
                        .flex_col()
                        .child(group_label("Not in this channel", cx));
                    match &self.found {
                        Some((text, Ok(page))) if text.to_lowercase() == query => {
                            let others: Vec<&Person> = page
                                .people
                                .iter()
                                .filter(|p| !state.roles.contains_key(&p.pubkey))
                                .collect();
                            if others.is_empty() {
                                section =
                                    section.child(note("No other matching people or agents.", cx));
                            }
                            for person in others {
                                section = section.child(self.candidate_row(person, cx));
                            }
                        }
                        Some((text, Err(message))) if text.to_lowercase() == query => {
                            section =
                                section.child(div().px(rems(1.)).child(error(message.clone(), cx)));
                        }
                        _ => section = section.child(note("Searching…", cx)),
                    }
                    list = list.child(section);
                }
            }
            Some(Err(_)) => {}
        }

        Dialog::new("channel-members", "Channel members")
            .description(super::sidebar::channel_label(&summary, &profiles))
            .compact_header_gap()
            .header_action(
                IconButton::new("members-refresh", IconName::ArrowsClockwise)
                    .size(IconButtonSize::Sm)
                    .surface(Surface::Floating)
                    .tooltip("Refresh member data")
                    .disabled(self.loading)
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .child(SearchField::new(&self.input).surface(Surface::Floating))
                    .child(
                        div()
                            .id("member-list")
                            .h(px(360.))
                            .overflow_y_scroll()
                            .p(rems(0.25))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(c.border_standard)
                            .child(list),
                    ),
            )
            .into_any_element()
    }
}
