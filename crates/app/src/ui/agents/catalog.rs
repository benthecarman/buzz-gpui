//! The community catalog on the Agents page (`CommunityCatalog.tsx`): the
//! "Share to catalog" switch, the Community Catalog browser, its agent
//! and team previews, and the Add agent dialog's catalog list.
//!
//! The relay data comes from [`CommunityCatalog`], one per community.
//! Adding never runs anything from the catalog: an agent goes through the
//! ordinary create form with a fresh identity, and a team through the
//! shared team importer.

use super::editor::{Dismissed, session_policy};
use super::inventory::in_community;
use crate::community::{AppState, Community};
use crate::hosts::{HostConnection, Hosts};
use crate::mirror::mirror;
use buzz_host::api::{self, Agent};
use buzz_relay_client::catalog::{
    AgentPublication, CatalogAgent, CatalogDelivery, CatalogKind, CatalogStatus, CatalogView,
    PersonaSource, Publication, SessionPolicy, TeamPublication, catalog_slug, share_notice,
    unsupported_transport_message,
};
use buzz_relay_client::session::CommunityCatalog;
use buzz_relay_client::{EventId, Partition};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, Dialog, DialogSize, Icon, IconName,
    NavigationItem, Surface, Switch, TypeStyled, tokens::radius,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

/// Agent sharing lives in the agent's Share dialog, which embeds
/// [`CatalogShareSwitch::agent`].
pub const AGENT_SHARE_DESCRIPTION: &str = "Anyone in this community can find and use a copy. Your agent instruction is shared as plaintext. Memories and secrets aren’t included.";
pub const TEAM_SHARE_DESCRIPTION: &str = "Anyone in this community can find and add a copy of this team. Both the team instructions and every member’s instructions are shared as plaintext. Memories and secrets aren’t included.";
/// View state holding which local copy each catalog entry produced.
const ADDED: &str = "catalog-added";

/// One community's catalog, mirrored into GPUI.
pub struct CatalogState {
    pub catalog: CommunityCatalog,
    pub view: Arc<CatalogView>,
    pub community: WeakEntity<Community>,
    partition: Partition,
    /// The stored Added hints, read once.
    added: RefCell<Option<HashMap<String, String>>>,
    _watch: Task<()>,
}

/// A catalog entry's coordinate, as [`Publication::coordinate`] builds it,
/// without an owned publication.
fn coordinate(kind: CatalogKind, owner: &str, d: &str) -> String {
    format!("{}:{owner}:{d}", kind.kind())
}

#[derive(Default)]
struct Catalogs(HashMap<EntityId, Entity<CatalogState>>);

impl Global for Catalogs {}

impl CatalogState {
    /// The community's catalog, started on first use.
    pub fn of(community: &Entity<Community>, cx: &mut App) -> Entity<Self> {
        let id = community.entity_id();
        if let Some(state) = cx.default_global::<Catalogs>().0.get(&id) {
            return state.clone();
        }
        let catalog = community.read(cx).session.community_catalog();
        let partition = community.read(cx).partition();
        let weak = community.downgrade();
        let state = cx.new(|cx| Self {
            view: catalog.snapshot(),
            _watch: mirror(catalog.view(), cx, |this: &mut Self, view| this.view = view),
            catalog,
            community: weak,
            partition,
            added: RefCell::new(None),
        });
        cx.global_mut::<Catalogs>().0.insert(id, state.clone());
        state
    }

    /// The current community's catalog.
    pub fn current(cx: &mut App) -> Option<Entity<Self>> {
        AppState::current(cx).map(|community| Self::of(&community, cx))
    }

    pub fn viewer(&self) -> &str {
        self.catalog.viewer()
    }

    /// The local copy each catalog coordinate produced for this viewer in
    /// this community. Only a hint: a copy is independent of its entry.
    fn added<R>(&self, cx: &App, f: impl FnOnce(&mut HashMap<String, String>) -> R) -> R {
        let mut added = self.added.borrow_mut();
        let added = added.get_or_insert_with(|| {
            cx.global::<AppState>()
                .store
                .view_state(&self.partition, ADDED)
                .ok()
                .flatten()
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default()
        });
        f(added)
    }

    pub fn remember(&self, publication: &Publication, copy: &str, cx: &App) {
        let json = self.added(cx, |added| {
            added.insert(publication.coordinate(), copy.to_owned());
            serde_json::to_string(added).expect("maps serialize")
        });
        // The copy exists either way; only the Added hint is lost.
        let _ = cx
            .global::<AppState>()
            .store
            .set_view_state(&self.partition, ADDED, Some(&json));
    }

    /// The viewer's own entries, and those whose copy still exists.
    pub fn already_added(
        &self,
        publication: &Publication,
        has_copy: impl Fn(&str) -> bool,
        cx: &App,
    ) -> bool {
        self.already_added_at(
            publication.kind(),
            publication.owner(),
            publication.d(),
            has_copy,
            cx,
        )
    }

    fn already_added_at(
        &self,
        kind: CatalogKind,
        owner: &str,
        d: &str,
        has_copy: impl Fn(&str) -> bool,
        cx: &App,
    ) -> bool {
        if owner == self.viewer() {
            return true;
        }
        // `has_copy` may read this state again, so the hints are let go
        // first.
        let copy = self.added(cx, |added| added.get(&coordinate(kind, owner, d)).cloned());
        copy.is_some_and(|copy| has_copy(&copy))
    }

    /// "You", the owner's profile name, or "Community member".
    pub fn owner_label(&self, owner: &str, cx: &App) -> SharedString {
        if owner == self.viewer() {
            return "You".into();
        }
        self.community
            .upgrade()
            .and_then(|community| {
                community
                    .read(cx)
                    .snapshot
                    .profiles
                    .get(owner)
                    .filter(|profile| !profile.name_is_fallback)
                    .map(|profile| profile.name.clone().into())
            })
            .unwrap_or_else(|| "Community member".into())
    }
}

/// Whether an agent with this id exists on any host.
pub fn has_agent(id: &str, cx: &App) -> bool {
    cx.global::<Hosts>()
        .hosts
        .iter()
        .any(|host| host.read(cx).agent(id).is_some())
}

/// The community's agents on every host, with the host that runs each.
pub fn community_agents(
    community: &Entity<Community>,
    cx: &App,
) -> Vec<(Entity<HostConnection>, Agent)> {
    let relay = &community.read(cx).origin;
    cx.global::<Hosts>()
        .hosts
        .iter()
        .flat_map(|host| {
            host.read(cx)
                .agents
                .agents
                .iter()
                .filter(|agent| in_community(&agent.relay_url, relay))
                .map(|agent| (host.clone(), agent.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn policy(policy: api::SessionPolicy) -> SessionPolicy {
    match policy {
        api::SessionPolicy::Channel => SessionPolicy::Channel,
        api::SessionPolicy::Thread => SessionPolicy::Thread,
    }
}

/// The agent as the catalog projects it.
pub fn persona(agent: &Agent) -> PersonaSource {
    PersonaSource {
        pubkey: agent.pubkey.clone(),
        name: agent.name.clone(),
        system_prompt: agent.system_prompt.clone(),
        picture: agent.picture.clone(),
        harness_command: agent.harness.command.clone(),
        model: agent.harness.model.clone(),
        provider: agent.harness.provider.clone(),
        acp_command: agent.acp_command.clone(),
        respond_to: agent.respond_to.clone(),
        session_policy: agent.session_policy.map(policy),
    }
}

/// The boundary an inheriting agent on `host` runs with.
pub fn default_policy(host: &Entity<HostConnection>, cx: &App) -> Option<SessionPolicy> {
    session_policy(host, cx).map(policy)
}

// ---------------------------------------------------------------------------
// The share switch

/// Whether a saved team with this ID exists.
pub type HasTeam = Rc<dyn Fn(&str, &App) -> bool>;

/// Told a new local copy's ID.
pub type Created = Rc<dyn Fn(&str, &mut App)>;

/// Called when a sidebar entry is chosen.
pub type Choose = Rc<dyn Fn(&mut Window, &mut App)>;

/// Projects the current local definition; may refuse with a reason.
pub type Content = Rc<dyn Fn(&mut App) -> Task<Result<String, String>>>;

/// One coordinate's "Share to catalog" switch (`CatalogShareSwitch`). The
/// switch shows the newest intent; its notice tells a queued, accepted
/// or refused relay delivery apart.
pub struct CatalogShareSwitch {
    state: Entity<CatalogState>,
    kind: CatalogKind,
    d: String,
    name: SharedString,
    description: &'static str,
    content: Content,
    pending: bool,
    error: Option<String>,
    /// A confirmation Retry stays busy while its re-read runs.
    checking: Option<EventId>,
    _observe: Subscription,
}

impl CatalogShareSwitch {
    pub fn new(
        state: Entity<CatalogState>,
        kind: CatalogKind,
        d: String,
        name: impl Into<SharedString>,
        description: &'static str,
        content: Content,
        cx: &mut Context<Self>,
    ) -> Self {
        state.read(cx).catalog.retain();
        Self {
            _observe: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            kind,
            d,
            name: name.into(),
            description,
            content,
            pending: false,
            error: None,
            checking: None,
        }
    }

    /// The switch for one agent (`AgentShareSwitch`), or `None` when the
    /// agent is not set up in the current community. The agent is read
    /// from its host when sharing, never from relay data. The agent's
    /// Share dialog embeds it.
    pub fn agent(
        host: Entity<HostConnection>,
        agent: &Agent,
        cx: &mut App,
    ) -> Option<Entity<Self>> {
        let community = AppState::current(cx)?;
        if !in_community(&agent.relay_url, &community.read(cx).origin) {
            return None;
        }
        let d = catalog_slug(&agent.pubkey).ok()?;
        let state = CatalogState::of(&community, cx);
        let id = agent.id.clone();
        let content: Content = Rc::new(move |cx: &mut App| {
            let host = host.clone();
            let agent = host.read(cx).agent(&id).cloned();
            let default = default_policy(&host, cx);
            Task::ready(
                agent
                    .ok_or_else(|| "Agent no longer exists".to_string())
                    .and_then(|agent| {
                        buzz_relay_client::catalog::agent_catalog_content(&persona(&agent), default)
                    }),
            )
        });
        let name = agent.name.clone();
        Some(cx.new(|cx| {
            Self::new(
                state,
                CatalogKind::Agent,
                d,
                name,
                AGENT_SHARE_DESCRIPTION,
                content,
                cx,
            )
        }))
    }

    fn busy(&mut self, pending: bool, cx: &mut Context<Self>) {
        self.pending = pending;
        cx.notify();
    }

    fn update(&mut self, next: bool, cx: &mut Context<Self>) {
        self.error = None;
        self.busy(true, cx);
        let content = next.then(|| (self.content)(cx));
        let catalog = self.state.read(cx).catalog.clone();
        let (kind, d) = (self.kind, self.d.clone());
        cx.spawn(async move |this, cx| {
            let text = match content {
                Some(task) => match task.await {
                    Ok(text) => Some(text),
                    Err(error) => {
                        let _ = this.update(cx, |this, cx| {
                            this.error = Some(error);
                            this.busy(false, cx);
                        });
                        return;
                    }
                },
                None => None,
            };
            // A projection that settles after the switch is gone must not
            // publish: a newer intent may already have been recorded.
            if this.upgrade().is_none() {
                return;
            }
            let result = catalog.publish(kind, &d, next, text).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some(error);
                }
                this.busy(false, cx);
            });
        })
        .detach();
    }
}

fn small(id: impl Into<ElementId>, label: &'static str) -> Button {
    Button::new(id, label)
        .size(ButtonSize::Sm)
        .surface(Surface::Floating)
}

impl Render for CatalogShareSwitch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let state = self.state.read(cx);
        let view = state.view.clone();
        let catalog = state.catalog.clone();
        let share = view.state(self.kind, &self.d);
        let change = share.change.clone();
        let rejected = change
            .as_ref()
            .is_some_and(|change| change.delivery == CatalogDelivery::Rejected);
        let checked = match &change {
            Some(change) if !rejected => change.shared,
            _ => share.shared,
        };
        let confirming = change.as_ref().is_some_and(|change| {
            self.checking == Some(change.operation)
                && change.delivery == CatalogDelivery::Queued
                && !change.stalled
        });
        let disabled = self.pending || view.status != CatalogStatus::Ready || !view.writable;
        let row = div()
            .flex()
            .items_center()
            .gap(rems(1.))
            .py(rems(0.75))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(rems(0.125))
                    .child(div().text_label_sm().child("Share to catalog"))
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .child(self.description),
                    ),
            )
            .child({
                let this = cx.entity().downgrade();
                Switch::new("share-to-catalog", checked)
                    .disabled(disabled)
                    .on_change(move |next, _, cx| {
                        let _ = this.update(cx, |this, cx| this.update(next, cx));
                    })
            });
        let line = || div().flex().flex_wrap().items_center().gap(rems(0.5));
        let mut column = div().flex().flex_col().min_w_0().gap(rems(0.5)).child(row);
        if view.status == CatalogStatus::Error {
            let refresh = catalog.clone();
            column = column.child(
                line()
                    .child(div().text_body_sm().children(view.error.clone()))
                    .child(
                        small("catalog-retry-read", "Retry")
                            .on_click(move |_, _, _| refresh.refresh()),
                    ),
            );
        }
        if let Some(change) = change.clone().filter(|_| !rejected) {
            let mut notice = share_notice(&self.name, change.shared, change.delivery);
            if change.stalled
                && let Some(error) = &change.error
            {
                notice = format!("{notice} {error}");
            }
            let operation = change.operation;
            let retry_catalog = catalog.clone();
            let dismiss_catalog = catalog.clone();
            column = column.child(
                line()
                    .child(div().text_body_sm().child(notice))
                    .when(change.stalled || confirming, |this| {
                        this.child(
                            small("catalog-retry", "Retry")
                                .loading(confirming)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.checking = Some(operation);
                                    retry_catalog.retry(operation);
                                    cx.notify();
                                })),
                        )
                    })
                    .when(
                        change.delivery == CatalogDelivery::Accepted
                            || change.stalled
                            || confirming,
                        |this| {
                            this.child(
                                Button::new("catalog-dismiss", "Dismiss")
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        let catalog = dismiss_catalog.clone();
                                        cx.spawn(async move |_, _| {
                                            let _ = catalog.dismiss(operation).await;
                                        })
                                        .detach();
                                    })),
                            )
                        },
                    ),
            );
        }
        if self.error.is_some() || rejected {
            let detail = self
                .error
                .clone()
                .or_else(|| change.as_ref().and_then(|change| change.error.clone()));
            let text = match detail {
                Some(detail) => format!("Failed to update catalog sharing. {detail}"),
                None => "Failed to update catalog sharing.".into(),
            };
            let mut alert =
                line().child(div().text_body_sm().text_color(c.text_danger).child(text));
            if let (true, None, Some(change)) = (rejected, &self.error, &change) {
                let operation = change.operation;
                let (retry, dismiss) = (catalog.clone(), catalog.clone());
                alert = alert
                    .child(
                        small("catalog-retry-refused", "Retry")
                            .on_click(move |_, _, _| retry.retry(operation)),
                    )
                    .child(
                        Button::new("catalog-dismiss-refused", "Dismiss")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let catalog = dismiss.clone();
                                cx.spawn(async move |_, _| {
                                    let _ = catalog.dismiss(operation).await;
                                })
                                .detach();
                            })),
                    );
            }
            column = column.child(alert);
        }
        column
    }
}

// ---------------------------------------------------------------------------
// Previews

/// "Added by {who}".
pub fn added_by(label: SharedString, c: &buzz_ui::Colors) -> Div {
    div()
        .text_body_sm()
        .text_color(c.text_subtle)
        .child("Added by")
        .child(div().text_color(c.text_standard).child(label))
        .flex()
        .gap(rems(0.25))
}

/// Type, model, runtime and provider in a four-column grid.
pub fn metadata(agent: &CatalogAgent, c: &buzz_ui::Colors) -> Div {
    let default = || "Use app default".to_string();
    let items = [
        ("Type", "Custom agent".to_string()),
        (
            "Preferred model",
            agent.model.clone().unwrap_or_else(default),
        ),
        (
            "Preferred runtime",
            agent.runtime.clone().unwrap_or_else(default),
        ),
        (
            "Preferred provider",
            agent.provider.clone().unwrap_or_else(default),
        ),
    ];
    div()
        .grid()
        .grid_cols(4)
        .gap(rems(0.75))
        .children(items.map(|(label, value)| {
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(div().text_body_sm().text_color(c.text_subtle).child(label))
                .child(div().text_body_sm().child(value))
        }))
}

/// Exact instructions as plain text: Markdown would hide link targets, so
/// the reviewed text could differ from what runs.
pub fn instructions(id: impl Into<ElementId>, text: &str, c: &buzz_ui::Colors) -> Stateful<Div> {
    let text = if text.is_empty() {
        "No instructions included.".to_string()
    } else {
        text.to_string()
    };
    div()
        .id(id)
        .max_h(rems(20.))
        .overflow_y_scroll()
        .p(rems(0.75))
        .rounded(radius::CONTROL)
        .bg(c.surface_base)
        .text_body_sm()
        .child(text)
}

fn avatar(name: &str, picture: Option<&String>, size: AvatarSize) -> Avatar {
    let mut avatar = Avatar::new(name.to_string()).size(size);
    if let Some(url) = picture {
        avatar = avatar.image(crate::ui::media::any_avatar(url));
    }
    avatar
}

/// One shared agent (`AgentCatalogPreview`).
pub fn agent_preview(
    publication: &AgentPublication,
    owner: SharedString,
    c: &buzz_ui::Colors,
) -> Div {
    let agent = &publication.agent;
    div()
        .flex()
        .flex_col()
        .gap(rems(1.5))
        .child(
            div()
                .flex()
                .items_center()
                .gap(rems(0.75))
                .child(avatar(
                    &agent.display_name,
                    agent.avatar_url.as_ref(),
                    AvatarSize::Large,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .text_heading()
                                .truncate()
                                .child(agent.display_name.clone()),
                        )
                        .child(added_by(owner, c)),
                ),
        )
        .children(
            agent
                .description
                .clone()
                .map(|text| div().text_body_sm().text_color(c.text_subtle).child(text)),
        )
        .child(metadata(agent, c))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .child(div().text_label().child("Agent instructions"))
                .child(instructions(
                    SharedString::from(format!("instructions-{}", publication.event_id)),
                    &agent.system_prompt,
                    c,
                )),
        )
}

pub type Toggle = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// One shared team (`TeamCatalogPreview`): members expand to their
/// settings and instructions.
pub fn team_preview(
    publication: &TeamPublication,
    owner: Option<SharedString>,
    expanded: &HashSet<String>,
    toggle: Toggle,
    c: &buzz_ui::Colors,
) -> Div {
    let count = publication.members.len();
    let members = publication
        .members
        .iter()
        .enumerate()
        .map(|(index, member)| {
            let open = expanded.contains(&member.member_key);
            let key = member.member_key.clone();
            let toggle = toggle.clone();
            let agent = &member.agent;
            div()
                .flex()
                .flex_col()
                .gap(rems(0.75))
                .child(
                    div()
                        .id(("team-member", index))
                        .flex()
                        .items_center()
                        .gap(rems(0.75))
                        .px(rems(0.75))
                        .py(rems(0.5))
                        .rounded(radius::CONTROL)
                        .cursor_pointer()
                        .hover(|this| this.bg(c.affordance_subtle_hover))
                        .child(avatar(
                            &agent.display_name,
                            agent.avatar_url.as_ref(),
                            AvatarSize::Small,
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().child(agent.display_name.clone()))
                                .child(
                                    div()
                                        .truncate()
                                        .text_body_sm()
                                        .text_color(c.text_subtle)
                                        .child(
                                            agent
                                                .model
                                                .clone()
                                                .unwrap_or_else(|| "Use app default".into()),
                                        ),
                                ),
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
                        .on_click(move |_, window, cx| toggle(key.clone(), window, cx)),
                )
                .when(open, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.75))
                            .pl(rems(0.5))
                            .child(metadata(agent, c))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(rems(0.5))
                                    .child(div().text_label_sm().child("Agent instructions"))
                                    .child(if agent.system_prompt.trim().is_empty() {
                                        div()
                                            .text_body_sm()
                                            .text_color(c.text_subtle)
                                            .child("No instructions")
                                            .into_any_element()
                                    } else {
                                        instructions(
                                            ("member-instructions", index),
                                            &agent.system_prompt,
                                            c,
                                        )
                                        .into_any_element()
                                    }),
                            ),
                    )
                })
        });
    div()
        .flex()
        .flex_col()
        .gap(rems(1.5))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_heading()
                        .truncate()
                        .child(publication.name.clone()),
                )
                .children(owner.map(|owner| added_by(owner, c)))
                .children(publication.description.clone().map(|text| {
                    div()
                        .mt(rems(0.75))
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(text)
                })),
        )
        .children(
            publication
                .instructions
                .clone()
                .filter(|text| !text.trim().is_empty())
                .map(|text| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.5))
                        .child(div().text_label().child("Team instructions"))
                        .child(instructions("team-instructions", &text, c))
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .child(div().text_label().child(format!(
                    "{count} {}",
                    if count == 1 { "member" } else { "members" }
                )))
                .children(members),
        )
}

// ---------------------------------------------------------------------------
// The Community Catalog dialog

/// An agent's portable create seed (`catalogSeed`); the create form
/// resolves the runtime locally.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogSeed {
    pub name: String,
    pub system_prompt: String,
    pub session_policy: SessionPolicy,
    pub runtime: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub picture: Option<String>,
}

impl CatalogSeed {
    pub fn of(agent: &CatalogAgent) -> Self {
        Self {
            name: agent.display_name.clone(),
            system_prompt: agent.system_prompt.clone(),
            session_policy: agent.session_policy,
            runtime: agent.runtime.clone(),
            model: agent.model.clone(),
            provider: agent.provider.clone(),
            picture: agent.avatar_url.clone(),
        }
    }
}

/// "Add agent" in the catalog: the page opens the create form seeded
/// with the entry, and the catalog remembers the copy it produced.
pub type AddAgent = Rc<dyn Fn(AgentPublication, &mut Window, &mut App)>;

/// "Add team": resolves with the local team copy's ID.
pub type AddTeam = Rc<dyn Fn(TeamPublication, &mut App) -> Task<Result<String, String>>>;

/// Browse and preview shared agents and teams (`CommunityCatalogDialog`).
pub struct CatalogDialog {
    state: Entity<CatalogState>,
    selected: Option<String>,
    adding: bool,
    error: Option<String>,
    expanded: HashSet<String>,
    add_agent: Option<AddAgent>,
    add_team: Option<AddTeam>,
    has_team: HasTeam,
    _observe: Subscription,
}

impl EventEmitter<Dismissed> for CatalogDialog {}

impl CatalogDialog {
    pub fn new(
        state: Entity<CatalogState>,
        add_agent: Option<AddAgent>,
        add_team: Option<AddTeam>,
        has_team: HasTeam,
        cx: &mut Context<Self>,
    ) -> Self {
        state.read(cx).catalog.retain();
        Self {
            _observe: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            selected: None,
            adding: false,
            error: None,
            expanded: HashSet::new(),
            add_agent,
            add_team,
            has_team,
        }
    }

    fn current(&self, view: &CatalogView) -> Option<Publication> {
        let agent = |key: &str| {
            view.agents
                .iter()
                .find(|entry| coordinate(CatalogKind::Agent, &entry.owner, &entry.d) == key)
        };
        let team = |key: &str| {
            view.teams
                .iter()
                .find(|entry| coordinate(CatalogKind::Team, &entry.owner, &entry.d) == key)
        };
        let selected = self.selected.as_deref().and_then(|key| {
            agent(key)
                .cloned()
                .map(Publication::Agent)
                .or_else(|| team(key).cloned().map(Publication::Team))
        });
        selected.or_else(|| view.publications().next())
    }

    fn is_added(&self, publication: &Publication, cx: &App) -> bool {
        let has_team = self.has_team.clone();
        let state = self.state.read(cx);
        match publication {
            Publication::Agent(_) => state.already_added(publication, |id| has_agent(id, cx), cx),
            Publication::Team(_) => state.already_added(publication, |id| has_team(id, cx), cx),
        }
    }

    fn add(&mut self, publication: Publication, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding {
            return;
        }
        match publication {
            Publication::Agent(agent) => {
                let Some(add) = self.add_agent.clone() else {
                    return;
                };
                cx.emit(Dismissed);
                add(agent, window, cx);
            }
            Publication::Team(team) => {
                let Some(add) = self.add_team.clone() else {
                    return;
                };
                self.error = None;
                self.adding = true;
                cx.notify();
                let task = add(team.clone(), cx);
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.adding = false;
                        match result {
                            Ok(copy) => {
                                this.state
                                    .read(cx)
                                    .remember(&Publication::Team(team), &copy, cx);
                            }
                            Err(error) => this.error = Some(error),
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
    }
}

impl Render for CatalogDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let view = self.state.read(cx).view.clone();
        let current = self.current(&view);
        let added = current
            .as_ref()
            .is_some_and(|entry| self.is_added(entry, cx));
        let transport = current
            .as_ref()
            .and_then(|entry| entry.unsupported_transport().map(str::to_owned));
        let mut dialog = Dialog::new("community-catalog", "Community Catalog")
            .size(DialogSize::Wide)
            .when(!self.adding, |this| {
                this.on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
            });
        match &current {
            Some(Publication::Agent(_)) if self.add_agent.is_some() => {
                let entry = current.clone().expect("an entry");
                dialog = dialog.footer(
                    Button::new(
                        "catalog-add-agent",
                        if added {
                            "Added to My Agents"
                        } else {
                            "Add agent"
                        },
                    )
                    .prominent()
                    .disabled(added || self.adding || transport.is_some())
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.add(entry.clone(), window, cx)),
                    ),
                );
            }
            Some(Publication::Team(_)) if self.add_team.is_some() => {
                let entry = current.clone().expect("an entry");
                dialog = dialog.footer(
                    Button::new(
                        "catalog-add-team",
                        if added {
                            "Added to my teams"
                        } else if self.adding {
                            "Adding…"
                        } else {
                            "Add team"
                        },
                    )
                    .prominent()
                    .disabled(added || self.adding || transport.is_some())
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.add(entry.clone(), window, cx)),
                    ),
                );
            }
            _ => {}
        }
        let loading = current.is_none()
            && matches!(view.status, CatalogStatus::Loading | CatalogStatus::Idle);
        let body = if loading {
            div()
                .child("Loading the community catalog…")
                .into_any_element()
        } else if view.status == CatalogStatus::Error {
            let catalog = self.state.read(cx).catalog.clone();
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(rems(0.5))
                .child(div().children(view.error.clone()))
                .child(
                    Button::new("catalog-retry", "Retry")
                        .surface(Surface::Floating)
                        .on_click(move |_, _, _| catalog.refresh()),
                )
                .into_any_element()
        } else if let Some(current) = current.clone() {
            self.browser(&view, &current, transport, cx)
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(rems(0.25))
                .py(rems(4.))
                .items_center()
                .text_center()
                .child(div().text_label().child("Nothing shared yet"))
                .child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child("Shared agents and teams will appear here."),
                )
                .into_any_element()
        };
        dialog.child(div().min_h(rems(28.)).child(body))
    }
}

impl CatalogDialog {
    fn browser(
        &self,
        view: &CatalogView,
        current: &Publication,
        transport: Option<String>,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let section =
            |label: &'static str| div().text_label_sm().text_color(c.text_subtle).child(label);
        let mut sidebar = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(rems(14.))
            .gap(rems(0.25))
            .pr(rems(1.))
            .border_r_1()
            .border_color(c.border_standard);
        if !view.agents.is_empty() {
            sidebar = sidebar.child(section("Agents"));
        }
        let current_key = current.coordinate();
        for (index, entry) in view.agents.iter().enumerate() {
            let key = coordinate(CatalogKind::Agent, &entry.owner, &entry.d);
            let selected = current_key == key;
            sidebar = sidebar.child(
                NavigationItem::new(("catalog-agent", index), entry.agent.display_name.clone())
                    .leading(avatar(
                        &entry.agent.display_name,
                        entry.agent.avatar_url.as_ref(),
                        AvatarSize::Small,
                    ))
                    .selected(selected)
                    .surface(Surface::Floating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(key.clone());
                        this.error = None;
                        cx.notify();
                    })),
            );
        }
        if !view.teams.is_empty() {
            sidebar = sidebar.child(section("Teams"));
        }
        for (index, entry) in view.teams.iter().enumerate() {
            let key = coordinate(CatalogKind::Team, &entry.owner, &entry.d);
            let selected = current_key == key;
            sidebar = sidebar.child(
                NavigationItem::new(("catalog-team", index), entry.name.clone())
                    .trailing(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .child(entry.members.len().to_string()),
                    )
                    .selected(selected)
                    .surface(Surface::Floating)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(key.clone());
                        this.error = None;
                        cx.notify();
                    })),
            );
        }
        let state = self.state.read(cx);
        let owner = state.owner_label(current.owner(), cx);
        let added = self.is_added(current, cx);
        let detail = match current {
            Publication::Agent(entry) => agent_preview(entry, owner, &c),
            Publication::Team(entry) => {
                let page = cx.entity().downgrade();
                let toggle: Toggle = Rc::new(move |key, _, cx| {
                    let _ = page.update(cx, |this, cx| {
                        if !this.expanded.remove(&key) {
                            this.expanded.insert(key);
                        }
                        cx.notify();
                    });
                });
                team_preview(entry, Some(owner), &self.expanded, toggle, &c)
            }
        };
        let details = div()
            .id("catalog-details")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(rems(1.5))
            .p(rems(1.5))
            .rounded(radius::CONTROL)
            .bg(c.surface_inset)
            .child(detail)
            .when_some(transport.filter(|_| !added), |this, alias| {
                this.child(
                    div()
                        .text_body_sm()
                        .child(unsupported_transport_message(current.name(), &alias)),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_body_sm().text_color(c.text_danger).child(error))
            });
        div()
            .flex()
            .gap(rems(1.5))
            .min_h_full()
            .child(sidebar)
            .child(details)
    }
}

// ---------------------------------------------------------------------------
// The Add agent dialog's catalog list (`AgentCreateDialog` sidebar)

/// The catalog's agents in the Add agent dialog, and the selected one's
/// preview. The dialog shows it beside its own panes.
pub struct CatalogPane {
    pub state: Entity<CatalogState>,
    /// The selected entry, `owner:d`.
    pub selected: Option<String>,
    _observe: Subscription,
}

impl CatalogPane {
    pub fn new(state: Entity<CatalogState>, cx: &mut Context<Self>) -> Self {
        state.read(cx).catalog.retain();
        Self {
            _observe: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            selected: None,
        }
    }

    /// Only for the current community, when it has a catalog.
    pub fn current(cx: &mut App) -> Option<Entity<Self>> {
        let state = CatalogState::current(cx)?;
        Some(cx.new(|cx| Self::new(state, cx)))
    }

    pub fn publication(&self, cx: &App) -> Option<AgentPublication> {
        self.selected_entry(self.state.read(cx)).cloned()
    }

    fn selected_entry<'a>(&self, state: &'a CatalogState) -> Option<&'a AgentPublication> {
        let key = self.selected.as_ref()?;
        state
            .view
            .agents
            .iter()
            .find(|entry| format!("{}:{}", entry.owner, entry.d) == *key)
    }

    pub fn already_added(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        self.selected_entry(state).is_some_and(|entry| {
            state.already_added_at(
                CatalogKind::Agent,
                &entry.owner,
                &entry.d,
                |id| has_agent(id, cx),
                cx,
            )
        })
    }

    pub fn transport(&self, cx: &App) -> Option<String> {
        self.publication(cx).and_then(|entry| {
            Publication::Agent(entry)
                .unsupported_transport()
                .map(str::to_owned)
        })
    }

    pub fn remember(&self, entry: &AgentPublication, copy: &str, cx: &App) {
        self.state
            .read(cx)
            .remember(&Publication::Agent(entry.clone()), copy, cx);
    }

    /// "AGENTS" and the catalog's agents, for the dialog's sidebar.
    /// `select` is told the newly selected entry.
    pub fn sidebar(&self, this: Entity<Self>, disabled: bool, select: Choose, cx: &App) -> Div {
        let c = cx.colors();
        let view = self.state.read(cx).view.clone();
        let mut list = div().flex().flex_col().gap(rems(0.25));
        if !view.agents.is_empty() {
            list = list.child(
                div()
                    .px(rems(0.75))
                    .pt(rems(0.75))
                    .pb(rems(0.25))
                    .text_caption()
                    .text_color(c.text_subtle)
                    .child("AGENTS"),
            );
        }
        for (index, entry) in view.agents.iter().enumerate() {
            let key = format!("{}:{}", entry.owner, entry.d);
            let pane = this.clone();
            let select = select.clone();
            list = list.child(
                NavigationItem::new(("catalog-entry", index), entry.agent.display_name.clone())
                    .leading(avatar(
                        &entry.agent.display_name,
                        entry.agent.avatar_url.as_ref(),
                        AvatarSize::Small,
                    ))
                    .selected(self.selected.as_ref() == Some(&key))
                    .disabled(disabled)
                    .on_click(move |_, window, cx| {
                        pane.update(cx, |pane, cx| {
                            pane.selected = Some(key.clone());
                            cx.notify();
                        });
                        select(window, cx);
                    }),
            );
        }
        if view.status == CatalogStatus::Error {
            list = list.child(
                div()
                    .px(rems(0.75))
                    .text_body_sm()
                    .text_color(c.text_danger)
                    .children(view.error.clone()),
            );
        }
        list
    }

    /// The selected entry's preview, or why it is gone.
    pub fn preview(&self, cx: &App) -> Div {
        let c = cx.colors();
        let Some(entry) = self.publication(cx) else {
            return div().child("This agent is no longer shared. Select another agent.");
        };
        let owner = self.state.read(cx).owner_label(&entry.owner, cx);
        agent_preview(&entry, owner, c).when_some(self.transport(cx), |this, alias| {
            this.child(div().text_body_sm().child(unsupported_transport_message(
                &entry.agent.display_name,
                &alias,
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CatalogKind, Publication, TeamPublication, coordinate};

    #[test]
    fn coordinate_matches_the_publication() {
        let team = TeamPublication {
            event_id: String::new(),
            owner: "owner".into(),
            d: "slug".into(),
            created_at: 0,
            name: String::new(),
            description: None,
            instructions: None,
            members: Vec::new(),
        };
        assert_eq!(
            coordinate(CatalogKind::Team, &team.owner, &team.d),
            Publication::Team(team).coordinate()
        );
    }
}
