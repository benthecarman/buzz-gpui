//! Importing agents from an earlier Buzz install on a host: buzz-app's
//! `AgentImport.tsx`.
//!
//! Two forms. An "Available to import" card opens the single-agent dialog,
//! "Import {name}?". A host whose imported agents still lack their old team
//! instructions shows the list form inside "Repair team import from another
//! installation". An old agent without an owner attestation gets one signed
//! here, with the human key, when the host asks during the import.

use super::editor::Dismissed;
use super::form::field;
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::ui::overlay::{self, Align};
use buzz_host::api::{
    CloneSettings, ImportCandidate, ImportPreview, ImportSource, Request, Response,
};
use buzz_ui::{
    ActiveTheme, Button, Dialog, Menu, MenuItem, MenuSize, Select, Surface, TextField, TextRole,
    TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;

/// An agent was imported; the page hands its card the focus.
pub struct Imported(pub String);

/// Clone this record into a new agent here.
pub struct Cloned(pub CloneSettings);

const STRIPPED_TEAM_INSTRUCTIONS: &str = "Old team instructions saved inside this agent's prompt \
                                          are left out, so the agent doesn't get them twice.";

pub fn source_label(source: ImportSource) -> &'static str {
    match source {
        ImportSource::Installed => "Installed Buzz",
        ImportSource::Development => "Development Buzz",
    }
}

enum Mode {
    /// One identity, from an "Available to import" card.
    Single { pubkey: String, name: String },
    /// Every candidate; with `repair_only`, just the agents whose team
    /// instructions were never imported.
    List { repair_only: bool },
}

pub struct ImportDialog {
    host: Entity<HostConnection>,
    mode: Mode,
    source: ImportSource,
    source_open: bool,
    destination: Entity<InputState>,
    initial_destination: String,
    preview: Option<ImportPreview>,
    previewing: bool,
    importing: bool,
    repaired: Option<String>,
    error: Option<SharedString>,
    write_failed: bool,
    generation: u64,
    options_open: bool,
    another_source_open: bool,
    /// Candidates whose "Identity" detail is open.
    identities_open: HashSet<String>,
    _subscription: Subscription,
}

impl EventEmitter<Dismissed> for ImportDialog {}
impl EventEmitter<Imported> for ImportDialog {}
impl EventEmitter<Cloned> for ImportDialog {}

impl ImportDialog {
    fn new(
        host: Entity<HostConnection>,
        mode: Mode,
        source: ImportSource,
        destination: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("https://community.example");
            state.set_value(destination.clone(), window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                // A new destination needs a new preview.
                this.generation += 1;
                this.preview = None;
                this.previewing = false;
                this.error = None;
            }
            cx.notify();
        });
        let options_open = matches!(mode, Mode::Single { .. }) || destination.is_empty();
        let mut dialog = Self {
            host,
            mode,
            source,
            source_open: false,
            destination: input,
            initial_destination: destination,
            preview: None,
            previewing: false,
            importing: false,
            repaired: None,
            error: None,
            write_failed: false,
            generation: 0,
            options_open,
            another_source_open: false,
            identities_open: HashSet::new(),
            _subscription: subscription,
        };
        dialog.load(cx);
        dialog
    }

    /// "Import {name}?" for one identity old Buzz holds.
    pub fn single(
        host: Entity<HostConnection>,
        pubkey: String,
        name: String,
        source: ImportSource,
        destination: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(
            host,
            Mode::Single { pubkey, name },
            source,
            destination,
            window,
            cx,
        )
    }

    /// The repair list for a host's imported agents.
    pub fn repair(
        host: Entity<HostConnection>,
        destination: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(
            host,
            Mode::List { repair_only: true },
            ImportSource::Installed,
            destination,
            window,
            cx,
        )
    }

    fn destination(&self, cx: &App) -> String {
        self.destination.read(cx).value().trim().to_string()
    }

    /// Reads the host's status, then previews the source. Refresh only
    /// reads; it never retries a failed operation.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.preview = None;
        self.error = None;
        self.write_failed = false;
        self.previewing = true;
        let refresh = self.host.update(cx, |host, cx| host.refresh(cx));
        let request = Request::ImportPreview {
            source: self.source,
            destination: self.destination(cx),
        };
        let host = self.host.clone();
        cx.spawn(async move |this, cx| {
            refresh.await;
            let response = host.update(cx, |host, cx| host.request(request, cx)).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.previewing = false;
                match response {
                    Ok(Response::ImportPreview(preview)) => this.preview = Some(preview),
                    _ => {
                        this.error = Some(
                            if matches!(this.mode, Mode::Single { .. }) {
                                "We couldn’t load this agent from the source."
                            } else {
                                "Could not load agents from old Buzz. Check Import options and try again."
                            }
                            .into(),
                        )
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The candidates to show: never a key the host already holds, except
    /// an import whose team instructions need repair.
    fn candidates(&self, cx: &App) -> Option<Vec<(ImportCandidate, bool)>> {
        let preview = self.preview.as_ref()?;
        let host = self.host.read(cx);
        let managed: HashSet<String> = host
            .agents
            .agents
            .iter()
            .map(|agent| agent.pubkey.to_lowercase())
            .collect();
        Some(
            preview
                .candidates
                .iter()
                .filter_map(|candidate| {
                    let saved = host.agent(&candidate.id);
                    let keep = match (&self.mode, saved) {
                        (Mode::Single { pubkey, .. }, _) if *pubkey != candidate.pubkey => false,
                        (Mode::Single { .. }, Some(_)) => false,
                        (Mode::List { .. }, Some(saved)) => saved.needs_team_import,
                        (Mode::List { repair_only: true }, None) => false,
                        (_, None) => !managed.contains(&candidate.pubkey.to_lowercase()),
                    };
                    keep.then(|| (candidate.clone(), saved.is_some()))
                })
                .collect(),
        )
    }

    fn commit(&mut self, candidate: ImportCandidate, repair: bool, cx: &mut Context<Self>) {
        let Some(preview) = self.preview.clone() else {
            return;
        };
        if self.importing || self.destination(cx).is_empty() || preview.token.is_empty() {
            return;
        }
        let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
            return;
        };
        let identity = identity.clone();
        let pubkeys: HashSet<String> = candidate
            .needs_attestation
            .then(|| candidate.pubkey.clone())
            .into_iter()
            .collect();
        let generation = self.generation;
        self.importing = true;
        cx.notify();
        let response = self.host.update(cx, |host, cx| {
            host.import(
                preview.token,
                vec![candidate.id.clone()],
                pubkeys,
                identity,
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let response = response.await;
            let _ = this.update(cx, |this, cx| {
                // Always clear it: a stale result is dropped, but the
                // import itself has finished.
                this.importing = false;
                cx.notify();
                if this.generation != generation {
                    return;
                }
                match response {
                    Ok(_) if repair => {
                        this.repaired = Some(candidate.name.clone());
                        this.load(cx);
                    }
                    Ok(_) => cx.emit(Imported(candidate.id.clone())),
                    Err(failure) => {
                        this.preview = None;
                        this.write_failed = true;
                        this.error = Some(failure.message.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn clone_record(&mut self, pubkey: String, cx: &mut Context<Self>) {
        let generation = self.generation;
        let response = self.host.update(cx, |host, cx| {
            host.request(
                Request::CloneSettings {
                    source: self.source,
                    pubkey,
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let response = response.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match response {
                    Ok(Response::CloneSettings(settings)) => cx.emit(Cloned(settings)),
                    _ => {
                        this.error = Some(
                            "Could not read clone settings. Reload the source and try again."
                                .into(),
                        );
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn busy(&self, cx: &App) -> bool {
        self.host.read(cx).control.busy
    }

    fn commit_available(&self, cx: &App) -> bool {
        let host = self.host.read(cx);
        host.status() == crate::hosts::control::Status::Ready
            && host.agents.import_available != Some(false)
    }

    fn source_select(&self, label: &'static str, window: &Window, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let mut menu = Menu::new().size(MenuSize::Wide);
        for source in [ImportSource::Installed, ImportSource::Development] {
            menu = menu.child(
                MenuItem::new(source_label(source), source_label(source))
                    .checked(self.source == source)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.source_open = false;
                        if this.source != source {
                            this.source = source;
                            this.load(cx);
                        }
                        cx.notify();
                    })),
            );
        }
        let dialog = cx.entity().downgrade();
        let dismiss: overlay::Dismiss = Rc::new(move |_, cx| {
            let _ = dialog.update(cx, |this, cx| {
                this.source_open = false;
                cx.notify();
            });
        });
        field(
            label,
            None,
            div()
                .relative()
                .w_full()
                .child(
                    Select::new("import-source")
                        .value(source_label(self.source))
                        .open(self.source_open)
                        .surface(Surface::Floating)
                        .disabled(self.importing || (self.busy(cx) && !self.previewing))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.source_open = !this.source_open;
                            cx.notify();
                        })),
                )
                .when(self.source_open, |this| {
                    this.child(overlay::dropdown(window, Align::Start, menu, dismiss))
                }),
            &c,
        )
    }

    /// A `<details>` summary that toggles its body.
    fn details(
        id: impl Into<ElementId>,
        summary: &'static str,
        open: bool,
        c: &buzz_ui::Colors,
        on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Stateful<Div> {
        super::snapshots::summary(id, summary, open, TextRole::BodySm, c)
            .text_color(c.text_subtle)
            .on_click(on_toggle)
    }

    fn source_details(&self, c: &buzz_ui::Colors) -> Option<Div> {
        let preview = self.preview.as_ref()?;
        let mut warnings: Vec<&String> = Vec::new();
        for warning in &preview.warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .pt(rems(0.75))
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(preview.source_path.clone())
                .children(
                    warnings
                        .into_iter()
                        .map(|warning| div().child(warning.clone())),
                ),
        )
    }

    fn render_single(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let candidates = self.candidates(cx);
        let candidate = candidates
            .as_ref()
            .and_then(|list| list.first().map(|(candidate, _)| candidate.clone()));
        let problem: Option<SharedString> = self.error.clone().or_else(|| {
            candidates
                .as_ref()
                .filter(|list| list.is_empty())
                .map(|_| "This agent is unavailable in this source or is already imported.".into())
        });
        let busy = self.busy(cx);
        let commit_available = self.commit_available(cx);
        let destination = self.destination(cx);
        let detail = |label: &'static str, value: AnyElement| {
            div()
                .flex()
                .gap(rems(1.))
                .text_body_sm()
                .child(div().w(rems(6.5)).text_color(c.text_subtle).child(label))
                .child(div().flex_1().min_w_0().child(value))
        };
        let destination_value: AnyElement = if !self.initial_destination.is_empty() {
            div().child(destination.clone()).into_any_element()
        } else {
            // Personal space has no selected community, so the user names one.
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(rems(0.5))
                .child(field(
                    "Destination community",
                    None,
                    TextField::new(&self.destination)
                        .surface(Surface::Floating)
                        .disabled(busy || self.importing),
                    &c,
                ))
                .child(
                    Button::new("use-destination", "Use destination")
                        .surface(Surface::Floating)
                        .disabled(
                            busy || self.importing || self.previewing || destination.is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                )
                .into_any_element()
        };
        let body = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .when_some(problem.clone(), |this, problem| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(rems(0.5))
                        .child(div().text_body_sm().child(problem))
                        .child(
                            Button::new(
                                "import-retry",
                                if self.write_failed {
                                    "Reload source"
                                } else {
                                    "Retry"
                                },
                            )
                            .surface(Surface::Floating)
                            .disabled(busy || self.importing)
                            .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(detail(
                        "Source",
                        div().child(source_label(self.source)).into_any_element(),
                    ))
                    .child(detail("Destination", destination_value)),
            )
            .when(
                candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.strips_team_instructions),
                |this| {
                    this.child(
                        div()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(STRIPPED_TEAM_INSTRUCTIONS),
                    )
                },
            )
            .child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("Stop the agent in the old Buzz installation before starting it here."),
            )
            .when(self.previewing, |this| {
                this.child(div().text_body_sm().child("Loading agent…"))
            })
            .when(!commit_available && problem.is_none(), |this| {
                this.child(div().child("Import is unavailable in this app session."))
            })
            .when(problem.is_some(), |this| {
                let open = self.another_source_open;
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .child(Self::details(
                            "another-source",
                            "Choose another source",
                            open,
                            &c,
                            cx.listener(|this, _, _, cx| {
                                this.another_source_open = !this.another_source_open;
                                cx.notify();
                            }),
                        ))
                        .when(open, |this| {
                            this.child(div().pt(rems(0.75)).child(self.source_select(
                                "Source library",
                                window,
                                cx,
                            )))
                            .children(self.source_details(&c))
                        }),
                )
            });
        let can_import = !busy
            && !self.importing
            && !self.previewing
            && commit_available
            && !destination.is_empty()
            && candidate.is_some()
            && self.preview.as_ref().is_some_and(|p| !p.token.is_empty());
        let mut dialog = Dialog::new("import-agent", format!("Import {name}?"))
            .on_close(cx.listener(|this, _, _, cx| {
                if !this.importing {
                    cx.emit(Dismissed)
                }
            }))
            .child(body)
            .footer(
                Button::new("import-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(busy || self.importing)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
            )
            .footer(
                Button::new(
                    "import-commit",
                    if self.importing {
                        "Importing…"
                    } else {
                        "Import agent"
                    },
                )
                .prominent()
                .disabled(!can_import)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(candidate) = candidate.clone() {
                        this.commit(candidate, false, cx);
                    }
                })),
            );
        if problem.is_none() {
            dialog = dialog.description(format!(
                "This copies {name}’s identity and settings to the destination community. The \
                 agent will remain stopped."
            ));
        }
        dialog.into_any_element()
    }

    fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let busy = self.busy(cx);
        let destination = self.destination(cx);
        let commit_available = self.commit_available(cx);
        let candidates = self.candidates(cx);
        let token = self.preview.as_ref().is_some_and(|p| !p.token.is_empty());
        let note = |text: String| div().text_body_sm().text_color(c.text_subtle).child(text);
        let mut section = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .pt(rems(0.75))
            .child(note(
                "Import copies the existing key and settings from the selected installation into \
                 the chosen community. It leaves the agent stopped. Repair team import adds \
                 missing team instructions to an existing import without replacing its identity \
                 or edited settings. Neither action starts an agent. Stop the old agent and \
                 disable automatic startup there before starting it here."
                    .into(),
            ));
        if !destination.is_empty() {
            section = section.child(
                div()
                    .text_body_sm()
                    .child(format!("Community: {destination}")),
            );
        } else {
            section = section.child(note(
                "These identities are saved on this computer. Choose a destination in Import \
                 options before importing; browsing does not need a connection."
                    .into(),
            ));
        }
        if !commit_available {
            section = section.child(div().child("Import is unavailable in this app session."));
        }
        if let Some(name) = &self.repaired {
            section = section.child(div().child(format!(
                "Team instructions imported for {name}. Use Start when ready to hand over from \
                 old Buzz."
            )));
        }
        if self.previewing {
            section = section.child(div().child("Loading agents from old Buzz…"));
        }
        if let Some(error) = self.error.clone() {
            section = section.child(
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(rems(0.5))
                    .child(div().child(error))
                    .child(
                        Button::new("import-list-retry", "Retry")
                            .surface(Surface::Floating)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                    ),
            );
        }
        if let Some(candidates) = candidates {
            if candidates.is_empty() {
                section = section.child(div().child(if destination.is_empty() {
                    "No agents in this local library."
                } else {
                    "No agents left to import or repair from this library for this community."
                }));
            }
            for (index, (candidate, repair)) in candidates.into_iter().enumerate() {
                let open = self.identities_open.contains(&candidate.id);
                let toggle_id = candidate.id.clone();
                let clone_key = candidate.pubkey.clone();
                let commit_with = candidate.clone();
                section = section.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap(rems(0.75))
                        .pt(rems(0.75))
                        .border_t_1()
                        .border_color(c.border_standard)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .gap(rems(0.25))
                                .child(div().text_label_sm().child(candidate.name.clone()))
                                .child(note(
                                    if repair {
                                        "Team instructions not imported"
                                    } else {
                                        "Not imported"
                                    }
                                    .into(),
                                ))
                                .when(!repair && candidate.strips_team_instructions, |this| {
                                    this.child(note(STRIPPED_TEAM_INSTRUCTIONS.into()))
                                })
                                .child(Self::details(
                                    ("import-identity", index),
                                    "Identity",
                                    open,
                                    &c,
                                    cx.listener(move |this, _, _, cx| {
                                        if !this.identities_open.remove(&toggle_id) {
                                            this.identities_open.insert(toggle_id.clone());
                                        }
                                        cx.notify();
                                    }),
                                ))
                                .when(open, |this| {
                                    this.child(
                                        div()
                                            .text_mono()
                                            .text_color(c.text_subtle)
                                            .child(candidate.pubkey.clone()),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .gap(rems(0.5))
                                .when(!repair, |this| {
                                    this.child(
                                        Button::new(
                                            ("clone-candidate", index),
                                            "Clone to this community",
                                        )
                                        .surface(Surface::Floating)
                                        .disabled(busy || self.previewing || destination.is_empty())
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.clone_record(clone_key.clone(), cx)
                                            }),
                                        ),
                                    )
                                })
                                .child(
                                    Button::new(
                                        ("import-candidate", index),
                                        if repair {
                                            "Repair team import"
                                        } else {
                                            "Import"
                                        },
                                    )
                                    .surface(Surface::Floating)
                                    .disabled(
                                        busy || self.previewing
                                            || self.importing
                                            || !commit_available
                                            || destination.is_empty()
                                            || !token,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.commit(commit_with.clone(), repair, cx)
                                        },
                                    )),
                                ),
                        ),
                );
            }
        }
        let open = self.options_open;
        section = section.child(
            div()
                .flex()
                .flex_col()
                .child(Self::details(
                    "import-options",
                    "Import options",
                    open,
                    &c,
                    cx.listener(|this, _, _, cx| {
                        this.options_open = !this.options_open;
                        cx.notify();
                    }),
                ))
                .when(open, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(1.))
                            .pt(rems(1.))
                            .child(self.source_select("Source library", window, cx))
                            .child(field(
                                "Destination community",
                                None,
                                TextField::new(&self.destination)
                                    .surface(Surface::Floating)
                                    .disabled(busy || self.importing),
                                &c,
                            ))
                            .child(
                                div().flex().child(
                                    Button::new("load-agents", "Load agents")
                                        .surface(Surface::Floating)
                                        .disabled(busy || self.previewing)
                                        .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                                ),
                            ),
                    )
                    .children(self.source_details(&c))
                }),
        );
        section.into_any_element()
    }
}

impl Render for ImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.mode {
            Mode::Single { name, .. } => {
                let name = name.clone();
                self.render_single(name, window, cx)
            }
            Mode::List { .. } => self.render_list(window, cx),
        }
    }
}
