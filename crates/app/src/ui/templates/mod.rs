//! Templates & teams (`bundled/channel-templates`, plugin
//! `buzz.channel-templates`): the viewer's private channel templates and
//! saved agent teams, kept in the channel kit.
//!
//! The plugin contributes the community's "Templates" Settings card, the
//! template selector in "Create a channel", "Save as template…" in channel
//! settings and the teams on the Agents page. buzz-app ships it off by
//! default; every contribution here is keyed by the plugin id so the
//! Plugins switches gate it.

mod dialog;
mod editor;
mod fields;
mod library;
mod owner;
mod save_as;

pub use editor::{Changed, GroupDefault, TemplateEditor, setup_summary};
pub use save_as::SaveAsTemplate;

use crate::community::{AppState, Community};
use crate::hosts::Hosts;
use crate::ui::settings::cards::{self, CardPlacement, SettingsCard};
use crate::ui::settings::form::{self, FieldStyle};
use crate::ui::toasts;
use buzz_relay_client::kit::{AgentChoice, KitSnapshot, KitStatus};
use buzz_relay_client::{ChannelType, RelayOrigin, RosterStatus};
use buzz_ui::{ActiveTheme, Icon, IconName, ToastTone, TypeStyled, tokens::radius, tokens::space};
use gpui_kit::*;
use nostr::nips::nip19::ToBech32;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

/// The template provider's contribution key (`channelTemplates.register`);
/// the text before the slash is the plugin id the Plugins switches gate.
pub const PROVIDER: &str = "buzz.channel-templates/templates";

/// One template provider (`TemplateProvider`). With none, or more than
/// one, active, no optional template controls show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemplateProvider {
    pub key: &'static str,
    pub title: &'static str,
}

#[derive(Default)]
struct Providers(Vec<TemplateProvider>);

impl Global for Providers {}

/// Adds a provider, replacing one with the same key.
pub fn register_provider(provider: TemplateProvider, cx: &mut App) {
    let providers = &mut cx.default_global::<Providers>().0;
    providers.retain(|existing| existing.key != provider.key);
    providers.push(provider);
}

/// The one active provider, if exactly one is registered.
pub fn provider(cx: &App) -> Option<TemplateProvider> {
    match cx.try_global::<Providers>().map(|p| p.0.as_slice()) {
        // Off while Templates & teams is switched off in Settings → Plugins.
        Some([provider]) if crate::plugins::enabled_key(provider.key) => Some(*provider),
        _ => None,
    }
}

/// Registers the Templates card and the template provider, and raises
/// channel setup notices.
pub fn init(cx: &mut App) {
    cards::register(
        SettingsCard {
            key: "buzz.channel-templates/templates",
            title: "Templates",
            icon: Some(IconName::Copy),
            placement: CardPlacement::Community,
            visibility: None,
            build: |_, cx| cx.new(library::TemplatesCard::new).into(),
        },
        cx,
    );
    register_provider(
        TemplateProvider {
            key: PROVIDER,
            title: "Templates & teams",
        },
        cx,
    );
    setup_notices(cx);
}

/// The Retry notice of a creation that may already exist.
pub fn pending_notice(cx: &App) -> Div {
    line(
        "This attempt may already have created a channel. Retry checks it first and, if unconfirmed, resends only the same creation request. It will not create another channel or continue template setup.",
        false,
        cx,
    )
    .text_body()
}

/// "Channel setup couldn’t finish" notices (`ChannelSidebar.tsx`), raised
/// as warning toasts for every community. Dismissing one only hides it.
fn setup_notices(cx: &mut App) {
    let watched: std::rc::Rc<std::cell::RefCell<HashSet<EntityId>>> = Default::default();
    let follow = move |cx: &mut App| {
        let Some(state) = cx.try_global::<AppState>() else {
            return;
        };
        for community in state.communities.clone() {
            if !watched.borrow_mut().insert(community.entity_id()) {
                continue;
            }
            let session = community.read(cx).session.clone();
            let mut creation = session.creation();
            let weak = community.downgrade();
            cx.spawn(async move |cx| {
                let mut shown: HashSet<String> = HashSet::new();
                loop {
                    let notices = creation.borrow_and_update().notices.clone();
                    let alive = cx.update(|cx| {
                        if weak.upgrade().is_none() {
                            return false;
                        }
                        for notice in &notices {
                            if !shown.insert(notice.id.clone()) {
                                continue;
                            }
                            let key = SharedString::from(format!("channel-setup:{}", notice.id));
                            let (id, session, close) =
                                (notice.id.clone(), session.clone(), key.clone());
                            toasts::show(
                                toasts::Notice {
                                    key,
                                    title: format!(
                                        "Channel setup couldn’t finish: {}",
                                        notice.name
                                    )
                                    .into(),
                                    description: Some(notice.error.clone().into()),
                                    tone: ToastTone::Warning,
                                    timeout: None,
                                    dismissible: false,
                                    actions: vec![toasts::Action::new("Dismiss", move |_, cx| {
                                        session.dismiss_setup_notice(&id);
                                        toasts::dismiss(&close, cx);
                                    })],
                                },
                                cx,
                            );
                        }
                        shown.retain(|id| notices.iter().any(|n| n.id == *id));
                        true
                    });
                    if !alive || creation.changed().await.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
    };
    follow(cx);
    cx.observe_global::<AppState>(follow).detach();
}

// ---------------------------------------------------------------------------
// Catalog (`useTemplateCatalog`)

/// What the template views read: the kit and the agents a recipe may name.
#[derive(Clone)]
pub(crate) struct Catalog {
    pub kit: Arc<KitSnapshot>,
    pub agents: Vec<AgentChoice>,
    /// The agent sources answered (`templates.status === "ready"`).
    pub agents_ready: bool,
    /// Every source is complete, so a copy names every agent.
    pub agents_complete: bool,
}

impl Catalog {
    pub fn read(community: &Entity<Community>, cx: &App) -> Self {
        let (agents, agents_ready, agents_complete) = AgentChoices::read(community, cx);
        Self {
            kit: community.read(cx).session.kit().borrow().clone(),
            agents,
            agents_ready,
            agents_complete,
        }
    }

    pub fn ready(&self) -> bool {
        self.kit.status == KitStatus::Ready
    }

    /// The status line shown while something is missing.
    pub fn status(&self, loading: &str) -> Option<String> {
        if self.kit.status == KitStatus::Ready && self.agents_ready {
            return None;
        }
        Some(match &self.kit.error {
            Some(error) => error.clone(),
            None if self.kit.status == KitStatus::Unavailable => {
                "This host does not support saved templates.".into()
            }
            None if self.kit.status != KitStatus::Ready => loading.into(),
            None => "Loading available agents…".into(),
        })
    }
}

/// [`agent_choices`] of one community, worked out again only when the
/// channels, profiles, roster or hosts change. The views that show a
/// [`Catalog`] hold it through [`watch_kit_with`].
pub(crate) struct AgentChoices {
    community: WeakEntity<Community>,
    value: (Vec<AgentChoice>, bool, bool),
    /// The snapshot parts `value` was worked out from, by identity.
    seen: Option<(usize, usize, RosterStatus)>,
    _hosts: Vec<Subscription>,
    _subscriptions: [Subscription; 2],
}

/// The live [`AgentChoices`] by community.
#[derive(Default)]
struct ChoicesHub(HashMap<EntityId, WeakEntity<AgentChoices>>);

impl Global for ChoicesHub {}

impl AgentChoices {
    /// The community's choices, shared by every view holding them.
    fn of(community: &Entity<Community>, cx: &mut App) -> Entity<Self> {
        let id = community.entity_id();
        let hub = &mut cx.default_global::<ChoicesHub>().0;
        if let Some(found) = hub.get(&id).and_then(WeakEntity::upgrade) {
            return found;
        }
        hub.retain(|_, choices| choices.upgrade().is_some());
        let choices = cx.new(|cx| {
            let subscriptions = [
                cx.observe(community, |this: &mut Self, community, cx| {
                    this.refresh(&community, false, cx)
                }),
                cx.observe_global::<Hosts>(|this, cx| {
                    this.watch_hosts(cx);
                    if let Some(community) = this.community.upgrade() {
                        this.refresh(&community, true, cx);
                    }
                }),
            ];
            let mut this = Self {
                community: community.downgrade(),
                value: Default::default(),
                seen: None,
                _hosts: Vec::new(),
                _subscriptions: subscriptions,
            };
            this.watch_hosts(cx);
            this.refresh(community, true, cx);
            this
        });
        cx.default_global::<ChoicesHub>()
            .0
            .insert(id, choices.downgrade());
        choices
    }

    /// The choices, from a live [`AgentChoices`] when a view holds one.
    fn read(community: &Entity<Community>, cx: &App) -> (Vec<AgentChoice>, bool, bool) {
        let live = cx
            .try_global::<ChoicesHub>()
            .and_then(|hub| hub.0.get(&community.entity_id()))
            .and_then(WeakEntity::upgrade);
        match live {
            Some(choices) => choices.read(cx).value.clone(),
            None => agent_choices(community, cx),
        }
    }

    fn watch_hosts(&mut self, cx: &mut Context<Self>) {
        let hosts = cx
            .try_global::<Hosts>()
            .map(|hosts| hosts.hosts.clone())
            .unwrap_or_default();
        self._hosts = hosts
            .iter()
            .map(|host| {
                cx.observe(host, |this, _, cx| {
                    if let Some(community) = this.community.upgrade() {
                        this.refresh(&community, true, cx);
                    }
                })
            })
            .collect();
    }

    /// Works the choices out again, unless (without `force`) the snapshot
    /// parts they read are the same.
    fn refresh(&mut self, community: &Entity<Community>, force: bool, cx: &mut Context<Self>) {
        let snapshot = &community.read(cx).snapshot;
        let seen = (
            Arc::as_ptr(&snapshot.channels) as *const () as usize,
            Arc::as_ptr(&snapshot.profiles) as usize,
            snapshot.roster.clone(),
        );
        if !force && self.seen.as_ref() == Some(&seen) {
            return;
        }
        self.seen = Some(seen);
        let value = agent_choices(community, cx);
        if value != self.value {
            self.value = value;
            cx.notify();
        }
    }
}

/// `templateAgentChoices`: the viewer's agents for this community, those
/// its hosts manage and relay-only ones it owns in its channels.
fn agent_choices(community: &Entity<Community>, cx: &App) -> (Vec<AgentChoice>, bool, bool) {
    let community = community.read(cx);
    let snapshot = &community.snapshot;
    let viewer = community.session.viewer().to_owned();
    let mut choices: Vec<AgentChoice> = Vec::new();
    let mut ready = true;
    let mut complete = true;
    if let Some(hosts) = cx.try_global::<Hosts>() {
        for host in &hosts.hosts {
            let host = host.read(cx);
            if !matches!(host.state, buzz_host_rpc::ConnectionState::Connected { .. }) {
                complete = false;
                if host.remote.is_none() {
                    ready = false;
                }
                continue;
            }
            for agent in &host.agents.agents {
                let here = RelayOrigin::parse(&agent.relay_url)
                    .is_ok_and(|origin| origin == community.origin);
                if !here || !agent.configured || choices.iter().any(|c| c.pubkey == agent.pubkey) {
                    continue;
                }
                let profile = snapshot.profiles.get(&agent.pubkey);
                choices.push(AgentChoice {
                    pubkey: agent.pubkey.clone(),
                    name: profile
                        .map(|p| p.name.clone())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| agent.name.clone()),
                    avatar: profile
                        .and_then(|p| p.picture.clone())
                        .or_else(|| agent.picture.clone()),
                });
            }
        }
    }
    if snapshot.roster != buzz_relay_client::RosterStatus::Ready {
        complete = false;
    }
    let members: HashSet<&String> = snapshot
        .channels
        .iter()
        .filter(|c| c.channel_type != Some(ChannelType::Dm))
        .flat_map(|c| c.members.iter())
        .collect();
    // Ownership comes from each member's signed profile: read the unknown.
    let unknown: Vec<String> = members
        .iter()
        .filter(|key| snapshot.profiles.get(key.as_str()).is_none())
        .map(|key| (*key).clone())
        .collect();
    if !unknown.is_empty() {
        complete = false;
        community.session.want_profiles(unknown);
    }
    let mut owned: Vec<AgentChoice> = members
        .into_iter()
        .filter(|key| !choices.iter().any(|c| c.pubkey == **key))
        .filter_map(|key| {
            let profile = snapshot.profiles.get(key)?;
            (profile.is_agent && profile.attested_owner.as_deref() == Some(viewer.as_str())).then(
                || AgentChoice {
                    pubkey: key.clone(),
                    name: profile.name.clone(),
                    avatar: profile.picture.clone(),
                },
            )
        })
        .collect();
    owned.sort_by(|a, b| a.name.cmp(&b.name));
    choices.extend(owned);
    (choices, ready, complete)
}

/// What [`watch_kit`] keeps running: the kit watch and the agent choices.
pub(crate) struct KitWatch {
    _task: Task<()>,
    _choices: Entity<AgentChoices>,
    _subscription: Subscription,
}

/// Re-renders `view` whenever the community's kit or agent choices
/// change, and reads the kit once (`ensure`).
pub(crate) fn watch_kit<V: 'static>(
    community: &Entity<Community>,
    cx: &mut Context<V>,
) -> KitWatch {
    watch_kit_with(community, cx, |_, cx| cx.notify())
}

/// Runs `changed` whenever the community's kit or agent choices change,
/// and reads the kit once (`ensure`).
pub(crate) fn watch_kit_with<V: 'static>(
    community: &Entity<Community>,
    cx: &mut Context<V>,
    changed: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> KitWatch {
    let changed = Rc::new(changed);
    let choices = AgentChoices::of(community, cx);
    let subscription = cx.observe(&choices, {
        let changed = changed.clone();
        move |this, _, cx| changed(this, cx)
    });
    let session = community.read(cx).session.clone();
    session.kit_ensure();
    let mut kit = session.kit();
    let mut connection = session.snapshot();
    let task = cx.spawn(async move |this, cx| {
        loop {
            // A read asked for before the relay answered waits for it.
            let (open, kit_changed) = futures::select_biased! {
                kit = futures::FutureExt::fuse(kit.changed()) => (kit.is_ok(), true),
                live = futures::FutureExt::fuse(connection.changed()) => {
                    session.kit_ensure();
                    (live.is_ok(), false)
                }
            };
            if !open || (kit_changed && this.update(cx, |this, cx| changed(this, cx)).is_err()) {
                break;
            }
        }
    });
    KitWatch {
        _task: task,
        _choices: choices,
        _subscription: subscription,
    }
}

// ---------------------------------------------------------------------------
// Shared pieces

/// `formatPublicKey`: `npub…` and the last three characters.
pub(crate) fn npub(pubkey: &str) -> String {
    nostr::prelude::PublicKey::from_hex(pubkey)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .unwrap_or_else(|| pubkey.to_owned())
}

/// `publicKeyLabels`: the shortest npub suffix, from three, that tells
/// every key apart.
pub(crate) fn public_key_labels<'a>(
    keys: impl IntoIterator<Item = &'a String>,
) -> HashMap<String, String> {
    let encoded: HashMap<String, String> = keys
        .into_iter()
        .map(|key| (key.to_ascii_lowercase(), npub(key)))
        .collect();
    let mut length = 3;
    while length < 58 {
        let suffixes: HashSet<&str> = encoded
            .values()
            .map(|npub| &npub[npub.len().saturating_sub(length)..])
            .collect();
        if suffixes.len() == encoded.len() {
            break;
        }
        length += 1;
    }
    encoded
        .into_iter()
        .map(|(key, npub)| {
            let suffix = npub[npub.len().saturating_sub(length)..].to_owned();
            (key, format!("npub…{suffix}"))
        })
        .collect()
}

/// The settings section `Header`: heading title, caption subtitle, actions.
pub(crate) fn header(
    title: &'static str,
    subtitle: Option<&'static str>,
    actions: Vec<AnyElement>,
    cx: &App,
) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .items_start()
        .gap(space::S4)
        .px(rems(1.0625))
        .mb(space::S8)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(space::S2)
                .child(div().text_heading().child(title))
                .children(
                    subtitle.map(|text| div().text_caption().text_color(c.text_subtle).child(text)),
                ),
        )
        .child(div().flex().items_center().gap(space::S2).children(actions))
}

/// `InlineHeader`: label title, caption subtitle, actions on the right.
pub(crate) fn inline_header(
    title: AnyElement,
    subtitle: Option<&'static str>,
    actions: Vec<AnyElement>,
    cx: &App,
) -> Div {
    let c = cx.colors();
    div()
        .flex()
        .items_start()
        .gap(space::S4)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(space::S2)
                .child(div().text_label_sm().child(title))
                .children(
                    subtitle.map(|text| div().text_caption().text_color(c.text_subtle).child(text)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(space::S2)
                .children(actions),
        )
}

/// `EmptyState` (`level` 4): a bordered card with an icon, title and
/// description.
pub(crate) fn empty_state(
    icon: IconName,
    title: &'static str,
    description: &'static str,
    cx: &App,
) -> Div {
    let c = cx.colors();
    div()
        .w_full()
        .border_1()
        .border_color(c.border_standard)
        .rounded(radius::CONTAINER)
        .flex()
        .flex_col()
        .items_center()
        .gap(space::S4)
        .py(space::S8)
        .px(space::S6)
        .text_center()
        .child(Icon::new(icon).size(32.).color(c.text_subtle))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .max_w(rems(30.))
                .child(div().text_label_sm().child(title))
                .child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(description),
                ),
        )
}

/// A labelled `Field` with an optional description under the label.
pub(crate) fn field(
    label: impl Into<SharedString>,
    description: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    let style = FieldStyle {
        description_first: true,
        ..FieldStyle::default()
    };
    form::field(label, description.map(Into::into), None, control, style, cx)
}

/// A status or alert line.
pub(crate) use crate::ui::settings::form::line;

/// The selected community, if any.
pub(crate) fn current(cx: &App) -> Option<Entity<Community>> {
    AppState::current(cx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn key_labels_grow_until_distinct() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let labels = public_key_labels([&a, &b]);
        assert!(labels[&a].starts_with("npub…"));
        assert_ne!(labels[&a], labels[&b]);
        assert_eq!(labels[&a].len(), "npub…".len() + 3);
    }
}
