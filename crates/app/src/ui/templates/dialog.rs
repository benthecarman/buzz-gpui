//! The template and team editor (`ChannelTemplatesDialog.tsx`): name,
//! description and lineup for a template; name and agents for a team.
//! A team's instructions live in their own record and are edited from
//! the Agents page (`agents/team_editor.rs`).

use super::{Catalog, field, line, watch_kit};
use crate::community::Community;
use crate::ui::overlay::{self, Dismiss};
use buzz_relay_client::EventId;
use buzz_relay_client::kit::{KitStatus, KitValue, Lineup, Team, Template};
use buzz_ui::{Button, Dialog, DialogSize, Surface, TextField, tokens::space};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// The dialog closed, saved or not.
pub struct Closed;

pub struct TemplateDialog {
    pub(super) community: Entity<Community>,
    /// What is being edited; its lineup is edited in place.
    pub(super) draft: KitValue,
    expected: Option<EventId>,
    notice: Option<SharedString>,
    name: Entity<InputState>,
    description: Entity<InputState>,
    pub(super) canvas: Entity<TextareaState>,
    pub(super) search: Entity<InputState>,
    /// Open disclosures of the template fields.
    pub(super) members_open: bool,
    pub(super) canvas_open: bool,
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
        let subscriptions = vec![
            cx.subscribe(&name, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    cx.notify();
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
        ];
        let kit = watch_kit(&community, cx);
        Self {
            community,
            draft: initial,
            expected,
            notice: notice.map(Into::into),
            name,
            description,
            canvas,
            search,
            members_open: lineup_open.0,
            canvas_open: lineup_open.1,
            busy: false,
            error: None,
            _kit: kit,
            _subscriptions: subscriptions,
        }
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
            KitValue::Team(team) => team.agents = agents,
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

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
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
        // A team imported from a file keeps its manifest as it is.
        if let KitValue::Team(team) = &value
            && team.portable.is_some()
            && team.agents.is_empty()
        {
            self.error = Some("A team imported from a file needs at least one member.".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = session.kit_save(value, expected, false).await;
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
                    .disabled(name_blank || catalog.kit.status != KitStatus::Ready)
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
