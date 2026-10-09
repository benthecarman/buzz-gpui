//! Importing agents from an earlier Buzz install on a host: the Add agent
//! dialog's "Import from Buzz" pane (ui.md §5a.7 sidebar, buzz-app's
//! `AgentImport.tsx`).
//!
//! The host reads the old library; candidates are listed in the dialog's
//! sidebar like catalog agents. An old agent without an owner attestation
//! gets one signed here, with the human key, when the host asks during the
//! import (see `HostConnection::import`).

use super::editor::{AgentEditor, Dismissed, connected};
use super::form::{field, note};
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::ui::overlay::{self, Align};
use buzz_host::api::{ImportCandidate, ImportPreview, ImportSource, Request, Response};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, Icon, IconName, Menu, MenuItem, MenuSize,
    NavigationItem, Select, Surface, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;

pub struct ImportPane {
    editor: WeakEntity<AgentEditor>,
    host: Option<Entity<HostConnection>>,
    community: Option<String>,
    source: ImportSource,
    source_open: bool,
    preview: Option<ImportPreview>,
    loading: bool,
    busy: bool,
    error: Option<SharedString>,
    status: Option<SharedString>,
    /// The candidate shown, by ID.
    pub selected: Option<String>,
    generation: u64,
}

fn source_label(source: ImportSource) -> &'static str {
    match source {
        ImportSource::Installed => "Installed Buzz",
        ImportSource::Development => "Development Buzz",
    }
}

impl ImportPane {
    pub fn new(editor: WeakEntity<AgentEditor>, _: &mut Window, _: &mut Context<Self>) -> Self {
        Self {
            editor,
            host: None,
            community: None,
            source: ImportSource::Installed,
            source_open: false,
            preview: None,
            loading: false,
            busy: false,
            error: None,
            status: None,
            selected: None,
            generation: 0,
        }
    }

    /// Previews for this host and community, unless already shown.
    pub fn ensure(
        &mut self,
        host: Option<Entity<HostConnection>>,
        community: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let same_host =
            self.host.as_ref().map(Entity::entity_id) == host.as_ref().map(Entity::entity_id);
        if same_host && self.community == community && (self.preview.is_some() || self.loading) {
            return;
        }
        self.host = host;
        self.community = community;
        self.status = None;
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.preview = None;
        self.selected = None;
        self.error = None;
        let Some(host) = self.host.clone() else {
            self.error = Some("Choose a host first.".into());
            return cx.notify();
        };
        if !connected(&host, cx) {
            self.error = Some(format!("{} is not connected.", host.read(cx).label).into());
            return cx.notify();
        }
        let Some(destination) = self.community.clone() else {
            self.error = Some("Choose a community for the imported agents first.".into());
            return cx.notify();
        };
        self.loading = true;
        let generation = self.generation;
        let request = Request::ImportPreview {
            source: self.source,
            destination,
        };
        let response = host.update(cx, |host, cx| host.request(request, cx));
        cx.spawn(async move |this, cx| {
            let response = response.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match response {
                    Ok(Response::ImportPreview(preview)) => this.preview = Some(preview),
                    Ok(_) => this.error = Some("The host sent an unexpected reply.".into()),
                    Err(error) => {
                        this.error = Some(
                            format!(
                                "Could not load agents from {}: {error}",
                                source_label(this.source)
                            )
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

    fn candidate(&self) -> Option<&ImportCandidate> {
        let id = self.selected.as_ref()?;
        self.preview
            .as_ref()?
            .candidates
            .iter()
            .find(|candidate| &candidate.id == id)
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        let (Some(candidate), Some(preview), Some(host)) = (
            self.candidate().cloned(),
            self.preview.clone(),
            self.host.clone(),
        ) else {
            return;
        };
        let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
            self.error = Some("Set up your identity first.".into());
            return cx.notify();
        };
        let identity = identity.clone();
        let pubkeys: HashSet<String> = candidate
            .needs_attestation
            .then(|| candidate.pubkey.clone())
            .into_iter()
            .collect();
        self.busy = true;
        self.error = None;
        self.status = Some(format!("Importing {}…", candidate.name).into());
        let response = host.update(cx, |host, cx| {
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
                this.busy = false;
                match response {
                    Ok(_) => {
                        // A preview imports once; show the fresh state.
                        this.load(cx);
                        this.status = Some(
                            format!(
                                "{} was imported. It stays stopped until you start it.",
                                candidate.name
                            )
                            .into(),
                        );
                    }
                    Err(error) => {
                        this.status = None;
                        this.error = Some(error.to_string().into());
                        // The failed commit used up the preview.
                        let selected = this.selected.clone();
                        this.load(cx);
                        this.selected = selected;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Candidates under "AGENTS" in the dialog's sidebar.
    pub fn sidebar(&self, this: Entity<Self>, cx: &App) -> Div {
        let c = cx.colors();
        let mut list = div().flex().flex_col().gap(rems(0.25));
        let Some(preview) = self.preview.as_ref().filter(|p| !p.candidates.is_empty()) else {
            return list;
        };
        list = list.child(
            div()
                .px(rems(0.75))
                .pt(rems(0.75))
                .pb(rems(0.25))
                .text_caption()
                .text_color(c.text_subtle)
                .child("AGENTS"),
        );
        for (index, candidate) in preview.candidates.iter().enumerate() {
            let id = candidate.id.clone();
            let pane = this.clone();
            let editor = self.editor.clone();
            list = list.child(
                NavigationItem::new(("import-candidate", index), candidate.name.clone())
                    .leading(Avatar::new(candidate.name.clone()).size(AvatarSize::Small))
                    .selected(self.selected.as_ref() == Some(&candidate.id))
                    .when(candidate.imported, |this| {
                        this.trailing(Icon::new(IconName::Check).size(14.).color(c.text_subtle))
                    })
                    .on_click(move |_, _, cx| {
                        pane.update(cx, |pane, cx| {
                            pane.selected = Some(id.clone());
                            pane.status = None;
                            pane.error = None;
                            cx.notify();
                        });
                        let _ = editor.update(cx, |_, cx| cx.notify());
                    }),
            );
        }
        list
    }

    pub fn footer(&self, this: Entity<Self>, cx: &App) -> Vec<AnyElement> {
        let editor = self.editor.clone();
        let mut buttons = vec![
            Button::new("cancel-import", "Close")
                .surface(Surface::Floating)
                .disabled(self.busy)
                .on_click(move |_, _, cx| {
                    let _ = editor.update(cx, |_, cx| cx.emit(Dismissed));
                })
                .into_any_element(),
        ];
        if let Some(candidate) = self.candidate() {
            let imported = candidate.imported;
            buttons.push(
                Button::new(
                    "commit-import",
                    if imported {
                        "Already on this host"
                    } else if self.busy {
                        "Importing…"
                    } else {
                        "Import agent"
                    },
                )
                .prominent()
                .loading(self.busy)
                .disabled(
                    imported
                        || self.loading
                        || self.preview.as_ref().is_none_or(|p| p.token.is_empty()),
                )
                .on_click(move |_, _, cx| this.update(cx, |pane, cx| pane.import(cx)))
                .into_any_element(),
            );
        }
        let _ = cx;
        buttons
    }

    fn source_field(&self, window: &Window, cx: &mut Context<Self>) -> Div {
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
                            this.status = None;
                            this.load(cx);
                        }
                        cx.notify();
                    })),
            );
        }
        let pane = cx.entity().downgrade();
        let dismiss: overlay::Dismiss = Rc::new(move |_, cx| {
            let _ = pane.update(cx, |this, cx| {
                this.source_open = false;
                cx.notify();
            });
        });
        field(
            "Source library",
            None,
            div()
                .relative()
                .w_full()
                .child(
                    Select::new("import-source")
                        .value(source_label(self.source))
                        .open(self.source_open)
                        .surface(Surface::Floating)
                        .disabled(self.busy)
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
}

impl Render for ImportPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let host = self
            .host
            .as_ref()
            .map(|host| host.read(cx).label.to_string())
            .unwrap_or_else(|| "the host".into());
        let community = self
            .community
            .clone()
            .unwrap_or_else(|| "the community".into());
        let mut body = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(div().text_label().child("Import from Buzz"))
            .child(note(
                format!(
                    "Agents from an earlier Buzz install on {host}. Imported agents join \
                     {community} and stay stopped until you start them."
                ),
                &c,
            ))
            .child(self.source_field(window, cx))
            .children(
                self.status
                    .clone()
                    .map(|status| div().text_body_sm().child(status)),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(rems(0.5))
                        .child(div().text_body_sm().text_color(c.text_danger).child(error))
                        .child(
                            Button::new("retry-import", "Retry")
                                .surface(Surface::Floating)
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                        ),
                )
            });
        if self.loading {
            body = body.child(div().text_body_sm().child("Loading agents from Buzz…"));
        } else if let Some(candidate) = self.candidate() {
            let detail = |label: &'static str, value: String| {
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.25))
                    .child(div().text_label_sm().child(label))
                    .child(div().text_mono().text_color(c.text_subtle).child(value))
            };
            body = body
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(rems(0.75))
                        .child(
                            Avatar::new(candidate.name.clone())
                                .squircle()
                                .size(AvatarSize::Large),
                        )
                        .child(div().text_label().child(candidate.name.clone())),
                )
                .child(detail("Public key", candidate.pubkey.clone()))
                .child(detail("Destination", candidate.relay_url.clone()))
                .when(candidate.strips_team_instructions, |this| {
                    this.child(note(
                        "Old team instructions saved inside this agent's prompt are left \
                         out, so the agent doesn't get them twice.",
                        &c,
                    ))
                })
                .when(candidate.needs_attestation && !candidate.imported, |this| {
                    this.child(note(
                        "Old Buzz saved no owner attestation for this agent. Importing signs \
                         one with your key, so the agent can start here.",
                        &c,
                    ))
                })
                .child(note(
                    if candidate.imported {
                        "This agent is already on this host."
                    } else {
                        "Stop the agent in the old Buzz installation before starting it here."
                    },
                    &c,
                ));
        } else if let Some(preview) = &self.preview {
            let available = preview.candidates.iter().filter(|c| !c.imported).count();
            body = body.child(div().text_body_sm().child(if preview.candidates.is_empty() {
                "No agents in this library.".to_string()
            } else {
                format!(
                    "{} agents found, {available} not yet on this host. Choose one on the left \
                     to review it.",
                    preview.candidates.len()
                )
            }));
            for warning in &preview.warnings {
                body = body.child(note(warning.clone(), &c));
            }
            body = body.child(
                div()
                    .text_mono()
                    .text_color(c.text_subtle)
                    .child(preview.source_path.clone()),
            );
        }
        body
    }
}
