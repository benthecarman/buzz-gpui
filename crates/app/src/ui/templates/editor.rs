//! The template selector in "Create a channel" (`TemplateEditor.tsx`):
//! choosing a saved template copies its lineup into the draft once,
//! resolved to the agents available now. Replacing a chosen setup asks
//! first.

use super::{Catalog, field, watch_kit};
use crate::community::Community;
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_relay_client::kit::setup::ChannelSetup;
use buzz_relay_client::kit::{self, Lineup};
use buzz_ui::{
    ActiveTheme, Button, Dialog, Menu, MenuItem, Select, Surface, TypeStyled, tokens::space,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

const UNAVAILABLE: &str = "Template unavailable. Choose another or None.";

/// The setup a template put in the draft (`TemplateDraft`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemplateDraft {
    pub template_id: String,
    pub lineup: Lineup,
    /// The lineup's agents, resolved when it was chosen.
    pub agents: Vec<String>,
    /// Why this setup cannot be accepted now.
    pub problem: Option<String>,
}

/// The draft changed.
pub struct Changed;

pub struct TemplateEditor {
    community: Entity<Community>,
    draft: Option<TemplateDraft>,
    select_open: bool,
    /// A template waiting for "Replace setup".
    replacement: Option<String>,
    _kit: Task<()>,
    _community: Subscription,
}

impl EventEmitter<Changed> for TemplateEditor {}

impl TemplateEditor {
    pub fn new(community: Entity<Community>, cx: &mut Context<Self>) -> Self {
        Self {
            _kit: watch_kit(&community, cx),
            _community: cx.observe(&community, |_, _, cx| cx.notify()),
            community,
            draft: None,
            select_open: false,
            replacement: None,
        }
    }

    pub fn draft(&self) -> Option<&TemplateDraft> {
        self.draft.as_ref()
    }

    /// Something was chosen (`setupKey` differs from the opening one).
    pub fn dirty(&self) -> bool {
        self.draft.as_ref().is_some_and(|d| {
            !d.template_id.is_empty() || !d.lineup.is_empty() || !d.agents.is_empty()
        })
    }

    /// The setup to create with, or the reason it cannot be accepted.
    pub fn setup(&self) -> Result<Option<ChannelSetup>, String> {
        let Some(draft) = &self.draft else {
            return Ok(None);
        };
        if let Some(problem) = &draft.problem {
            return Err(problem.clone());
        }
        Ok(Some(ChannelSetup {
            canvas: draft.lineup.canvas.clone(),
            agents: draft.agents.clone(),
            group_id: String::new(),
            group_source: None,
            template_id: draft.template_id.clone(),
        }))
    }

    fn set(&mut self, draft: Option<TemplateDraft>, cx: &mut Context<Self>) {
        self.draft = draft;
        cx.emit(Changed);
        cx.notify();
    }

    /// `publish`: resolves a lineup against the catalog now.
    fn publish(&mut self, template_id: String, lineup: Lineup, cx: &mut Context<Self>) {
        let catalog = Catalog::read(&self.community, cx);
        let resolved = if !catalog.ready() {
            Err("Load the template catalog before accepting setup.".to_owned())
        } else if (!lineup.team_ids.is_empty() || !lineup.agents.is_empty())
            && !catalog.agents_ready
        {
            Err("Load available agents before accepting this lineup.".to_owned())
        } else {
            kit::resolve_lineup(&lineup, &catalog.kit.entries, &catalog.agents)
                .map(|agents| agents.into_iter().map(|a| a.pubkey).collect())
        };
        let (agents, problem) = match resolved {
            Ok(agents) => (agents, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        self.set(
            Some(TemplateDraft {
                template_id,
                lineup,
                agents,
                problem,
            }),
            cx,
        );
    }

    /// `apply`: copies a template's lineup, or clears it for None.
    fn apply(&mut self, id: String, cx: &mut Context<Self>) {
        if id.is_empty() {
            self.set(Some(TemplateDraft::default()), cx);
            return;
        }
        let catalog = Catalog::read(&self.community, cx);
        let template = kit::templates(&catalog.kit.entries)
            .into_iter()
            .find(|t| t.id == id)
            .cloned();
        match template {
            Some(template) => self.publish(id, template.lineup(), cx),
            None => self.set(
                Some(TemplateDraft {
                    template_id: id,
                    problem: Some(UNAVAILABLE.into()),
                    ..TemplateDraft::default()
                }),
                cx,
            ),
        }
    }

    /// `choose`: asks before replacing a setup that adds something.
    fn choose(&mut self, id: String, cx: &mut Context<Self>) {
        self.select_open = false;
        if self.draft.as_ref().is_some_and(|d| !d.lineup.is_empty()) {
            self.replacement = Some(id);
            cx.notify();
        } else {
            self.apply(id, cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let session = self.community.read(cx).session.clone();
        cx.background_spawn(async move {
            let _ = session.kit_refresh().await;
        })
        .detach();
    }

    fn select(&self, catalog: &Catalog, window: &Window, cx: &mut Context<Self>) -> Div {
        let templates = kit::templates(&catalog.kit.entries);
        let selected = self
            .draft
            .as_ref()
            .map(|d| d.template_id.clone())
            .unwrap_or_default();
        let mut options: Vec<(String, String)> =
            vec![(String::new(), "None — blank channel".into())];
        options.extend(templates.iter().map(|t| (t.id.clone(), t.name.clone())));
        if !selected.is_empty() && !templates.iter().any(|t| t.id == selected) {
            options.push((selected.clone(), "Unavailable template".into()));
        }
        let label = options
            .iter()
            .find(|(id, _)| *id == selected)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let mut root = div().relative().w_full().child(
            Select::new("template-select")
                .value(label)
                .open(self.select_open)
                .surface(Surface::Floating)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select_open = !this.select_open;
                    cx.notify();
                })),
        );
        if self.select_open {
            let items = options.into_iter().map(|(id, label)| {
                let checked = id == selected;
                MenuItem::new(SharedString::from(format!("template-option-{id}")), label)
                    .checked(checked)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(id.clone(), cx)))
            });
            let weak = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.select_open = false;
                    cx.notify();
                });
            });
            root = root.child(overlay::dropdown(
                window,
                Align::Start,
                div().min_w(rems(20.)).child(Menu::new().children(items)),
                dismiss,
            ));
        }
        root
    }

    fn replace_dialog(&self, cx: &mut Context<Self>) -> Dialog {
        let weak = cx.entity().downgrade();
        let cancel = move |_: &mut Context<Self>| {
            let weak = weak.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    this.replacement = None;
                    cx.notify();
                });
            }
        };
        Dialog::new("replace-setup", "Replace channel setup?")
            .description(
                "This replaces the current teams, agents, and starting Canvas in your draft.",
            )
            .on_close(cancel(cx))
            .footer(
                Button::new("replace-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .on_click(cancel(cx)),
            )
            .footer(
                Button::new("replace-confirm", "Replace setup")
                    .prominent()
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(id) = this.replacement.take() {
                            this.apply(id, cx);
                        }
                    })),
            )
    }
}

impl Render for TemplateEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let catalog = Catalog::read(&self.community, cx);
        let status = catalog.status("Loading templates and available agents…");
        let recheck = self
            .draft
            .as_ref()
            .filter(|d| d.problem.is_some() || !d.lineup.team_ids.is_empty())
            .cloned();
        let replace = self.replacement.is_some().then(|| {
            let weak = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.replacement = None;
                    cx.notify();
                });
            });
            overlay::modal(window, self.replace_dialog(cx), Some(dismiss))
        });
        div()
            .flex()
            .flex_col()
            .gap(space::S3)
            .child(field(
                "Template",
                None,
                self.select(&catalog, window, cx),
                cx,
            ))
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(space::S2)
                        .child(div().text_body().text_color(c.text_subtle).child(status))
                        .child(
                            Button::new("reload-templates", "Reload templates and agents")
                                .surface(Surface::Floating)
                                .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                        ),
                )
            })
            .when_some(recheck, |this, draft| {
                let label = if draft.problem.is_some() {
                    "Recheck selected setup"
                } else {
                    "Refresh selected team membership"
                };
                this.child(
                    div().flex().child(
                        Button::new("recheck-setup", label)
                            .surface(Surface::Floating)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if draft.problem.as_deref() == Some(UNAVAILABLE) {
                                    this.apply(draft.template_id.clone(), cx);
                                } else {
                                    this.publish(
                                        draft.template_id.clone(),
                                        draft.lineup.clone(),
                                        cx,
                                    );
                                }
                            })),
                    ),
                )
            })
            .children(replace)
    }
}

/// `recoverySummary`: what a pending or retained setup adds.
pub fn setup_summary(agents: usize, cx: &App) -> Div {
    let c = cx.colors();
    div()
        .text_body()
        .text_color(c.text_standard)
        .child(match agents {
            0 => "Only you.".to_owned(),
            1 => "1 selected agent.".to_owned(),
            n => format!("{n} selected agents."),
        })
}
