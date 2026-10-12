//! Agent teams on the Agents page (`TemplateLibrary.tsx` with
//! `section="team"`, from buzz-app's channel-templates plugin): the
//! viewer's saved teams in the current community, and creating, editing,
//! importing, sharing, exporting, deploying and deleting them.

use super::catalog::{AddAgent, AddTeam, CatalogDialog, CatalogState, HasTeam};
use super::editor::Dismissed;
use super::team_dialogs::{self, TeamDeployDialog, TeamImportDialog};
use super::team_editor::TeamEditor;
use super::team_file;
use super::team_ops;
use super::team_text::deliver_team_texts;
use crate::community::{AppState, Community};
use crate::mirror::mirror;
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_relay_client::catalog::{
    Publication, TeamPublication, catalog_team_snapshot, unsupported_transport_message,
};
use buzz_relay_client::session::{Resume, TeamKit, TeamsStatus, TeamsView};
use buzz_relay_client::teams::TeamEntry;
use buzz_ui::{
    ActiveTheme, AlertDialog, Avatar, AvatarSize, Button, ButtonSize, Icon, IconButton,
    IconButtonSize, IconName, Menu, MenuItem, MenuSize, Surface, TypeStyled, tokens::radius,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// One community's saved teams, mirrored into GPUI.
pub struct TeamsState {
    pub kit: TeamKit,
    pub view: Arc<TeamsView>,
    pub community: Entity<Community>,
    /// Why app-start delivery of team instructions gave up, until a
    /// delivery succeeds (`teamSyncError`).
    pub sync_error: Option<String>,
    /// Team instruction deliveries run one at a time.
    pub delivery: Arc<tokio::sync::Mutex<()>>,
    _watch: Task<()>,
}

/// Each community's teams, until the community is removed.
#[derive(Default)]
struct Kits(HashMap<EntityId, Entity<TeamsState>>);

impl Global for Kits {}

impl Kits {
    /// Drops removed communities' teams, so they and their community are
    /// freed once nothing else holds them.
    fn prune(cx: &mut App) {
        let live: Vec<EntityId> = cx
            .global::<AppState>()
            .communities
            .iter()
            .map(Entity::entity_id)
            .collect();
        cx.global_mut::<Kits>().0.retain(|id, _| live.contains(id));
    }
}

impl TeamsState {
    pub fn of(community: &Entity<Community>, cx: &mut App) -> Entity<Self> {
        if !cx.has_global::<Kits>() {
            cx.set_global(Kits::default());
            cx.observe_global::<AppState>(Kits::prune).detach();
        }
        let id = community.entity_id();
        if let Some(state) = cx.global::<Kits>().0.get(&id) {
            return state.clone();
        }
        let kit = community.read(cx).session.team_kit();
        let community = community.clone();
        let state = cx.new(|cx| Self {
            view: kit.snapshot(),
            _watch: mirror(kit.view(), cx, |this: &mut Self, view| this.view = view),
            kit,
            community,
            sync_error: None,
            delivery: Arc::default(),
        });
        cx.global_mut::<Kits>().0.insert(id, state.clone());
        state
    }

    pub fn has_team(&self, id: &str) -> bool {
        self.view.teams().any(|entry| entry.team.id == id)
    }
}

/// Adopts a listed catalog team through the shared importer, after
/// checking the previewed head is still the shared one (`adoptCatalogTeam`).
/// Catalog teams carry no memories or allowlists, and nothing starts.
pub fn adopt_catalog_team(
    catalog: Entity<CatalogState>,
    teams: Entity<TeamsState>,
    listed: TeamPublication,
    cx: &mut App,
) -> Task<Result<String, String>> {
    let context = match team_ops::import_context(&teams, false, false, cx) {
        Ok(context) => context,
        Err(error) => return Task::ready(Err(error)),
    };
    let catalog = catalog.read(cx).catalog.clone();
    cx.spawn(async move |cx| {
        let team = catalog.current_team(&listed).await?;
        let publication = Publication::Team(team.clone());
        if let Some(alias) = publication.unsupported_transport() {
            return Err(unsupported_transport_message(&team.name, alias));
        }
        let snapshot = serde_json::from_value(catalog_team_snapshot(&team))
            .map_err(|error| error.to_string())?;
        let imported = team_ops::import_team(context, snapshot, cx).await?;
        Ok(imported.id)
    })
}

enum Modal {
    Editor(Entity<TeamEditor>),
    Delete {
        entry: Box<TeamEntry>,
        busy: bool,
        error: Option<SharedString>,
    },
    Import(Entity<TeamImportDialog>),
    Deploy(Entity<TeamDeployDialog>),
    Catalog(Entity<CatalogDialog>),
}

/// One team delete: its team tombstone, then its text head's, so a retry
/// confirms the same events and repeats only unfinished phases.
struct Removal {
    event_id: String,
    team_done: bool,
    text_done: bool,
    team: Resume,
    text: Resume,
}

/// The Agents page's "Agent teams" section, for the current community.
pub struct TeamsSection {
    /// The page's create form, seeded from a catalog agent.
    add_agent: AddAgent,
    community: Option<Entity<Community>>,
    teams: Option<Entity<TeamsState>>,
    catalog: Option<Entity<CatalogState>>,
    menu: Option<String>,
    modal: Option<Modal>,
    /// The phases of the current delete that finished or were enqueued.
    removed: Option<Removal>,
    preview_error: Option<SharedString>,
    /// The section width, measured after layout, for the card grid.
    width: Rc<Cell<Pixels>>,
    _subscriptions: Vec<Subscription>,
    _modal: Vec<Subscription>,
}

impl TeamsSection {
    pub fn new(add_agent: AddAgent, cx: &mut Context<Self>) -> Self {
        team_dialogs::register(cx);
        let mut section = Self {
            add_agent,
            community: None,
            teams: None,
            catalog: None,
            menu: None,
            modal: None,
            removed: None,
            preview_error: None,
            width: Rc::new(Cell::new(px(0.))),
            _subscriptions: Vec::new(),
            _modal: Vec::new(),
        };
        section.sync(cx);
        section
    }

    /// Follows the selected community.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let current = AppState::current(cx);
        if current.as_ref().map(Entity::entity_id) == self.community.as_ref().map(Entity::entity_id)
            && !self._subscriptions.is_empty()
        {
            return;
        }
        let mut subscriptions = vec![cx.observe_global::<AppState>(|this, cx| this.sync(cx))];
        self.teams = current
            .as_ref()
            .map(|community| TeamsState::of(community, cx));
        self.catalog = current
            .as_ref()
            .map(|community| CatalogState::of(community, cx));
        if let Some(teams) = &self.teams {
            subscriptions.push(cx.observe(teams, |_, _, cx| cx.notify()));
            teams.read(cx).kit.ensure();
        }
        for host in cx.global::<crate::hosts::Hosts>().hosts.clone() {
            subscriptions.push(cx.observe(&host, |_, _, cx| cx.notify()));
        }
        self.community = current;
        // Everything open belongs to the old community.
        self.modal = None;
        self._modal.clear();
        self.menu = None;
        self.preview_error = None;
        self._subscriptions = subscriptions;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        self._modal.clear();
        cx.notify();
    }

    fn show<T: EventEmitter<Dismissed>>(
        &mut self,
        entity: &Entity<T>,
        modal: Modal,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self._modal = vec![cx.subscribe(entity, |this, _, _: &Dismissed, cx| this.close(cx))];
        self.modal = Some(modal);
        cx.notify();
    }

    fn has_team(&self) -> HasTeam {
        let teams = self.teams.clone();
        Rc::new(move |id, cx| {
            teams
                .as_ref()
                .is_some_and(|teams| teams.read(cx).has_team(id))
        })
    }

    fn add_team(&self) -> Option<AddTeam> {
        let (catalog, teams) = (self.catalog.clone()?, self.teams.clone()?);
        Some(Rc::new(move |listed, cx| {
            adopt_catalog_team(catalog.clone(), teams.clone(), listed, cx)
        }))
    }

    /// The Community Catalog browser (`CatalogLauncher`).
    pub fn open_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(catalog) = self.catalog.clone() else {
            return;
        };
        let (add, has) = (self.add_team(), self.has_team());
        let agent = Some(self.add_agent.clone());
        let dialog = cx.new(|cx| CatalogDialog::new(catalog, agent, add, has, cx));
        self.show(&dialog, Modal::Catalog(dialog.clone()), cx);
    }

    fn open_editor(
        &mut self,
        entry: Option<TeamEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(teams), Some(catalog)) = (self.teams.clone(), self.catalog.clone()) else {
            return;
        };
        let (add, has) = (self.add_team(), self.has_team());
        let editor = cx.new(|cx| TeamEditor::new(teams, catalog, entry, add, has, window, cx));
        self.show(&editor, Modal::Editor(editor.clone()), cx);
        self._modal.push(cx.subscribe_in(
            &editor,
            window,
            |this, _, _: &ImportFile, window, cx| {
                this.close(cx);
                this.pick_file(window, cx);
            },
        ));
    }

    /// Picks a team file and previews it (`previewFile`).
    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_error = None;
        let dev = std::env::var_os("BUZZ_DEV_PICK_FILE").map(std::path::PathBuf::from);
        let paths = dev.is_none().then(|| {
            cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Import team snapshot".into()),
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match (dev, paths) {
                (Some(path), _) => path,
                (None, Some(paths)) => match paths.await {
                    Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                        Some(path) => path,
                        None => return,
                    },
                    _ => return,
                },
                (None, None) => return,
            };
            let content = cx
                .background_executor()
                .spawn(async move {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    if size > team_file::MAX_PNG_BYTES as u64 {
                        return Err("Team snapshot exceeds the size limit".to_string());
                    }
                    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                    team_file::decode(&bytes)
                })
                .await;
            let host = this
                .update(cx, |_, cx| team_ops::import_host(cx))
                .ok()
                .flatten();
            let result = match (content, host) {
                (Ok(content), Some(host)) => team_ops::preview(&host, content, cx).await,
                (Ok(_), None) => Err("Team preview is unavailable".into()),
                (Err(error), _) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(snapshot) => this.open_import(snapshot, window, cx),
                Err(error) => {
                    this.preview_error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_import(
        &mut self,
        snapshot: buzz_host::api::TeamSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(teams) = self.teams.clone() else {
            return;
        };
        let dialog = cx.new(|cx| TeamImportDialog::new(teams, snapshot, window, cx));
        self.show(&dialog, Modal::Import(dialog.clone()), cx);
    }

    fn open_deploy(&mut self, entry: TeamEntry, cx: &mut Context<Self>) {
        let Some(teams) = self.teams.clone() else {
            return;
        };
        let dialog = cx.new(|cx| TeamDeployDialog::new(teams, entry.team, cx));
        self.show(&dialog, Modal::Deploy(dialog.clone()), cx);
    }

    /// "Share {team}" in the snapshot share dialog.
    fn open_share(&mut self, entry: TeamEntry, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(teams), Some(catalog)) = (self.teams.clone(), self.catalog.clone()) else {
            return;
        };
        self.menu = None;
        team_dialogs::open_share(teams, catalog, entry.team, window, cx);
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let (Some(Modal::Delete { entry, busy, error }), Some(teams)) =
            (&mut self.modal, self.teams.clone())
        else {
            return;
        };
        if *busy {
            return;
        }
        *busy = true;
        *error = None;
        let (team, expected) = (entry.team.clone(), entry.event_id.clone());
        let kit = teams.read(cx).kit.clone();
        // A retry of the same delete repeats only its unfinished phases.
        let mut run = self
            .removed
            .take()
            .filter(|run| run.event_id == expected)
            .unwrap_or_else(|| Removal {
                event_id: expected.clone(),
                team_done: false,
                text_done: false,
                team: Resume::new(),
                text: Resume::new(),
            });
        cx.spawn(async move |this, cx| {
            let result: Result<(), String> = async {
                if !run.team_done {
                    kit.save_resumable(team.clone(), Some(expected), true, run.team.clone())
                        .await?;
                    run.team_done = true;
                }
                let mut cleanup = None;
                if !run.text_done {
                    // Retire the text head with a tombstone so legacy text
                    // never reappears. No head, or one already retired,
                    // needs nothing.
                    let retired: Result<(), String> = async {
                        let current = match run.text.id() {
                            Some(_) => None,
                            None => kit.read_text_head(&team.id).await?,
                        };
                        if run.text.id().is_some() || current.as_ref().is_some_and(|c| !c.deleted)
                        {
                            kit.publish_text(
                                &team.id,
                                None,
                                current.map(|current| current.head),
                                None,
                                run.text.clone(),
                            )
                            .await?;
                        }
                        Ok(())
                    }
                    .await;
                    match retired {
                        Ok(()) => run.text_done = true,
                        Err(error) => cleanup = Some(error),
                    }
                }
                // Members lose the deleted team's text even if cleanup
                // failed.
                deliver_team_texts(&teams, cx).await?;
                cleanup.map_or(Ok(()), Err)
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                let team_done = run.team_done;
                this.removed = Some(run);
                match result {
                    Ok(()) => this.close(cx),
                    Err(message) => {
                        let message = if team_done {
                            format!(
                                "Team deleted; instructions cleanup or member update pending: {message}"
                            )
                        } else {
                            message
                        };
                        if let Some(Modal::Delete { busy, error, .. }) = &mut this.modal {
                            *busy = false;
                            *error = Some(message.into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss_menu(cx: &mut Context<Self>) -> Dismiss {
        let page = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| {
                this.menu = None;
                cx.notify();
            });
        })
    }
}

/// The team editor asks for a team file (`onImport`).
pub struct ImportFile;

/// A short key for a member that is not available here.
fn short_key(pubkey: &str) -> String {
    match pubkey.get(..8) {
        Some(head) => format!("{head}…"),
        None => "Unavailable agent".into(),
    }
}

/// A community agent's name and picture, by pubkey.
type Faces<'a> = HashMap<&'a str, (&'a str, Option<&'a str>)>;

/// The community's agents on every host, borrowed for one render.
fn faces<'a>(community: &Entity<Community>, cx: &'a App) -> Faces<'a> {
    let relay = community.read(cx).origin.websocket().to_string();
    cx.global::<crate::hosts::Hosts>()
        .hosts
        .iter()
        .flat_map(|host| host.read(cx).agents.agents.iter())
        .filter(|agent| agent.relay_url.trim_end_matches('/') == relay)
        .map(|agent| {
            (
                agent.pubkey.as_str(),
                (agent.name.as_str(), agent.picture.as_deref()),
            )
        })
        .collect()
}

impl TeamsSection {
    fn card(
        &self,
        index: usize,
        entry: &TeamEntry,
        members: Vec<(String, Option<String>)>,
        disabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let count = members.len();
        let mut stack = div()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .h(rems(5.))
            .px(rems(0.5));
        for (position, (name, picture)) in members.iter().take(4).enumerate() {
            let mut avatar = Avatar::new(name.clone())
                .squircle()
                .size(AvatarSize::Px(44.));
            if let Some(url) = picture.clone().filter(|p| !p.is_empty()) {
                avatar = avatar.image(crate::ui::media::any_avatar(&url));
            }
            // The reference fans the avatars out; GPUI cannot rotate, so
            // they overlap and step instead.
            let lift = [0.2, 0., 0., 0.2][position];
            stack = stack.child(
                div()
                    .when(position > 0, |this| this.ml(rems(-1.5)))
                    .mt(rems(lift))
                    .rounded(radius::CONTAINER)
                    .border_2()
                    .border_color(c.surface_panel)
                    .child(avatar),
            );
        }
        if members.is_empty() {
            stack = stack.child(Icon::new(IconName::Users).size(48.).color(c.text_subtle));
        }
        if count > 4 {
            stack = stack.child(
                div()
                    .absolute()
                    .right_0()
                    .bottom(rems(0.5))
                    .min_w(rems(1.25))
                    .px(rems(0.25))
                    .rounded_full()
                    .bg(c.surface_panel)
                    .text_caption()
                    .child(format!("+{}", count - 4)),
            );
        }
        let open = self.menu.as_ref() == Some(&entry.team.id);
        // One copy for the card's handlers, which copy it again on click.
        let menu_entry = Rc::new(entry.clone());
        let deploy = menu_entry.clone();
        let share = menu_entry.clone();
        let edit = menu_entry.clone();
        let remove = menu_entry.clone();
        let menu = Menu::new()
            .size(MenuSize::Compact)
            .child(MenuItem::new("deploy-team", "Deploy to channel").on_click(
                cx.listener(move |this, _, _, cx| this.open_deploy((*deploy).clone(), cx)),
            ))
            .child(MenuItem::new("share-team", "Share").on_click(cx.listener(
                move |this, _, window, cx| this.open_share((*share).clone(), window, cx),
            )))
            .child(
                MenuItem::new("edit-team", "Edit team").on_click(cx.listener(
                    move |this, _, window, cx| this.open_editor(Some((*edit).clone()), window, cx),
                )),
            )
            .child(
                MenuItem::new("delete-team", "Delete team…")
                    .danger()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.modal = Some(Modal::Delete {
                            entry: Box::new((*remove).clone()),
                            busy: false,
                            error: None,
                        });
                        cx.notify();
                    })),
            );
        let id = menu_entry.team.id.clone();
        div()
            .id(("team-card", index))
            .relative()
            .min_w_0()
            .border_1()
            .border_color(c.border_standard)
            .rounded(radius::CONTAINER)
            .child(
                div()
                    .id(("team-open", index))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .w_full()
                    .min_h(rems(14.))
                    .px(rems(0.75))
                    .py(rems(1.5))
                    .rounded(radius::CONTAINER)
                    .when(!disabled, |this| {
                        this.cursor_pointer().hover(|this| this.bg(c.surface_base))
                    })
                    .when(disabled, |this| this.opacity(0.6))
                    .child(stack)
                    .child(
                        div()
                            .mt(rems(1.))
                            .max_w_full()
                            .text_label_sm()
                            .text_center()
                            .child(menu_entry.team.name.clone()),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .child(format!(
                                "{count} {}",
                                if count == 1 { "member" } else { "members" }
                            )),
                    )
                    .when(!disabled, |this| {
                        this.on_click(cx.listener(move |this, _, window, cx| {
                            this.open_editor(Some((*menu_entry).clone()), window, cx)
                        }))
                    }),
            )
            .child(
                div().absolute().top(rems(0.5)).right(rems(0.5)).child(
                    div()
                        .relative()
                        .child(
                            IconButton::new(("team-actions", index), IconName::DotsThree)
                                .size(IconButtonSize::Sm)
                                .selected(open)
                                .disabled(disabled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.menu = Some(id.clone());
                                    cx.notify();
                                })),
                        )
                        .when(open, |this| {
                            this.child(overlay::dropdown(
                                window,
                                Align::End,
                                menu,
                                Self::dismiss_menu(cx),
                            ))
                        }),
                ),
            )
            .into_any_element()
    }

    fn modal_element(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let page = cx.entity().downgrade();
        let close: Dismiss = Rc::new(move |_, cx| {
            let _ = page.update(cx, |this, cx| this.close(cx));
        });
        let (content, dismiss): (AnyElement, Option<Dismiss>) = match self.modal.as_ref()? {
            Modal::Editor(editor) => (editor.clone().into_any_element(), None),
            Modal::Import(dialog) => (dialog.clone().into_any_element(), None),
            Modal::Deploy(dialog) => (dialog.clone().into_any_element(), None),
            Modal::Catalog(dialog) => (dialog.clone().into_any_element(), None),
            Modal::Delete { entry, busy, error } => {
                let dialog =
                    AlertDialog::new(format!("Delete “{}”?", entry.team.name))
                        .description(
                            "Only this saved team is deleted. Its agents and their channel \
                         memberships stay unchanged. Templates using this team will need a \
                         replacement.",
                        )
                        .children(error.clone().map(|error| {
                            div().text_body_sm().text_color(c.text_danger).child(error)
                        }))
                        .action(
                            Button::new("cancel-delete-team", "Cancel")
                                .surface(Surface::Floating)
                                .disabled(*busy)
                                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                        )
                        .action(
                            Button::new("confirm-delete-team", "Delete")
                                .destructive()
                                .loading(*busy)
                                .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
                        );
                (dialog.into_any_element(), (!*busy).then_some(close))
            }
        };
        Some(overlay::modal(window, content, dismiss).into_any_element())
    }
}

/// Columns of `.teamGrid`: `repeat(auto-fill, minmax(10.75rem, 1fr))`
/// with a 16px gap.
fn columns(width: Pixels) -> u16 {
    ((f32::from(width) + 16.) / 188.).floor().clamp(1., 8.) as u16
}

impl Render for TeamsSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let Some(teams) = self.teams.clone() else {
            return div()
                .flex()
                .flex_col()
                .gap(rems(0.75))
                .child(div().text_label_sm().child("Agent teams"))
                .child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child("Choose a connected community to manage teams."),
                )
                .into_any_element();
        };
        let view = teams.read(cx).view.clone();
        let ready = view.status == TeamsStatus::Ready;
        let loading = view.status == TeamsStatus::Loading;
        let width = self.width.clone();
        let section = cx.entity().downgrade();
        let measure = canvas(
            move |bounds, _, cx| {
                let old = width.replace(bounds.size.width);
                if columns(old) != columns(bounds.size.width) {
                    let _ = section.update(cx, |_, cx| cx.notify());
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let status = match (&view.status, &view.error) {
            (_, Some(error)) => Some(error.clone()),
            (TeamsStatus::Ready, None) => None,
            _ => Some("Loading your teams…".to_string()),
        };
        let kit = teams.read(cx).kit.clone();
        let header =
            div()
                .flex()
                .items_start()
                .gap(rems(0.75))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_label_sm()
                        .child("Agent teams"),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .flex_wrap()
                        .items_center()
                        .gap(rems(0.5))
                        .children(self.preview_error.clone().map(|error| {
                            div().text_body_sm().text_color(c.text_danger).child(error)
                        }))
                        .child(
                            Button::new("refresh-teams", "Refresh teams")
                                .ghost()
                                .size(ButtonSize::Sm)
                                .icon(IconName::ArrowsClockwise)
                                .loading(loading)
                                .on_click(move |_, _, _| kit.refresh()),
                        )
                        .child(
                            Button::new("create-team", "Create team")
                                .size(ButtonSize::Sm)
                                .icon(IconName::Plus)
                                .disabled(!ready)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_editor(None, window, cx)
                                })),
                        ),
                );
        let body = if view.teams().next().is_none() {
            ready.then(|| {
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(rems(1.))
                    .py(rems(2.))
                    .px(rems(1.5))
                    .border_1()
                    .border_color(c.border_standard)
                    .rounded(radius::CONTAINER)
                    .text_center()
                    .child(Icon::new(IconName::Users).size(32.).color(c.text_subtle))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .max_w(rems(30.))
                            .child(div().text_label_sm().child("Bring your agents together"))
                            .child(div().text_body_sm().text_color(c.text_subtle).child(
                                "Create a team to reuse your favorite combination of agents.",
                            )),
                    )
                    .into_any_element()
            })
        } else {
            // Each card's members, from one pass over the hosts' agents.
            let members: Vec<Vec<(String, Option<String>)>> = {
                let faces = match &self.community {
                    Some(community) => faces(community, cx),
                    None => Faces::new(),
                };
                view.teams()
                    .map(|entry| {
                        entry
                            .team
                            .agents
                            .iter()
                            .map(|pubkey| match faces.get(pubkey.as_str()) {
                                Some((name, picture)) => {
                                    (name.to_string(), picture.map(str::to_string))
                                }
                                None => (short_key(pubkey), None),
                            })
                            .collect()
                    })
                    .collect()
            };
            let cards: Vec<AnyElement> = view
                .teams()
                .zip(members)
                .enumerate()
                .map(|(index, (entry, members))| {
                    self.card(index, entry, members, !ready, window, cx)
                })
                .collect();
            Some(
                div()
                    .grid()
                    .grid_cols(columns(self.width.get()))
                    .gap(rems(1.))
                    .children(cards)
                    .into_any_element(),
            )
        };
        let modal = self.modal_element(window, cx);
        let sync_error = teams.read(cx).sync_error.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .child(measure)
            .children(
                sync_error.map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .children(status.map(|text| div().text_body_sm().text_color(c.text_subtle).child(text)))
            .child(header)
            .children(body)
            .children(modal)
            .into_any_element()
    }
}
