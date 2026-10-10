//! The template and team editor (`ChannelTemplatesDialog.tsx`): name,
//! description and lineup for a template; name, agents and, for portable
//! teams, description and team instructions for a team.

use super::{Catalog, field, line, watch_kit};
use crate::community::Community;
use crate::ui::overlay::{self, Dismiss};
use buzz_relay_client::EventId;
use buzz_relay_client::kit::{KitStatus, KitValue, Lineup, Team, Template};
use buzz_ui::{Button, Dialog, DialogSize, Surface, TextArea, TextField, tokens::space};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde_json::{Value, json};
use std::rc::Rc;

/// The dialog closed, saved or not.
pub struct Closed;

pub struct TemplateDialog {
    pub(super) community: Entity<Community>,
    /// What is being edited; its lineup is edited in place.
    pub(super) draft: KitValue,
    initial: KitValue,
    expected: Option<EventId>,
    notice: Option<SharedString>,
    name: Entity<InputState>,
    description: Entity<InputState>,
    pub(super) canvas: Entity<TextareaState>,
    pub(super) search: Entity<InputState>,
    /// Open disclosures of the template fields.
    pub(super) members_open: bool,
    pub(super) canvas_open: bool,
    /// The loaded portable definition of a version 2 team.
    portable: Option<Value>,
    loading: bool,
    team_description: Entity<TextareaState>,
    team_instructions: Entity<TextareaState>,
    /// One revision per edit, kept across retries of the same save.
    revision: String,
    busy: bool,
    error: Option<SharedString>,
    _kit: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Closed> for TemplateDialog {}

impl TemplateDialog {
    pub fn new(
        community: Entity<Community>,
        initial: KitValue,
        expected: Option<EventId>,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let team = matches!(initial, KitValue::Team(_));
        let name = cx.new(|cx| {
            let state = InputState::new(window, cx);
            if team {
                state.placeholder("Engineering Squad")
            } else {
                state
            }
        });
        let description = cx.new(|cx| InputState::new(window, cx));
        let canvas = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("# Goal\n\n# Scope\n\n# Success criteria")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Name or npub"));
        let team_description = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("Optional description for this team.")
        });
        let team_instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Optional instructions applied to every deployed team member.")
        });
        name.update(cx, |state, cx| {
            state.set_value(initial.name().to_owned(), window, cx);
            state.focus(window, cx);
        });
        if let KitValue::Template(template) = &initial {
            description.update(cx, |state, cx| {
                state.set_value(template.description.clone(), window, cx)
            });
            canvas.update(cx, |state, cx| {
                state.set_value(template.canvas.clone(), window, cx)
            });
        }
        let lineup_open = match &initial {
            KitValue::Template(t) => (
                !t.team_ids.is_empty() || !t.agents.is_empty(),
                !t.canvas.is_empty(),
            ),
            _ => (false, false),
        };
        let edited = |this: &mut Self, cx: &mut Context<Self>| {
            this.revision = uuid::Uuid::new_v4().to_string();
            this.error = None;
            cx.notify();
        };
        let subscriptions = vec![
            cx.subscribe(&name, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    edited(this, cx);
                }
            }),
            cx.subscribe(&description, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&canvas, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&team_description, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    edited(this, cx);
                }
            }),
            cx.subscribe(
                &team_instructions,
                move |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        edited(this, cx);
                    }
                },
            ),
        ];
        let loading = matches!(&initial, KitValue::Team(team) if team.portable.is_some());
        let kit = watch_kit(&community, cx);
        let mut this = Self {
            community,
            draft: initial.clone(),
            initial,
            expected,
            notice: notice.map(Into::into),
            name,
            description,
            canvas,
            search,
            members_open: lineup_open.0,
            canvas_open: lineup_open.1,
            portable: None,
            loading,
            team_description,
            team_instructions,
            revision: uuid::Uuid::new_v4().to_string(),
            busy: false,
            error: None,
            _kit: kit,
            _subscriptions: subscriptions,
        };
        if loading {
            this.load_portable(window, cx);
        }
        this
    }

    /// Loads a portable team's definition before it can be edited.
    fn load_portable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let KitValue::Team(team) = self.initial.clone() else {
            return;
        };
        let session = self.community.read(cx).session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let agents = team.agents.len();
            let result = session.kit_load_team(team).await.and_then(|value| {
                let members = value["members"].as_array().map_or(0, Vec::len);
                if members != agents {
                    return Err("Portable team members do not match their definitions".into());
                }
                Ok(value)
            });
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(value) => {
                        let text =
                            |key: &str| value["team"][key].as_str().unwrap_or_default().to_owned();
                        let (description, instructions) =
                            (text("description"), text("instructions"));
                        this.team_description
                            .update(cx, |state, cx| state.set_value(description, window, cx));
                        this.team_instructions
                            .update(cx, |state, cx| state.set_value(instructions, window, cx));
                        this.portable = Some(value);
                        this.loading = false;
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn lineup(&self, cx: &App) -> Lineup {
        match &self.draft {
            KitValue::Template(template) => Lineup {
                canvas: self.canvas.read(cx).value().to_string(),
                ..template.lineup()
            },
            KitValue::Team(team) => Lineup {
                agents: team.agents.clone(),
                ..Lineup::default()
            },
            KitValue::Groups(_) => Lineup::default(),
        }
    }

    pub(super) fn set_agents(&mut self, agents: Vec<String>, cx: &mut Context<Self>) {
        match &mut self.draft {
            KitValue::Template(template) => template.agents = agents,
            KitValue::Team(team) => {
                team.agents = agents;
                self.revision = uuid::Uuid::new_v4().to_string();
            }
            KitValue::Groups(_) => {}
        }
        self.error = None;
        cx.notify();
    }

    pub(super) fn set_teams(&mut self, team_ids: Vec<String>, cx: &mut Context<Self>) {
        if let KitValue::Template(template) = &mut self.draft {
            template.team_ids = team_ids;
        }
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(Closed);
        }
    }

    fn portable_initial(&self) -> Option<&Team> {
        match &self.initial {
            KitValue::Team(team) if team.portable.is_some() => Some(team),
            _ => None,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let name = self.name.read(cx).value().to_string();
        let session = self.community.read(cx).session.clone();
        let expected = self.expected;
        let value = match &self.draft {
            KitValue::Template(template) => KitValue::Template(Template {
                name: name.clone(),
                description: self.description.read(cx).value().to_string(),
                canvas: self.canvas.read(cx).value().to_string(),
                ..template.clone()
            }),
            KitValue::Team(team) => KitValue::Team(Team {
                name: name.clone(),
                ..team.clone()
            }),
            KitValue::Groups(_) => return,
        };
        // A portable team saves its definitions with it.
        let portable = match (&value, self.portable_initial(), &self.portable) {
            (KitValue::Team(team), Some(previous), Some(snapshot)) => {
                let members: Option<Vec<Value>> = team
                    .agents
                    .iter()
                    .map(|key| {
                        let index = previous.agents.iter().position(|k| k == key)?;
                        snapshot["members"].get(index).cloned()
                    })
                    .collect();
                let Some(members) = members else {
                    self.error = Some("A selected agent has no portable definition".into());
                    cx.notify();
                    return;
                };
                let mut definition = snapshot["team"].clone();
                definition["name"] = json!(name);
                definition["description"] =
                    json!(self.team_description.read(cx).value().to_string());
                definition["instructions"] =
                    json!(self.team_instructions.read(cx).value().to_string());
                let snapshot = json!({
                    "format": "buzz-team-snapshot",
                    "version": 1,
                    "team": definition,
                    "members": members,
                });
                Some((
                    Team {
                        portable: None,
                        ..team.clone()
                    },
                    snapshot,
                ))
            }
            _ => None,
        };
        self.busy = true;
        self.error = None;
        let revision = self.revision.clone();
        cx.spawn(async move |this, cx| {
            let result = match portable {
                Some((team, snapshot)) => {
                    session
                        .kit_save_portable(team, snapshot, expected, revision)
                        .await
                }
                None => session.kit_save(value, expected, false).await,
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(_) => cx.emit(Closed),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Dialog {
        let catalog = Catalog::read(&self.community, cx);
        let team = matches!(self.draft, KitValue::Team(_));
        let noun = if team { "team" } else { "template" };
        let title = match (self.expected.is_some(), team) {
            (true, _) => format!("Edit {noun}"),
            (false, true) => "Add team".into(),
            (false, false) => "New template".into(),
        };
        let name_blank = self.name.read(cx).value().trim().is_empty();
        let weak = cx.entity().downgrade();
        let close = move |_: &mut Context<Self>| {
            let weak = weak.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| this.close(cx));
            }
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap(space::S4)
            .children(self.notice.clone().map(|text| line(text, false, cx)))
            .children(self.error.clone().map(|text| line(text, true, cx)))
            .when(catalog.kit.status != KitStatus::Ready, |this| {
                this.child(line(
                    catalog.kit.error.clone().unwrap_or_else(|| {
                        if catalog.kit.status == KitStatus::Unavailable {
                            "This host does not support saved templates.".into()
                        } else {
                            "Loading your saved templates…".into()
                        }
                    }),
                    false,
                    cx,
                ))
            })
            .child(field(
                "Name",
                None,
                TextField::new(&self.name).surface(Surface::Floating),
                cx,
            ));
        if team {
            if self.portable_initial().is_some() {
                body = body
                    .child(field(
                        "Description",
                        None,
                        TextArea::new(&self.team_description).surface(Surface::Floating),
                        cx,
                    ))
                    .child(field(
                        "Team Instructions",
                        None,
                        TextArea::new(&self.team_instructions).surface(Surface::Floating),
                        cx,
                    ));
            }
            if self.loading {
                body = body.child(line("Loading team definitions…", false, cx));
            }
            body = body.child(self.agent_selection(&catalog, window, cx));
        } else {
            body = body
                .child(field(
                    "Description",
                    Some("Optional. A short reminder of what this template is for."),
                    TextField::new(&self.description).surface(Surface::Floating),
                    cx,
                ))
                .child(self.template_fields(&catalog, window, cx));
        }
        Dialog::new("template-dialog", title)
            .size(DialogSize::Px(560.))
            .description(if team {
                "Choose agents to reuse together in future channels and @mentions. This team is private to you in this community."
            } else {
                "Save a starting setup for future channels. Applying a template copies it once; existing channels stay unchanged."
            })
            .on_close(close(cx))
            .child(body)
            .footer(
                Button::new("template-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(self.busy)
                    .on_click(close(cx)),
            )
            .footer(
                Button::new("template-save", format!("Save {noun}"))
                    .prominent()
                    .loading(self.busy)
                    .disabled(name_blank || catalog.kit.status != KitStatus::Ready || self.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }
}

impl Render for TemplateDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| this.close(cx));
        });
        let dialog = self.dialog(window, cx);
        overlay::modal(
            window,
            div()
                .id("template-dialog-scroll")
                .max_h(window.viewport_size().height - px(32.))
                .overflow_y_scroll()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.close(cx);
                    }
                }))
                .child(dialog),
            Some(dismiss),
        )
    }
}
