//! The Add team and Edit team dialog (`ChannelTemplatesDialog.tsx` for a
//! team): a name, the team instructions every member gets after its own,
//! and the member agents. A new team can also be imported from a file or
//! added from the community catalog through the sidebar.
//!
//! The instructions live in their own record. Save writes the members
//! first, then the text, then delivers every team's text to the members'
//! settings; a retry repeats only the phases that did not finish.

use super::catalog::{AddTeam, CatalogState, HasTeam, Toggle, community_agents, team_preview};
use super::editor::Dismissed;
use super::form::field;
use super::team_text::{
    deliver_team_texts, read_team_texts, resolve_team_text, team_text_conflict,
};
use super::teams::{ImportFile, TeamsState};
use buzz_relay_client::catalog::{Publication, TeamPublication, unsupported_transport_message};
use buzz_relay_client::kit::TeamManifest;
use buzz_relay_client::session::{Resume, TeamKit, TeamsStatus};
use buzz_relay_client::teams::{Team, TeamEntry};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, Checkbox, Dialog, IconName,
    NavigationItem, Surface, TextArea, TextField, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;

/// Instructions as saved, and the text head they came from.
#[derive(Clone, Debug, Default)]
struct Saved {
    text: String,
    head: Option<String>,
}

/// One save attempt: its text revision and the phases it finished or
/// enqueued, so a retry confirms the same events instead of writing again.
/// Any draft edit starts a new attempt.
#[derive(Clone)]
struct Attempt {
    serial: u64,
    revision: String,
    team_head: Option<String>,
    manifest: Option<TeamManifest>,
    team_done: bool,
    text_done: bool,
    team: Resume,
    text: Resume,
}

impl Attempt {
    fn new(serial: u64, team_head: Option<String>) -> Self {
        Self {
            serial,
            revision: uuid::Uuid::new_v4().to_string(),
            team_head,
            manifest: None,
            team_done: false,
            text_done: false,
            team: Resume::new(),
            text: Resume::new(),
        }
    }
}

pub struct TeamEditor {
    teams: Entity<TeamsState>,
    catalog: Entity<CatalogState>,
    /// The saved team being edited; `None` adds one.
    initial: Option<TeamEntry>,
    id: String,
    name: Entity<InputState>,
    instructions: Entity<TextareaState>,
    search: Entity<InputState>,
    agents: Vec<String>,
    /// The instructions as saved; `None` until they load.
    saved: Option<Saved>,
    /// The roster as last saved, so a text-only retry never rewrites the
    /// team.
    saved_team: Option<Team>,
    attempt: Attempt,
    loading: bool,
    busy: bool,
    error: Option<SharedString>,
    /// The catalog team selected in the sidebar, `owner:d`.
    selected: Option<String>,
    catalog_busy: bool,
    catalog_error: Option<String>,
    expanded: HashSet<String>,
    add_team: Option<AddTeam>,
    has_team: HasTeam,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for TeamEditor {}
impl EventEmitter<ImportFile> for TeamEditor {}

impl TeamEditor {
    pub fn new(
        teams: Entity<TeamsState>,
        catalog: Entity<CatalogState>,
        initial: Option<TeamEntry>,
        add_team: Option<AddTeam>,
        has_team: HasTeam,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Engineering Squad"));
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Optional instructions every member gets after its own.")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Name or npub"));
        if let Some(entry) = &initial {
            let value = entry.team.name.clone();
            name.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        name.update(cx, |state, cx| state.focus(window, cx));
        let edited = |this: &mut Self, event: &InputEvent, cx: &mut Context<Self>| {
            if matches!(event, InputEvent::Change) {
                this.changed(cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe(&name, move |this, _, event: &InputEvent, cx| {
                edited(this, event, cx)
            }),
            cx.subscribe(&instructions, move |this, _, event: &InputEvent, cx| {
                edited(this, event, cx)
            }),
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.observe(&teams, |_, _, cx| cx.notify()),
            cx.observe(&catalog, |_, _, cx| cx.notify()),
        ];
        if initial.is_none() {
            catalog.read(cx).catalog.retain();
        }
        teams.read(cx).kit.ensure();
        let team_head = initial.as_ref().map(|entry| entry.event_id.clone());
        let mut editor = Self {
            id: initial
                .as_ref()
                .map(|entry| entry.team.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            agents: initial
                .as_ref()
                .map(|entry| entry.team.agents.clone())
                .unwrap_or_default(),
            // Only an edited saved team loads its instructions; a new team
            // starts empty with no head.
            loading: initial.is_some(),
            saved: initial.is_none().then(Saved::default),
            saved_team: initial.as_ref().map(|entry| entry.team.clone()),
            attempt: Attempt::new(0, team_head),
            teams,
            catalog,
            initial,
            name,
            instructions,
            search,
            busy: false,
            error: None,
            selected: None,
            catalog_busy: false,
            catalog_error: None,
            expanded: HashSet::new(),
            add_team,
            has_team,
            _subscriptions: subscriptions,
        };
        editor.load_text(window, cx);
        editor
    }

    /// Loads an edited team's current instructions: its text head, else a
    /// legacy portable bundle's text. A team whose text can't be read can't
    /// be saved, so stale text never overwrites it.
    fn load_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.initial.clone() else {
            return;
        };
        let kit = self.teams.read(cx).kit.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = resolve_team_text(&kit, &entry.team, cx).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((text, head)) => {
                        let value = text.clone();
                        this.instructions
                            .update(cx, |state, cx| state.set_value(value, window, cx));
                        this.saved = Some(Saved { text, head });
                        this.loading = false;
                    }
                    Err(error) => {
                        this.error = Some(
                            format!(
                                "This team's instructions can't be read, so it can't be saved now: {error}"
                            )
                            .into(),
                        )
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Any edit starts a new save attempt.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.attempt = Attempt::new(self.attempt.serial + 1, self.attempt.team_head.clone());
        cx.notify();
    }

    fn draft_name(&self, cx: &App) -> String {
        self.name.read(cx).value().to_string()
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.draft_name(cx);
        if self.busy || self.loading || self.catalog_busy || name.trim().is_empty() {
            return;
        }
        let Some(saved) = self.saved.clone() else {
            self.error = Some("Team instructions are still loading".into());
            cx.notify();
            return;
        };
        let draft = Team {
            id: self.id.clone(),
            name,
            agents: self.agents.clone(),
            portable: self
                .initial
                .as_ref()
                .and_then(|entry| entry.team.portable.clone()),
        };
        if draft.portable.is_some() && draft.agents.is_empty() {
            self.error = Some("A team imported from a file needs at least one member.".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let text = self.instructions.read(cx).value().to_string();
        let roster_changed = self
            .saved_team
            .as_ref()
            .is_none_or(|base| base.name != draft.name || base.agents != draft.agents);
        let text_changed = text != saved.text;
        let teams = self.teams.clone();
        let kit = teams.read(cx).kit.clone();
        let mut run = self.attempt.clone();
        cx.spawn(async move |this, cx| {
            let check = Check {
                kit: &kit,
                draft: &draft,
                saved: &saved,
                text: &text,
                roster_changed,
            };
            // A recheck holds until a phase writes: preparing text chunks
            // leaves every head as it was.
            let mut checked = false;
            let result: Result<(), String> = async {
                if text_changed && run.manifest.is_none() {
                    check.recheck(&run, cx).await?;
                    checked = true;
                    run.manifest = Some(kit.prepare_text(&draft.id, &text, &run.revision).await?);
                }
                if roster_changed && !run.team_done {
                    if run.team.id().is_none() && !checked {
                        check.recheck(&run, cx).await?;
                    }
                    let head = kit
                        .save_resumable(
                            draft.clone(),
                            run.team_head.clone(),
                            false,
                            run.team.clone(),
                        )
                        .await?;
                    run.team_head = Some(head);
                    run.team_done = true;
                    checked = false;
                    let _ = this.update(cx, |this, _| this.saved_team = Some(draft.clone()));
                }
                if let Some(manifest) = run.manifest.clone().filter(|_| !run.text_done) {
                    if run.text.id().is_none() && !checked {
                        check.recheck(&run, cx).await?;
                    }
                    let head = kit
                        .publish_text(
                            &draft.id,
                            Some(manifest),
                            saved.head.clone(),
                            run.team_head.clone(),
                            run.text.clone(),
                        )
                        .await?;
                    run.text_done = true;
                    let saved = Saved {
                        text: text.clone(),
                        head: Some(head),
                    };
                    let _ = this.update(cx, |this, _| this.saved = Some(saved));
                }
                deliver_team_texts(&teams, cx)
                    .await
                    .map_err(|error| format!("Saved; members' instructions not updated: {error}"))
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                let message = result.err().map(|message| {
                    if run.team_done && run.manifest.is_some() && !run.text_done {
                        format!("Members and name saved; instructions not saved: {message}")
                    } else {
                        message
                    }
                });
                // An edit during the save started a new attempt, from the
                // team head as it was then: give it the head this save
                // wrote, so its retry doesn't conflict with this save.
                if this.attempt.serial == run.serial {
                    this.attempt = run;
                } else {
                    this.attempt.team_head = run.team_head;
                }
                match message {
                    None => cx.emit(Dismissed),
                    Some(message) => {
                        this.error = Some(message.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn publication(&self, cx: &App) -> Option<TeamPublication> {
        let key = self.selected.as_ref()?;
        self.catalog
            .read(cx)
            .view
            .teams
            .iter()
            .find(|team| format!("{}:{}", team.owner, team.d) == *key)
            .cloned()
    }
}

/// What a save rechecks before every phase it has not enqueued yet,
/// retries included: other teams and this team's text can change between
/// attempts. Saving goes members first, then text, so the in-between
/// roster is checked against the old text too.
struct Check<'a> {
    kit: &'a TeamKit,
    draft: &'a Team,
    saved: &'a Saved,
    text: &'a str,
    roster_changed: bool,
}

impl Check<'_> {
    async fn recheck(&self, run: &Attempt, cx: &mut AsyncApp) -> Result<(), String> {
        let others = read_team_texts(self.kit, cx).await?;
        // Members-only saves check too: the conflict checks assume the text
        // this dialog loaded is still the team's current text.
        if !run.text_done {
            let current = self.kit.read_text_head(&self.draft.id).await?;
            if current.map(|current| current.head) != self.saved.head {
                return Err(
                    "This team's instructions changed. Refresh and review them before saving."
                        .into(),
                );
            }
        }
        let (id, agents) = (&self.draft.id, &self.draft.agents);
        let before = (self.roster_changed && !run.team_done)
            .then(|| team_text_conflict(&others, id, agents, &self.saved.text))
            .flatten();
        match before.or_else(|| team_text_conflict(&others, id, agents, self.text)) {
            Some(conflict) => Err(conflict),
            None => Ok(()),
        }
    }
}

impl TeamEditor {
    fn add_from_catalog(&mut self, cx: &mut Context<Self>) {
        let (Some(add), Some(listed)) = (self.add_team.clone(), self.publication(cx)) else {
            return;
        };
        if self.catalog_busy {
            return;
        }
        self.catalog_busy = true;
        self.catalog_error = None;
        cx.notify();
        let task = add(listed.clone(), cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.catalog_busy = false;
                match result {
                    Ok(copy) => {
                        this.catalog
                            .read(cx)
                            .remember(&Publication::Team(listed), &copy, cx);
                        cx.emit(Dismissed);
                    }
                    Err(error) => this.catalog_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `.teamAddSidebar`: create, import, and the catalog's teams.
    fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let view = self.catalog.read(cx).view.clone();
        let locked =
            self.catalog_busy || !self.draft_name(cx).is_empty() || !self.agents.is_empty();
        let mut sidebar = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(rems(12.))
            .gap(rems(0.25))
            .pr(rems(0.75))
            .border_r_1()
            .border_color(c.border_standard)
            .child(
                NavigationItem::new("create-team-pane", "Create team")
                    .icon(IconName::Plus)
                    .selected(self.selected.is_none())
                    .disabled(locked)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        cx.notify();
                    })),
            )
            .child(
                div().pt(rems(0.25)).child(
                    Button::new("import-team", "Import")
                        .outline()
                        .size(ButtonSize::Sm)
                        .full_width()
                        .disabled(locked)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ImportFile))),
                ),
            );
        if !view.teams.is_empty() {
            sidebar = sidebar.child(
                div()
                    .p(rems(0.75))
                    .text_label()
                    .text_color(c.text_subtle)
                    .child("TEAMS"),
            );
        }
        for (index, team) in view.teams.iter().enumerate() {
            let key = format!("{}:{}", team.owner, team.d);
            sidebar = sidebar.child(
                NavigationItem::new(("catalog-team", index), team.name.clone())
                    .selected(self.selected.as_ref() == Some(&key))
                    .disabled(locked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(key.clone());
                        this.catalog_error = None;
                        cx.notify();
                    })),
            );
        }
        if let Some(error) = view.error.clone() {
            sidebar = sidebar.child(div().text_body_sm().text_color(c.text_danger).child(error));
        }
        sidebar
    }

    fn catalog_pane(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let Some(listed) = self.publication(cx) else {
            return div().child("This team is no longer shared. Select another team.");
        };
        let publication = Publication::Team(listed.clone());
        let transport = publication.unsupported_transport().map(str::to_owned);
        let has_team = self.has_team.clone();
        let added = self
            .catalog
            .read(cx)
            .already_added(&publication, |id| has_team(id, cx), cx);
        let editor = cx.entity().downgrade();
        let toggle: Toggle = Rc::new(move |key, _, cx| {
            let _ = editor.update(cx, |this, cx| {
                if !this.expanded.remove(&key) {
                    this.expanded.insert(key);
                }
                cx.notify();
            });
        });
        div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(team_preview(&listed, None, &self.expanded, toggle, &c))
            .children(transport.clone().map(|alias| {
                div()
                    .text_body_sm()
                    .child(unsupported_transport_message(&listed.name, &alias))
            }))
            .children(
                self.catalog_error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .child(
                div().child(
                    Button::new(
                        "add-catalog-team",
                        if added {
                            "Added to my teams"
                        } else if self.catalog_busy {
                            "Adding…"
                        } else {
                            "Add team"
                        },
                    )
                    .prominent()
                    .disabled(
                        added
                            || self.catalog_busy
                            || transport.is_some()
                            || self.add_team.is_none(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.add_from_catalog(cx))),
                ),
            )
    }

    /// `AgentSelection`: the community's local agents with checkboxes.
    fn selection(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let community = self.teams.read(cx).community.clone();
        let mut choices: Vec<(String, String, Option<String>)> = community_agents(&community, cx)
            .into_iter()
            .map(|(_, agent)| (agent.pubkey, agent.name, agent.picture))
            .collect();
        choices.dedup_by(|a, b| a.0 == b.0);
        for key in &self.agents {
            if !choices.iter().any(|(pubkey, ..)| pubkey == key) {
                choices.push((key.clone(), "Unavailable agent".into(), None));
            }
        }
        let query = self.search.read(cx).value().to_lowercase();
        let matches: Vec<_> = choices
            .iter()
            .filter(|(pubkey, name, _)| format!("{name} {pubkey}").to_lowercase().contains(&query))
            .cloned()
            .collect();
        let mut list = div()
            .id("team-agent-choices")
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .max_h(rems(12.))
            .overflow_y_scroll();
        for (index, (pubkey, name, picture)) in matches.iter().enumerate() {
            let checked = self.agents.contains(pubkey);
            let key = pubkey.clone();
            let mut avatar = Avatar::new(name.clone()).squircle().size(AvatarSize::Small);
            if let Some(url) = picture.clone().filter(|p| !p.is_empty()) {
                avatar = avatar.image(crate::ui::media::any_avatar(&url));
            }
            let this = cx.entity().downgrade();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .child(avatar)
                    .child(
                        Checkbox::new(("team-agent", index), checked)
                            .label(name.clone())
                            .description(format!("{}…{}", &pubkey[..8], &pubkey[56..]))
                            .surface(Surface::Floating)
                            .on_change(move |checked, _, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    if checked {
                                        if !this.agents.contains(&key) {
                                            this.agents.push(key.clone());
                                        }
                                    } else {
                                        this.agents.retain(|known| *known != key);
                                    }
                                    this.changed(cx);
                                });
                            }),
                    ),
            );
        }
        if !choices.is_empty() && matches.is_empty() {
            list = list.child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("No agents match your search."),
            );
        }
        if choices.is_empty() {
            list = list.child(
                div()
                    .text_color(c.text_subtle)
                    .child("No agents from the Agents page are available in this community."),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .when(!choices.is_empty(), |this| {
                this.child(field(
                    "Find individual agents",
                    None,
                    TextField::new(&self.search).surface(Surface::Floating),
                    &c,
                ))
            })
            .child(list)
    }

    fn form(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let state = self.teams.read(cx).view.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(rems(1.))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .when(state.status != TeamsStatus::Ready, |this| {
                this.child(
                    div().text_body_sm().child(
                        state
                            .error
                            .clone()
                            .unwrap_or_else(|| "Loading your saved templates…".into()),
                    ),
                )
            })
            .child(field(
                "Name",
                None,
                TextField::new(&self.name).surface(Surface::Floating),
                &c,
            ))
            .child(field(
                "Team Instructions",
                None,
                TextArea::new(&self.instructions)
                    .rows(3)
                    .disabled(self.saved.is_none())
                    .surface(Surface::Floating),
                &c,
            ))
            .when(self.loading && self.error.is_none(), |this| {
                this.child(div().text_body_sm().child("Loading team instructions…"))
            })
            .child(self.selection(cx))
    }
}

impl Render for TeamEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let adding = self.initial.is_none();
        let ready = self.teams.read(cx).view.status == TeamsStatus::Ready;
        let blocked = self.busy || self.catalog_busy;
        let body = if adding {
            let pane = if self.selected.is_some() {
                self.catalog_pane(cx)
            } else {
                self.form(cx)
            };
            div()
                .flex()
                .gap(rems(1.))
                .child(self.sidebar(cx))
                .child(div().flex_1().min_w_0().child(pane))
        } else {
            self.form(cx)
        };
        Dialog::new("team-editor", if adding { "Add team" } else { "Edit team" })
            .description(
                "Choose agents to reuse together in future channels and @mentions. This team is \
             private to you in this community.",
            )
            .when(!blocked, |this| {
                this.on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
            })
            .child(body)
            .footer(
                Button::new("cancel-team", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(blocked)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
            )
            .footer(
                Button::new("save-team", "Save team")
                    .prominent()
                    .loading(self.busy)
                    .disabled(
                        self.draft_name(cx).trim().is_empty()
                            || !ready
                            || self.loading
                            || self.catalog_busy,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }
}
