//! The profile panel (`bundled/profiles/ProfilePanel.tsx`): a person or
//! agent beside the conversation, opened from a mention, an avatar or the
//! members list, or as the viewer's own profile from the account menu.
//! Tabs: Info, Runtime (the viewer's agents running here), Channels, and
//! Memories (what the viewer's agent remembers about them).
//!
//! It is a channel panel for profile links (`nostr:npub…` and the app's
//! `buzz:agent-profile:<hex>`), so a profile opens as a tab in the
//! channel's tab pane; the companion dock shows the same view.

mod agent;

pub use agent::parse_instance_target;

use super::channel_panels::{self, ChannelPanel, Opening};
use super::media;
use super::message::channel_icon;
use crate::community::Community;
use crate::hosts::{HostConnection, Hosts};
use buzz_host::api::{Agent, ProcessStatus, Request};
use buzz_relay_client::{
    ChannelType, MemoryError, MemoryListing, PresenceState, RelayOrigin, RosterStatus, StatusWatch,
};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, ButtonVariant, Icon, IconName,
    NavigationItem, Presence, Switch, Tab, TabVariant, Tabs, TypeStyled,
    tokens::{radius, space},
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nostr::nips::nip19::{FromBech32, ToBech32};
use nostr::prelude::PublicKey;
use std::collections::HashSet;
use std::time::Duration;

/// The panel's key among channel panels.
pub const KEY: &str = "buzz.profiles/profile";
/// How long "Loading profile…" waits for the profile before saying there
/// is none in this community.
const PROFILE_WAIT: Duration = Duration::from_secs(10);
const AUTO_RESTART_OFF_BLURB: &str = "Configuration changed since this agent started. Automatic \
                                      restart is off for this agent — stop and respawn it to \
                                      apply the changes.";

/// A person's profile link (`profileTarget`): `nostr:npub…`, or with
/// the agent hint the app-local `buzz:agent-profile:<hex>`.
pub fn target(pubkey: &str, agent: bool) -> Option<String> {
    let key = PublicKey::from_hex(pubkey).ok()?;
    Some(match agent {
        true => format!("buzz:agent-profile:{}", key.to_hex()),
        false => format!("nostr:{}", key.to_bech32().ok()?),
    })
}

/// The key a profile link names and whether it carries the agent hint
/// (`profilePanelKey`, `profileAgentHint`).
pub fn parse_target(target: &str) -> Option<(String, bool)> {
    if let Some(hex) = target.strip_prefix("buzz:agent-profile:") {
        let valid = hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        return valid.then(|| (hex.to_string(), true));
    }
    let npub = target.strip_prefix("nostr:")?;
    if !npub.starts_with("npub1") {
        return None;
    }
    Some((PublicKey::from_bech32(npub).ok()?.to_hex(), false))
}

/// Registers the panel for profile links.
pub fn init(cx: &mut App) {
    channel_panels::register(
        ChannelPanel {
            key: KEY,
            title: "Profile",
            icon: IconName::User,
            matches: |target| {
                parse_target(target).is_some() || parse_instance_target(target).is_some()
            },
            tool: false,
            launcher: None,
            menu: None,
            build: |opening, window, cx| {
                cx.new(|cx| ProfilePanel::from_opening(opening, window, cx))
                    .into()
            },
        },
        cx,
    );
    channel_panels::register_title(
        KEY,
        |view, cx| {
            let panel = view.clone().downcast::<ProfilePanel>().ok()?;
            Some(panel.read(cx).name(cx))
        },
        cx,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Info,
    Runtime,
    Channels,
    Memories,
}

/// The "Add agent to channel" dialog (`ProfileAddChannel`).
#[derive(Clone, Debug, Default)]
struct AddChannel {
    selected: Option<String>,
    menu: bool,
    pending: bool,
    error: Option<SharedString>,
}

/// `MemorySnapshot["status"]`, with the listing when ready.
#[derive(Clone, Debug, PartialEq)]
enum Memories {
    Idle,
    Blocked,
    Loading,
    Ready(MemoryListing),
    Error,
    Denied,
}

pub struct ProfilePanel {
    community: Entity<Community>,
    pubkey: String,
    /// The link said this is an agent; presentation only.
    agent_hint: bool,
    /// The conversation the panel was opened from.
    context: Option<String>,
    close: Option<channel_panels::Close>,
    tab: Section,
    /// The profile read has had its chance to answer.
    settled: bool,
    copy_status: Option<SharedString>,
    /// The copy field that just copied, by label.
    copied: Option<&'static str>,
    opening_message: bool,
    message_error: Option<SharedString>,
    memories: Memories,
    memory_generation: u64,
    open_memories: HashSet<String>,
    runtime_notice: Option<(SharedString, bool)>,
    /// "Add agent to channel", while open.
    add_channel: Option<AddChannel>,
    agent: agent::AgentState,
    focus: FocusHandle,
    _status: StatusWatch,
    _wait: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ProfilePanel {
    fn from_opening(opening: Opening, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // An instance link shows that one record, for its viewer only.
        let instance = parse_instance_target(&opening.target).filter(|instance| {
            let community = opening.community.read(cx);
            instance.viewer == community.session.viewer()
                && instance.community_origin == community.origin.http()
        });
        let (pubkey, agent_hint) = match &instance {
            Some(instance) => (instance.pubkey.clone(), true),
            None => parse_target(&opening.target).unwrap_or_default(),
        };
        let mut panel = Self::new(opening.community, pubkey, agent_hint, window, cx);
        panel.agent.instance = instance.map(|instance| instance.id);
        panel.context = Some(opening.channel);
        panel.close = Some(opening.close);
        panel
    }

    pub fn new(
        community: Entity<Community>,
        pubkey: String,
        agent_hint: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let status = community
            .read(cx)
            .session
            .watch_statuses(std::slice::from_ref(&pubkey));
        community.update(cx, |community, _| community.hold_profile_presence(&pubkey));
        let observe = cx.observe(&community, |this, _, cx| {
            if !this.settled && this.profile(cx).is_some() {
                this.settled = true;
                // The verified owner scopes the public metadata.
                this.load_agent(cx);
            }
            cx.notify();
        });
        let hosts = cx
            .try_global::<Hosts>()
            .map(|hosts| hosts.hosts.clone())
            .unwrap_or_default();
        let mut subscriptions = vec![observe];
        subscriptions.extend(
            hosts
                .iter()
                .map(|host| cx.observe(host, |_, _, cx| cx.notify())),
        );
        subscriptions.push(cx.on_release(|this: &mut Self, cx| {
            let pubkey = this.pubkey.clone();
            this.community.update(cx, |community, _| {
                community.release_profile_presence(&pubkey)
            });
        }));
        let mut panel = Self {
            community,
            pubkey,
            agent_hint,
            context: None,
            close: None,
            tab: Section::Info,
            settled: false,
            copy_status: None,
            copied: None,
            opening_message: false,
            message_error: None,
            memories: Memories::Idle,
            memory_generation: 0,
            open_memories: HashSet::new(),
            runtime_notice: None,
            add_channel: None,
            agent: agent::AgentState::default(),
            focus: cx.focus_handle(),
            _status: status,
            _wait: None,
            _subscriptions: subscriptions,
        };
        panel.load(cx);
        panel
    }

    pub fn pubkey(&self) -> &str {
        &self.pubkey
    }

    fn session(&self, cx: &App) -> std::sync::Arc<buzz_relay_client::Session> {
        self.community.read(cx).session.clone()
    }

    fn profile(&self, cx: &App) -> Option<std::sync::Arc<buzz_relay_client::Profile>> {
        self.community
            .read(cx)
            .snapshot
            .profiles
            .get(&self.pubkey)
            .cloned()
    }

    /// Asks the session for the profile, and gives it a while to answer.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.settled = self.profile(cx).is_some();
        self.session(cx).want_profiles(vec![self.pubkey.clone()]);
        self.load_agent(cx);
        self._wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROFILE_WAIT).await;
            let _ = this.update(cx, |this, cx| {
                this.settled = true;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// A known agent: its profile says so, or it runs on one of the
    /// viewer's hosts (`useKnownAgentPubkeys`).
    fn known_agent(&self, cx: &App) -> bool {
        self.profile(cx).is_some_and(|p| p.is_agent)
            || crate::ui::typing::owned_agents(cx).contains(&self.pubkey)
    }

    /// The agent's owner, when its attestation verifies.
    fn verified_owner(&self, cx: &App) -> Option<String> {
        self.profile(cx)?.attested_owner.clone()
    }

    fn is_owner(&self, cx: &App) -> bool {
        self.known_agent(cx)
            && self.verified_owner(cx).as_deref() == Some(self.community.read(cx).session.viewer())
    }

    /// The name, or "Unknown agent" or the short key without a profile.
    pub fn name(&self, cx: &App) -> SharedString {
        match self.profile(cx) {
            Some(profile) => profile.name.clone().into(),
            None if self.known_agent(cx) || self.agent_hint => "Unknown agent".into(),
            None => short_key(&self.pubkey).into(),
        }
    }

    /// The viewer's agents with this key on hosts serving this community
    /// (`useRuntimeAgents`).
    fn runtime_agents(&self, cx: &App) -> Vec<(Entity<HostConnection>, Agent)> {
        community_agents(&self.community.read(cx).origin, &self.pubkey, cx)
    }

    fn can_view_runtime(&self, cx: &App) -> bool {
        self.is_owner(cx) && !self.runtime_agents(cx).is_empty()
    }

    /// The tabs shown, and the one selected: a tab that went away falls
    /// back to Info.
    fn sections(&self, cx: &App) -> (Vec<Section>, Section) {
        let mut sections = vec![Section::Info];
        if self.can_view_runtime(cx) {
            sections.push(Section::Runtime);
        }
        sections.push(Section::Channels);
        // An Agents2 agent's sections replace the harness Memories tab.
        let agents2 = crate::agents2::Agents2::global(cx)
            .is_some_and(|service| service.read(cx).find(&self.pubkey).is_some());
        if self.is_owner(cx) && !agents2 {
            sections.push(Section::Memories);
        }
        let selected = match sections.contains(&self.tab) {
            true => self.tab,
            false => Section::Info,
        };
        (sections, selected)
    }

    fn select(&mut self, section: Section, cx: &mut Context<Self>) {
        self.tab = section;
        if section == Section::Memories && self.memories == Memories::Idle {
            self.refresh_memories(cx);
        }
        cx.notify();
    }

    // ----- actions -----

    /// Whether Message is offered: someone else, and for an agent only
    /// one the viewer manages here (`messageable`).
    fn can_message(&self, cx: &App) -> bool {
        let viewer = self.community.read(cx).session.viewer().to_string();
        let agent = self.known_agent(cx) || self.agent_hint;
        viewer != self.pubkey
            && (!agent || crate::ui::typing::owned_agents(cx).contains(&self.pubkey))
    }

    fn open_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening_message || !self.can_message(cx) {
            return;
        }
        self.opening_message = true;
        self.message_error = None;
        let session = self.session(cx);
        let pubkey = self.pubkey.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.open_dm(vec![pubkey]).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.opening_message = false;
                match result {
                    Ok(id) => crate::ui::navigation::open_conversation(id, window, cx),
                    Err(error) => this.message_error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn copy_key(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(npub(&self.pubkey)));
        self.copy_status = Some("Public key copied.".into());
        cx.notify();
    }

    fn copy_value(&mut self, label: &'static str, value: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(value));
        self.copied = Some(label);
        super::notice::success(format!("Copied {}", label.to_lowercase()), cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.copied == Some(label) {
                    this.copied = None;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Reads the memories again (`view.refresh`): blocked while live
    /// updates are down.
    fn refresh_memories(&mut self, cx: &mut Context<Self>) {
        if self.memories == Memories::Loading {
            return;
        }
        if !self.community.read(cx).snapshot.live {
            self.memories = Memories::Blocked;
            cx.notify();
            return;
        }
        self.memory_generation += 1;
        let generation = self.memory_generation;
        self.memories = Memories::Loading;
        let session = self.session(cx);
        let agent = self.pubkey.clone();
        cx.spawn(async move |this, cx| {
            let result = session.agent_memories(&agent).await;
            let _ = this.update(cx, |this, cx| {
                if this.memory_generation != generation {
                    return;
                }
                this.memories = match result {
                    Ok(_) if !this.community.read(cx).snapshot.live => Memories::Blocked,
                    Ok(listing) => Memories::Ready(listing),
                    Err(MemoryError::Denied) => Memories::Denied,
                    Err(MemoryError::Failed(_)) => Memories::Error,
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Adding is offered only for an agent the viewer manages on a ready
    /// host serving this community, with the channel list loaded.
    fn can_add_to_channel(&self, cx: &App) -> bool {
        let roster = matches!(
            self.community.read(cx).snapshot.roster,
            RosterStatus::Ready | RosterStatus::Error(_)
        );
        roster
            && self
                .runtime_agents(cx)
                .iter()
                .any(|(host, _)| host.read(cx).status() == crate::hosts::control::Status::Ready)
    }

    /// Streams and forums the viewer may add the agent to: listed, writable,
    /// and without it.
    fn channel_candidates(&self, cx: &App) -> Vec<buzz_relay_client::ChannelSummary> {
        self.community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .filter(|channel| {
                !channel.archived
                    && !channel.hidden
                    && !channel.read_only
                    && !channel.cached
                    && matches!(
                        channel.channel_type,
                        Some(ChannelType::Stream | ChannelType::Forum)
                    )
                    && !channel.members.contains(&self.pubkey)
            })
            .cloned()
            .collect()
    }

    /// Adds the agent to the chosen channel and starts it, as the members
    /// dialog does. The choice is checked again after the host refresh.
    fn add_to_channel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.add_channel else {
            return;
        };
        let Some(channel) = dialog.selected.clone() else {
            return;
        };
        if dialog.pending {
            return;
        }
        dialog.pending = true;
        dialog.error = None;
        let hosts: Vec<_> = self
            .runtime_agents(cx)
            .into_iter()
            .map(|(host, _)| host)
            .collect();
        let community = self.community.clone();
        let session = self.session(cx);
        let pubkey = self.pubkey.clone();
        cx.spawn_in(window, async move |this, cx| {
            for host in hosts {
                host.update(cx, |host, cx| host.refresh_status(cx)).await;
            }
            let eligible = this
                .update(cx, |this, cx| {
                    this.can_add_to_channel(cx)
                        && this.channel_candidates(cx).iter().any(|c| c.id == channel)
                })
                .unwrap_or(false);
            let result = match eligible {
                false => {
                    Err("Channel addition cancelled. Check membership before retrying.".to_string())
                }
                true => match session.add_member(&channel, &pubkey).await {
                    Ok(()) => {
                        super::members::start_added_agent(&community, &channel, &pubkey, false, cx)
                            .await
                    }
                    Err(error) => Err(error),
                },
            };
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.add_channel = None,
                    Err(error) => {
                        if let Some(dialog) = &mut this.add_channel {
                            dialog.pending = false;
                            dialog.error = Some(error.into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn add_channel_dialog(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.add_channel.clone()?;
        let c = *cx.colors();
        let available = self.can_add_to_channel(cx);
        let candidates = match available {
            true => self.channel_candidates(cx),
            false => Vec::new(),
        };
        let selected = dialog
            .selected
            .as_ref()
            .and_then(|id| candidates.iter().find(|c| &c.id == id))
            .map(|channel| format!("#{}", channel.name));
        let roster_ready = self.community.read(cx).snapshot.roster == RosterStatus::Ready;
        let host_error = self
            .runtime_agents(cx)
            .iter()
            .any(|(host, _)| host.read(cx).status() == crate::hosts::control::Status::Error);
        let mut field = div().relative().w_full().child(
            buzz_ui::Select::new("profile-add-channel")
                .placeholder("Choose a channel")
                .when_some(selected.clone(), |select, value| select.value(value))
                .open(dialog.menu)
                .disabled(dialog.pending || !available)
                .surface(buzz_ui::Surface::Floating)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(dialog) = &mut this.add_channel {
                        dialog.menu = !dialog.menu;
                    }
                    cx.notify();
                })),
        );
        if dialog.menu {
            let items = candidates.iter().map(|channel| {
                let id = channel.id.clone();
                buzz_ui::MenuItem::new(
                    SharedString::from(format!("add-channel-{}", channel.id)),
                    format!("#{}", channel.name),
                )
                .checked(dialog.selected.as_deref() == Some(channel.id.as_str()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(dialog) = &mut this.add_channel {
                        dialog.selected = Some(id.clone());
                        dialog.menu = false;
                        dialog.error = None;
                    }
                    cx.notify();
                }))
            });
            let weak = cx.entity().downgrade();
            field = field.child(super::overlay::dropdown(
                window,
                super::overlay::Align::Start,
                div()
                    .min_w(rems(16.))
                    .child(buzz_ui::Menu::new().children(items)),
                std::rc::Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        if let Some(dialog) = &mut this.add_channel {
                            dialog.menu = false;
                        }
                        cx.notify();
                    });
                }),
            ));
        }
        let alert = |text: SharedString| div().text_color(c.text_danger).child(text);
        let content = buzz_ui::Dialog::new("profile-add-channel-dialog", "Add agent to channel")
            .description(
                "Choose a channel to add this managed agent. The relay must authorize membership.",
            )
            .on_close(cx.listener(|this, _, _, cx| {
                if !this.add_channel.as_ref().is_some_and(|d| d.pending) {
                    this.add_channel = None;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S3)
                    .child(div().text_label().child("Channel"))
                    .child(field)
                    .when(!roster_ready, |this| {
                        this.child(
                            "Channel discovery is incomplete; only loaded channels are offered.",
                        )
                    })
                    .when(host_error, |this| {
                        let hosts: Vec<_> = self
                            .runtime_agents(cx)
                            .into_iter()
                            .map(|(host, _)| host)
                            .collect();
                        this.child(alert(
                            "Could not refresh local agents. Retry to get current host status."
                                .into(),
                        ))
                        .child(
                            div().child(
                                Button::new("profile-add-retry-agents", "Retry agents")
                                    .size(ButtonSize::Sm)
                                    .disabled(dialog.pending)
                                    .on_click(move |_, _, cx| {
                                        for host in &hosts {
                                            host.update(cx, |host, cx| {
                                                host.refresh_status(cx).detach()
                                            });
                                        }
                                    }),
                            ),
                        )
                    })
                    .when(!available && !host_error && !dialog.pending, |this| {
                        this.child(alert(
                            "This agent or channel is no longer eligible. Check its status before \
                             adding again."
                                .into(),
                        ))
                    })
                    .when(available && candidates.is_empty(), |this| {
                        this.child("No eligible channels in the loaded list.")
                    })
                    .children(dialog.error.clone().map(alert)),
            )
            .footer(
                Button::new("profile-add-cancel", "Cancel")
                    .variant(ButtonVariant::Ghost)
                    .disabled(dialog.pending)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_channel = None;
                        cx.notify();
                    })),
            )
            .footer(
                Button::new(
                    "profile-add-submit",
                    if dialog.pending {
                        "Adding…"
                    } else {
                        "Add to channel"
                    },
                )
                .disabled(dialog.pending || selected.is_none() || !available)
                .on_click(cx.listener(|this, _, window, cx| this.add_to_channel(window, cx))),
            );
        Some(super::overlay::modal(window, content, None).into_any_element())
    }

    fn set_start_on_launch(
        &mut self,
        host: Entity<HostConnection>,
        agent: Agent,
        value: bool,
        cx: &mut Context<Self>,
    ) {
        self.runtime_notice = None;
        let task = host.update(cx, |host, cx| {
            host.write(
                Request::SetStartOnLaunch {
                    id: agent.id.clone(),
                    value,
                },
                Default::default(),
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.runtime_notice = Some(match result {
                    Ok(_) if value => (
                        format!(
                            "Will start {} automatically when the desktop app opens.",
                            agent.name
                        )
                        .into(),
                        false,
                    ),
                    Ok(_) => (
                        format!("{} will stay manual-start only.", agent.name).into(),
                        false,
                    ),
                    Err(_) => ("Failed to update startup preference.".into(), true),
                });
                cx.notify();
            });
        })
        .detach();
    }

    // ----- rendering -----

    fn identity(&self, cx: &App) -> AnyElement {
        let snapshot = self.community.read(cx).snapshot.clone();
        let media = self.community.read(cx).session.media().clone();
        let name = self.name(cx);
        let presence = snapshot.presence.get(&self.pubkey).copied();
        let mut avatar = Avatar::new(name.clone())
            .size(AvatarSize::Px(80.))
            .presence(presence.map(|state| match state {
                PresenceState::Online => Presence::Online,
                PresenceState::Away => Presence::Away,
                PresenceState::Offline => Presence::Offline,
            }));
        if self.known_agent(cx) || self.agent_hint {
            avatar = avatar.squircle();
        }
        if let Some(picture) = self.profile(cx).and_then(|p| p.picture.clone()) {
            avatar = avatar.image(media::avatar(&picture, &media));
        }
        div()
            .flex()
            .items_center()
            .gap(space::S4)
            .min_w_0()
            .child(div().flex_none().child(avatar))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().text_heading().child(name))
                    .children(presence.map(|state| {
                        div()
                            .text_body_sm()
                            .text_color(cx.colors().text_subtle)
                            .child(presence_label(state))
                    })),
            )
            .into_any_element()
    }

    fn info(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = *cx.colors();
        let profile = self.profile(cx);
        let snapshot = self.community.read(cx).snapshot.clone();
        let mut items: Vec<AnyElement> = Vec::new();
        items.extend(crate::ui::status::display(
            snapshot.statuses.get(&self.pubkey),
            &self.community,
            cx,
        ));
        if self.can_message(cx) {
            items.push(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(space::S2)
                    .child(
                        Button::new("profile-message", "Message")
                            .size(ButtonSize::Sm)
                            .loading(self.opening_message)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_message(window, cx)),
                            ),
                    )
                    .children(
                        self.message_error
                            .clone()
                            .map(|error| div().text_color(c.text_danger).child(error)),
                    )
                    .into_any_element(),
            );
        }
        if let Some(about) = profile
            .as_ref()
            .and_then(|p| p.about.clone())
            .filter(|about| !about.is_empty())
        {
            items.push(div().child(about).into_any_element());
        }
        // The local agent, its actions, archive and delete
        // (`ProfileAgentRuntime`, `ProfileAgentActions`, `ProfileAgentArchive`).
        items.extend(self.agent_runtime_items(window, cx));
        // An Agents2 agent's sections and Delete, then a known agent's
        // latest activity (`ProfileActivity`).
        items.extend(crate::ui::agents2::profile_sections(
            &self.pubkey,
            window,
            cx,
        ));
        if self.known_agent(cx) {
            items.extend(crate::ui::agent_activity::profile::latest_activity(
                &self.community,
                &self.pubkey,
                self.context.as_deref(),
                cx,
            ));
        }
        if !self.can_view_runtime(cx) {
            items.extend(self.instances(cx));
        }
        items.push(
            div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(space::S2)
                        .child(
                            div()
                                .text_body()
                                .text_color(c.text_subtle)
                                .child("Public key"),
                        )
                        .child(
                            Button::new("profile-copy-npub", "Copy")
                                .size(ButtonSize::Sm)
                                .variant(ButtonVariant::Ghost)
                                .icon(IconName::Copy)
                                .on_click(cx.listener(|this, _, _, cx| this.copy_key(cx))),
                        ),
                )
                .child(
                    div()
                        .p(space::S3)
                        .rounded(radius::CONTROL)
                        .bg(c.surface_inset)
                        .text_mono()
                        .child(npub(&self.pubkey)),
                )
                .children(
                    self.copy_status
                        .clone()
                        .map(|status| div().text_color(c.text_subtle).child(status)),
                )
                .into_any_element(),
        );
        items.extend(self.agent_identity_items(cx));
        if let Some(nip05) = profile.as_ref().and_then(|p| p.nip05.clone()) {
            items.push(self.copy_field("NIP-05", nip05, cx));
        }
        items.extend(self.agent_metadata_items(cx));
        if profile.is_some() && self.metadata_failed() {
            items.push(
                div()
                    .child(
                        Button::new("profile-retry-metadata", "Retry profile")
                            .size(ButtonSize::Sm)
                            .on_click(cx.listener(|this, _, _, cx| this.retry_metadata(cx))),
                    )
                    .into_any_element(),
            );
        }
        if profile.is_none() {
            let (note, retry) = match self.settled {
                false => ("Loading profile…", false),
                true => ("No profile metadata is available in this community.", true),
            };
            items.push(div().child(note).into_any_element());
            if retry {
                items.push(
                    div()
                        .child(
                            Button::new("profile-retry", "Retry profile")
                                .size(ButtonSize::Sm)
                                .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                        )
                        .into_any_element(),
                );
            }
        }
        items
    }

    /// A field that copies its value (`ProfileCopyField`).
    fn copy_field(&self, label: &'static str, value: String, cx: &mut Context<Self>) -> AnyElement {
        self.copy_field_shown(label, value.clone(), value, cx)
    }

    /// [`Self::copy_field`] showing `display` for the value it copies.
    fn copy_field_shown(
        &self,
        label: &'static str,
        value: String,
        display: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let copied = self.copied == Some(label);
        let copy = value;
        let value = display;
        div()
            .id(SharedString::from(format!("profile-copy-{label}")))
            .group("profile-copy-field")
            .flex()
            .items_center()
            .gap(space::S3)
            .p(space::S4)
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::CONTROL)
            .bg(c.surface_panel)
            .cursor_pointer()
            .hover(|this| this.bg(c.surface_inset))
            .tooltip(buzz_ui::Tooltip::text(format!("Copy {label}")))
            .on_click(cx.listener(move |this, _, _, cx| this.copy_value(label, copy.clone(), cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(space::S2)
                    .min_w_0()
                    .child(div().text_body().child(label))
                    .child(div().truncate().text_color(c.text_subtle).child(value)),
            )
            .child(
                div()
                    .flex_none()
                    .invisible()
                    .when(copied, |this| this.visible())
                    .group_hover("profile-copy-field", |this| this.visible())
                    .child(
                        Icon::new(if copied {
                            IconName::Check
                        } else {
                            IconName::Copy
                        })
                        .size(16.),
                    ),
            )
            .into_any_element()
    }

    fn channels(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = *cx.colors();
        let snapshot = self.community.read(cx).snapshot.clone();
        let note = |text: &'static str| div().p(space::S4).child(text).into_any_element();
        let listed = matches!(
            snapshot.roster,
            RosterStatus::Ready | RosterStatus::Error(_)
        );
        let visible = |channel: &&buzz_relay_client::ChannelSummary| {
            !channel.archived && !channel.hidden && channel.members.contains(&self.pubkey)
        };
        let channels: Vec<_> = snapshot
            .channels
            .iter()
            .filter(visible)
            .filter(|channel| {
                matches!(
                    channel.channel_type,
                    Some(ChannelType::Stream | ChannelType::Forum)
                )
            })
            .cloned()
            .collect();
        let unclassified = listed
            && snapshot
                .channels
                .iter()
                .filter(visible)
                .any(|channel| channel.channel_type.is_none());
        let mut items = Vec::new();
        if self.can_add_to_channel(cx) {
            items.push(
                Button::new("profile-add-to-channel", "Add to channel")
                    .variant(ButtonVariant::Ghost)
                    .full_width()
                    .icon_end(IconName::CaretRight)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.add_channel = Some(AddChannel::default());
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        match &snapshot.roster {
            RosterStatus::Loading => items.push(note("Loading channels…")),
            RosterStatus::Error(_) => items.push(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(space::S2)
                    .p(space::S4)
                    .child("Could not finish loading channels.")
                    .child(
                        Button::new("profile-retry-channels", "Retry channels")
                            .size(ButtonSize::Sm)
                            .on_click(cx.listener(|this, _, _, cx| this.session(cx).visible())),
                    )
                    .into_any_element(),
            ),
            RosterStatus::Ready => {}
        }
        if unclassified {
            items.push(
                div()
                    .p(space::S4)
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("Some memberships for this identity are unclassified and omitted.")
                    .into_any_element(),
            );
        }
        if snapshot.roster == RosterStatus::Ready && channels.is_empty() {
            items.push(note(if unclassified {
                "No matching channels with a known visible type; other memberships could not \
                 be classified."
            } else {
                "No visible channels with verified membership for this identity."
            }));
        }
        if !channels.is_empty() {
            items.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(space::S1)
                    .children(channels.iter().enumerate().map(|(index, channel)| {
                        let id = channel.id.clone();
                        NavigationItem::new(
                            SharedString::from(format!("profile-channel-{index}")),
                            channel.name.clone(),
                        )
                        .icon(channel_icon(channel))
                        .on_click(move |_, window, cx| {
                            crate::ui::navigation::open_conversation(id.clone(), window, cx)
                        })
                    }))
                    .into_any_element(),
            );
        }
        items
    }

    fn memories(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = *cx.colors();
        let text = |text: &'static str| div().child(text).into_any_element();
        let mut items = Vec::new();
        match &self.memories {
            Memories::Idle => items.push(text(
                "Memory data was cleared. Retry to load memories again.",
            )),
            Memories::Loading => items.push(text("Loading memories…")),
            Memories::Denied => items.push(text("You don’t have access to these memories.")),
            Memories::Blocked => items.push(text(
                "Memory reads are paused while live updates are disconnected or session data is \
                 being cleared. Retry after live updates reconnect or clearing finishes.",
            )),
            Memories::Error => items.push(text(
                "Couldn’t load memories. Check your connection and try again.",
            )),
            Memories::Ready(listing) => {
                if listing.partial {
                    items.push(text(
                        "This snapshot may be incomplete. Some records were invalid or the read \
                         limit was reached.",
                    ));
                }
                if listing.entries.is_empty() {
                    items.push(text(if listing.partial {
                        "No readable entries in this partial snapshot."
                    } else {
                        "No memories were returned for your account in this community."
                    }));
                }
                for (index, entry) in listing.entries.iter().enumerate() {
                    let open = self.open_memories.contains(&entry.slug);
                    let slug = entry.slug.clone();
                    let label = match entry.slug.as_str() {
                        "core" => "Core memory".to_string(),
                        other => other.to_string(),
                    };
                    items.push(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::S2)
                            .min_w_0()
                            .child(
                                div()
                                    .id(SharedString::from(format!("memory-{index}")))
                                    .flex()
                                    .items_center()
                                    .gap(space::S1)
                                    .cursor_pointer()
                                    .text_mono()
                                    .child(
                                        Icon::new(if open {
                                            IconName::CaretDown
                                        } else {
                                            IconName::CaretRight
                                        })
                                        .size(12.),
                                    )
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.open_memories.remove(&slug) {
                                            this.open_memories.insert(slug.clone());
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(open, |this| {
                                this.child(div().text_body_sm().text_color(c.text_subtle).child(
                                    format!(
                                        "Updated {}",
                                        crate::ui::time::locale_string(entry.created_at)
                                    ),
                                ))
                                .child(
                                    div()
                                        .p(space::S3)
                                        .rounded(radius::CONTROL)
                                        .bg(c.surface_inset)
                                        .text_mono()
                                        .whitespace_normal()
                                        .child(entry.body.clone()),
                                )
                            })
                            .into_any_element(),
                    );
                }
            }
        }
        if matches!(
            self.memories,
            Memories::Error | Memories::Denied | Memories::Blocked | Memories::Idle
        ) {
            items.push(
                div()
                    .child(
                        Button::new("profile-retry-memories", "Retry memories")
                            .size(ButtonSize::Sm)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_memories(cx))),
                    )
                    .into_any_element(),
            );
        }
        items
    }

    fn runtime(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = *cx.colors();
        let Some((host, agent)) = self.exact_agent(cx) else {
            return self.instances(cx).into_iter().collect();
        };
        let (status, busy, runtime_available, runtime_message, error) = {
            let host = host.read(cx);
            (
                host.status(),
                host.control.busy,
                host.agents.runtime_available,
                host.agents.runtime_message.clone(),
                host.control.error.clone(),
            )
        };
        let ready = status == crate::hosts::control::Status::Ready;
        let section = |title: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(space::S3)
                .child(div().text_body().child(title))
        };
        let rows = |rows: Vec<(&'static str, String)>| {
            div()
                .flex()
                .flex_col()
                .gap(space::S3)
                .children(rows.into_iter().map(|(label, value)| {
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().text_body_sm().text_color(c.text_subtle).child(label))
                        .child(div().text_mono().child(value))
                }))
        };
        let mut items = Vec::new();
        if !agent.restart_diff.is_empty() {
            items.push(
                section("Restart required")
                    .child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(AUTO_RESTART_OFF_BLURB),
                    )
                    .children(agent.restart_diff.iter().map(|entry| {
                        let change = entry.change.as_ref().map(agent::diff_change);
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(space::S1)
                            .text_body_sm()
                            .child(format!("{}:", field_label(&entry.field)))
                            .children(change.map(|(before, after)| {
                                div()
                                    .flex()
                                    .gap(space::S1)
                                    .children(
                                        before.map(|before| div().line_through().child(before)),
                                    )
                                    .when(
                                        entry.change.as_ref().is_some_and(|change| {
                                            matches!(
                                                change,
                                                buzz_host::api::RestartChange::Value { .. }
                                                    | buzz_host::api::RestartChange::Masked { .. }
                                            )
                                        }),
                                        |this| this.child("→"),
                                    )
                                    .child(after)
                            }))
                    }))
                    .into_any_element(),
            );
        }
        let launch = !agent.start_on_app_launch;
        let toggle = {
            let (host, agent) = (host.clone(), agent.clone());
            cx.listener(move |this, _: &bool, _, cx| {
                this.set_start_on_launch(host.clone(), agent.clone(), launch, cx)
            })
        };
        items.push(
            section("Activity")
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .border_1()
                        .border_color(c.border_standard)
                        .rounded(radius::CONTROL)
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .p(space::S4)
                                .child("Status")
                                .child(status_label(agent.status)),
                        )
                        .child(
                            div()
                                .p(space::S4)
                                .border_t_1()
                                .border_color(c.border_standard)
                                .child(
                                    Switch::new(
                                        "profile-start-on-launch",
                                        agent.start_on_app_launch,
                                    )
                                    .label("Start on launch")
                                    .disabled(busy || !ready)
                                    .on_change(move |value, window, cx| toggle(&value, window, cx)),
                                ),
                        )
                        .child(
                            div()
                                .border_t_1()
                                .border_color(c.border_standard)
                                .child(self.log_entry(&agent, ready, cx)),
                        ),
                )
                .into_any_element(),
        );
        if status == crate::hosts::control::Status::Error {
            let host = host.clone();
            items.push(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(space::S2)
                    .children(error.map(|error| div().text_color(c.text_danger).child(error)))
                    .child(
                        Button::new("profile-retry-status", "Retry status")
                            .size(ButtonSize::Sm)
                            .disabled(busy)
                            .on_click(move |_, _, cx| {
                                host.update(cx, |host, cx| host.refresh_status(cx).detach())
                            }),
                    )
                    .into_any_element(),
            );
        }
        if !runtime_available {
            items.push(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(
                        runtime_message
                            .filter(|message| !message.is_empty())
                            .unwrap_or_else(|| "The bundled agent runtime is unavailable.".into()),
                    )
                    .into_any_element(),
            );
        }
        let owner = self.verified_owner(cx).and_then(|owner| {
            self.community
                .read(cx)
                .snapshot
                .profiles
                .get(&owner)
                .map(|p| p.name.clone())
        });
        let configuration: Vec<(&'static str, String)> = [
            ("Runtime", Some(agent.harness.command.clone())),
            (
                "Who can send instructions",
                respond_to_label(agent.respond_to.as_deref(), owner.as_deref()),
            ),
            ("ACP command", agent.acp_command.clone()),
            ("MCP command", agent.mcp_command.clone()),
            ("Backend", agent.backend.clone()),
        ]
        .into_iter()
        .filter_map(|(label, value)| Some((label, value.filter(|v| !v.is_empty())?)))
        .collect();
        if !configuration.is_empty() {
            items.push(
                section("Agent configuration")
                    .child(rows(configuration))
                    .into_any_element(),
            );
        }
        items.push(self.model_settings(&agent, cx));
        items.extend(self.instances(cx));
        let databricks = agent.harness.databricks.clone().unwrap_or_default();
        let advanced: Vec<(&'static str, String)> = [
            ("Workspace", agent.workspace.clone()),
            ("Environment", agent.harness.environment_keys.join(", ")),
            ("Databricks workspace (HTTPS origin)", databricks.host),
            ("Model filter (optional)", databricks.filter),
        ]
        .into_iter()
        .filter(|(_, value)| !value.is_empty())
        .collect();
        if !advanced.is_empty() {
            items.push(section("Advanced").child(rows(advanced)).into_any_element());
        }
        if let Some((notice, error)) = &self.runtime_notice {
            items.push(
                div()
                    .text_body_sm()
                    .text_color(if *error {
                        c.text_danger
                    } else {
                        c.text_success
                    })
                    .child(notice.clone())
                    .into_any_element(),
            );
        }
        items
    }
}

impl Focusable for ProfilePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ProfilePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let (sections, selected) = self.sections(cx);
        let add_channel = self.add_channel_dialog(window, cx);
        let mut tabs = Tabs::new(TabVariant::Panel);
        for section in sections {
            let (id, label) = match section {
                Section::Info => ("profile-tab-info", "Info"),
                Section::Runtime => ("profile-tab-runtime", "Runtime"),
                Section::Channels => ("profile-tab-channels", "Channels"),
                Section::Memories => ("profile-tab-memories", "Memories"),
            };
            tabs = tabs.tab(
                Tab::new(id, label)
                    .selected(section == selected)
                    .on_click(cx.listener(move |this, _, _, cx| this.select(section, cx))),
            );
        }
        let instance = self.instance_header(cx);
        let content = match selected {
            _ if instance.as_ref().is_some_and(|(_, valid)| !valid) => Vec::new(),
            Section::Info => self.info(window, cx),
            Section::Runtime => self.runtime(cx),
            Section::Channels => self.channels(cx),
            Section::Memories => self.memories(cx),
        };
        let editor = self.editor_view(window);
        let dialogs = self.agent_dialogs(window, cx);
        let log = self.log_view();
        let close = self.close.clone();
        div()
            .id("profile-panel")
            .key_context("ProfilePanel")
            .track_focus(&self.focus)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape"
                    && let Some(close) = &close
                {
                    cx.stop_propagation();
                    close(window, cx);
                }
            })
            .flex()
            .flex_col()
            .gap(space::S6)
            .p(space::S5)
            .size_full()
            .min_w_0()
            .overflow_y_scroll()
            .bg(c.surface_panel)
            .text_body()
            .text_color(c.text_standard)
            .children(instance.map(|(header, _)| header))
            .map(|panel| match log {
                // The log replaces the profile while it is open.
                Some(log) => panel.child(log),
                None => panel.child(self.identity(cx)).child(
                    div().flex().flex_col().child(tabs).child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(space::S6)
                            .pt(space::S4)
                            .min_w_0()
                            .children(content),
                    ),
                ),
            })
            .children(add_channel)
            .children(editor)
            .children(dialogs)
    }
}

/// The viewer's agents with key `pubkey` on hosts serving the community at
/// `origin` (`sameCommunityAgents`).
pub fn community_agents(
    origin: &RelayOrigin,
    pubkey: &str,
    cx: &App,
) -> Vec<(Entity<HostConnection>, Agent)> {
    let community = origin.http();
    let Some(hosts) = cx.try_global::<Hosts>() else {
        return Vec::new();
    };
    hosts
        .hosts
        .iter()
        .flat_map(|host| {
            host.read(cx)
                .agents
                .agents
                .iter()
                .filter(|agent| {
                    agent.pubkey == pubkey
                        && RelayOrigin::parse(&agent.relay_url)
                            .is_ok_and(|origin| origin.http() == community)
                })
                .map(|agent| (host.clone(), agent.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// "Active", "Away" or "Offline".
pub fn presence_label(state: PresenceState) -> &'static str {
    match state {
        PresenceState::Online => "Active",
        PresenceState::Away => "Away",
        PresenceState::Offline => "Offline",
    }
}

/// The full `npub1…`, or the key itself if it is not one.
fn npub(pubkey: &str) -> String {
    PublicKey::from_hex(pubkey)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .unwrap_or_else(|| pubkey.to_string())
}

/// `formatPublicKey`: `npub1abcd…wxyz`.
fn short_key(pubkey: &str) -> String {
    let npub = npub(pubkey);
    match npub.starts_with("npub1") && npub.len() > 16 {
        true => format!("{}…{}", &npub[..10], &npub[npub.len() - 4..]),
        false => npub,
    }
}

fn status_label(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Stopped => "Stopped",
        ProcessStatus::Waiting => "Waiting to start · unlock Keychain if prompted",
        ProcessStatus::Starting => "Starting",
        ProcessStatus::Running => "Running",
        ProcessStatus::Stopping => "Stopping",
        ProcessStatus::Failed => "Failed",
    }
}

fn respond_to_label(respond_to: Option<&str>, owner: Option<&str>) -> Option<String> {
    Some(match respond_to? {
        "owner-only" => match owner {
            Some(owner) => format!("Only {owner} (owner)"),
            None => "Only the owner".into(),
        },
        "allowlist" => "Selected people".into(),
        "anyone" => "Anyone".into(),
        _ => return None,
    })
}

/// Base Buzz's itemized list: `snake_case` → "Snake case", `env.FOO` →
/// "FOO (env)".
fn field_label(field: &str) -> String {
    if let Some(name) = field.strip_prefix("env.") {
        return format!("{name} (env)");
    }
    let spaced = field.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Opens `pubkey`'s profile as its own companion panel.
pub fn view(
    community: Entity<Community>,
    pubkey: String,
    close: channel_panels::Close,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ProfilePanel> {
    cx.new(|cx| {
        let mut panel = ProfilePanel::new(community, pubkey, false, window, cx);
        panel.close = Some(close);
        panel
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn profile_targets_round_trip() {
        let key = buzz_identity::Identity::ephemeral().public_key().to_hex();
        let person = target(&key, false).unwrap();
        assert!(person.starts_with("nostr:npub1"));
        assert_eq!(parse_target(&person), Some((key.clone(), false)));
        let agent = target(&key, true).unwrap();
        assert_eq!(agent, format!("buzz:agent-profile:{key}"));
        assert_eq!(parse_target(&agent), Some((key.clone(), true)));
        assert_eq!(parse_target("nostr:nsec1abc"), None);
        assert_eq!(parse_target("buzz:agent-profile:XYZ"), None);
        assert_eq!(parse_target("https://example.com"), None);
        assert_eq!(target("nothex", false), None);
    }

    #[test]
    fn labels_follow_buzz_app() {
        assert_eq!(field_label("launch_model"), "Launch model");
        assert_eq!(field_label("env.OPENAI_KEY"), "OPENAI_KEY (env)");
        assert_eq!(
            respond_to_label(Some("owner-only"), Some("Alice")).as_deref(),
            Some("Only Alice (owner)")
        );
        assert_eq!(
            respond_to_label(Some("owner-only"), None).as_deref(),
            Some("Only the owner")
        );
        assert_eq!(respond_to_label(Some("other"), None), None);
        assert_eq!(presence_label(PresenceState::Online), "Active");
        assert_eq!(status_label(ProcessStatus::Running), "Running");
    }

    #[test]
    fn short_keys_keep_both_ends() {
        let key = buzz_identity::Identity::ephemeral().public_key().to_hex();
        let short = short_key(&key);
        assert!(short.starts_with("npub1") && short.contains('…'));
        assert_eq!(short.chars().count(), 15);
    }
}
