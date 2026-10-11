//! The Add team and Edit team dialog (`ChannelTemplatesDialog.tsx` for a
//! team): a name, the portable description and team instructions, and the
//! member agents. A new team can also be imported from a file or added
//! from the community catalog through the sidebar.

use super::catalog::{AddTeam, CatalogState, HasTeam, Toggle, community_agents, team_preview};
use super::editor::Dismissed;
use super::form::field;
use super::team_ops::{self, capture};
use super::teams::{ImportFile, TeamsState};
use buzz_host::api::{TeamMeta, TeamSnapshot};
use buzz_relay_client::catalog::{Publication, TeamPublication, unsupported_transport_message};
use buzz_relay_client::session::TeamsStatus;
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

pub struct TeamEditor {
    teams: Entity<TeamsState>,
    catalog: Entity<CatalogState>,
    /// The saved team being edited; `None` adds one.
    initial: Option<TeamEntry>,
    id: String,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    instructions: Entity<TextareaState>,
    search: Entity<InputState>,
    agents: Vec<String>,
    /// The saved portable definition, or the text typed for a new one.
    portable: Option<TeamSnapshot>,
    loading: bool,
    /// Names this save's payload, so a retry reuses its chunks.
    revision: String,
    prepared: Option<TeamSnapshot>,
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
        let description = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("Optional description for this team.")
        });
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Optional instructions applied to every deployed team member.")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Name or npub"));
        if let Some(entry) = &initial {
            let value = entry.team.name.clone();
            name.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        name.update(cx, |state, cx| state.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe(&name, |this: &mut Self, _, _: &InputEvent, cx| {
                this.changed(cx)
            }),
            cx.subscribe(&description, |this: &mut Self, _, _: &InputEvent, cx| {
                this.text_changed(cx)
            }),
            cx.subscribe(&instructions, |this: &mut Self, _, _: &InputEvent, cx| {
                this.text_changed(cx)
            }),
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.observe(&teams, |_, _, cx| cx.notify()),
            cx.observe(&catalog, |_, _, cx| cx.notify()),
        ];
        if initial.is_none() {
            catalog.read(cx).catalog.retain();
        }
        teams.read(cx).kit.ensure();
        let mut editor = Self {
            id: initial
                .as_ref()
                .map(|entry| entry.team.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            agents: initial
                .as_ref()
                .map(|entry| entry.team.agents.clone())
                .unwrap_or_default(),
            loading: initial
                .as_ref()
                .is_some_and(|entry| entry.team.portable.is_some()),
            teams,
            catalog,
            initial,
            name,
            description,
            instructions,
            search,
            portable: None,
            revision: uuid::Uuid::new_v4().to_string(),
            prepared: None,
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
        editor.load_portable(window, cx);
        editor
    }

    /// Loads a portable team's saved definition for its text fields.
    fn load_portable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.initial.clone().filter(|e| e.team.portable.is_some()) else {
            return;
        };
        let kit = self.teams.read(cx).kit.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<TeamSnapshot, String> = async {
                let loaded = kit.load_team(&entry.team).await?;
                let host = cx
                    .update(|_, cx| team_ops::import_host(cx))
                    .ok()
                    .flatten()
                    .ok_or("Team preview is unavailable")?;
                let snapshot = team_ops::preview(&host, loaded.to_string(), cx).await?;
                if snapshot.members.len() != entry.team.agents.len() {
                    return Err("Portable team members do not match their definitions".into());
                }
                Ok(snapshot)
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(snapshot) => {
                        let (description, instructions) = (
                            snapshot.team.description.clone().unwrap_or_default(),
                            snapshot.team.instructions.clone().unwrap_or_default(),
                        );
                        this.description
                            .update(cx, |state, cx| state.set_value(description, window, cx));
                        this.instructions
                            .update(cx, |state, cx| state.set_value(instructions, window, cx));
                        this.portable = Some(snapshot);
                        this.loading = false;
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Any change makes a new save.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.revision = uuid::Uuid::new_v4().to_string();
        self.prepared = None;
        cx.notify();
    }

    fn text_changed(&mut self, cx: &mut Context<Self>) {
        let description = self.description.read(cx).value().to_string();
        let instructions = self.instructions.read(cx).value().to_string();
        let name = self.name.read(cx).value().to_string();
        let portable = self.portable.get_or_insert_with(|| TeamSnapshot {
            format: "buzz-team-snapshot".into(),
            version: 1,
            team: TeamMeta::default(),
            members: Vec::new(),
        });
        let changed = portable.team.description.as_deref().unwrap_or_default() != description
            || portable.team.instructions.as_deref().unwrap_or_default() != instructions;
        portable.team.name = name;
        portable.team.description = Some(description);
        portable.team.instructions = Some(instructions);
        if changed {
            self.changed(cx);
        }
    }

    fn draft_name(&self, cx: &App) -> String {
        self.name.read(cx).value().to_string()
    }

    fn has_portable_fields(&self) -> bool {
        self.portable.as_ref().is_some_and(|portable| {
            portable
                .team
                .description
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
                || portable
                    .team
                    .instructions
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
        })
    }

    fn saved_portable(&self) -> bool {
        self.initial
            .as_ref()
            .is_some_and(|entry| entry.team.portable.is_some())
    }

    /// A new team whose members all run here can carry a portable
    /// definition.
    fn can_capture(&self, cx: &App) -> bool {
        let community = self.teams.read(cx).community.clone();
        let local = community_agents(&community, cx);
        self.initial.is_none()
            && !self.agents.is_empty()
            && self
                .agents
                .iter()
                .all(|key| local.iter().any(|(_, agent)| agent.pubkey == *key))
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.draft_name(cx);
        if self.busy || self.loading || self.catalog_busy || name.trim().is_empty() {
            return;
        }
        let portable_save = self.saved_portable() || self.can_capture(cx);
        if !portable_save && self.has_portable_fields() {
            self.error = Some(
                "Description and team instructions require nonempty local team members".into(),
            );
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let teams = self.teams.read(cx);
        let (kit, community) = (teams.kit.clone(), teams.community.clone());
        let team = Team {
            id: self.id.clone(),
            name: name.clone(),
            agents: self.agents.clone(),
            portable: None,
        };
        let expected = self.initial.as_ref().map(|entry| entry.event_id.clone());
        let previous = self
            .initial
            .as_ref()
            .filter(|entry| entry.team.portable.is_some())
            .map(|entry| entry.team.agents.clone())
            .unwrap_or_default();
        let portable = self.portable.clone();
        let prepared = self.prepared.clone();
        let revision = self.revision.clone();
        cx.spawn(async move |this, cx| {
            let result: Result<(), String> = async {
                if !portable_save {
                    kit.save(team, expected, false).await?;
                    return Ok(());
                }
                let snapshot = match prepared {
                    Some(prepared) => prepared,
                    None => {
                        let loaded = portable.as_ref().filter(|p| !p.members.is_empty());
                        let added: Vec<String> = team
                            .agents
                            .iter()
                            .filter(|key| loaded.is_none() || !previous.contains(key))
                            .cloned()
                            .collect();
                        let captured = if added.is_empty() {
                            None
                        } else {
                            let meta = TeamMeta {
                                name: team.name.clone(),
                                ..Default::default()
                            };
                            Some(capture(&community, meta, &added, cx).await?)
                        };
                        let members = team
                            .agents
                            .iter()
                            .map(|key| {
                                let saved = previous
                                    .iter()
                                    .position(|known| known == key)
                                    .and_then(|index| loaded?.members.get(index).cloned());
                                saved
                                    .or_else(|| {
                                        let index = added.iter().position(|k| k == key)?;
                                        captured.as_ref()?.members.get(index).cloned()
                                    })
                                    .ok_or_else(|| {
                                        "A selected agent has no portable definition".to_string()
                                    })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let mut meta = portable.map(|p| p.team).unwrap_or_default();
                        meta.name = team.name.clone();
                        TeamSnapshot {
                            format: "buzz-team-snapshot".into(),
                            version: 1,
                            team: meta,
                            members,
                        }
                    }
                };
                let _ = this.update(cx, |this, _| this.prepared = Some(snapshot.clone()));
                let value = serde_json::to_value(&snapshot).map_err(|e| e.to_string())?;
                kit.save_portable(team, value, expected, revision).await?;
                Ok(())
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(Dismissed),
                    Err(error) => {
                        this.error = Some(error.into());
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
        let show_text = self.saved_portable() || self.can_capture(cx) || self.has_portable_fields();
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
            .when(show_text, |this| {
                this.child(field(
                    "Description",
                    None,
                    TextArea::new(&self.description)
                        .rows(3)
                        .surface(Surface::Floating),
                    &c,
                ))
                .child(field(
                    "Team Instructions",
                    None,
                    TextArea::new(&self.instructions)
                        .rows(3)
                        .surface(Surface::Floating),
                    &c,
                ))
            })
            .when(self.loading, |this| {
                this.child(div().text_body_sm().child("Loading team definitions…"))
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
