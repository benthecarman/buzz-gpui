//! The team dialogs: import a team snapshot (`TeamImportDialog.tsx`),
//! export a saved team (`TeamExportDialog.tsx`), deploy it to a channel
//! (`TeamDeployDialog.tsx`), and share it (`TeamShareDialog` in
//! `CommunityCatalog.tsx`, with the export entry of the full share dialog).

use super::catalog::{
    CatalogShareSwitch, CatalogState, Content, SharePending, TEAM_SHARE_DESCRIPTION,
    community_agents, default_policy, persona,
};
use super::editor::Dismissed;
use super::form::{Options, field};
use super::team_file::{self, Format};
use super::team_ops::{self, ImportOptions, MemoryLevel};
use super::teams::{ExportTeam, TeamsState};
use crate::community::{AppState, IdentityState};
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_host::api::TeamSnapshot;
use buzz_relay_client::catalog::{CatalogKind, TeamSource, team_catalog_content};
use buzz_relay_client::members::can_add_members;
use buzz_relay_client::teams::Team;
use buzz_ui::{ActiveTheme, Button, Checkbox, Dialog, MenuItem, Select, Surface, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// A labelled select: what it shows, and whether its menu is open.
struct SelectField {
    id: &'static str,
    label: &'static str,
    description: Option<SharedString>,
    value: SharedString,
    open: bool,
    disabled: bool,
}

impl SelectField {
    fn render(
        self,
        toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        options: Options,
        dismiss: Dismiss,
        window: &Window,
        cx: &App,
    ) -> Div {
        let c = cx.colors();
        field(
            self.label,
            self.description,
            div()
                .relative()
                .w_full()
                .child(
                    Select::new(self.id)
                        .value(self.value)
                        .open(self.open)
                        .surface(Surface::Floating)
                        .disabled(self.disabled)
                        .on_click(toggle),
                )
                .when(self.open, |this| {
                    this.child(overlay::dropdown(
                        window,
                        Align::Start,
                        div().min_w(rems(20.)).child(options),
                        dismiss,
                    ))
                }),
            c,
        )
    }
}

fn error_text(error: &Option<SharedString>, cx: &App) -> Option<Div> {
    let c = cx.colors();
    error
        .clone()
        .map(|error| div().text_body_sm().text_color(c.text_danger).child(error))
}

// ---------------------------------------------------------------------------
// Import

pub struct TeamImportDialog {
    teams: Entity<TeamsState>,
    snapshot: TeamSnapshot,
    restore: bool,
    keep: bool,
    open: bool,
    started: bool,
    busy: bool,
    done: bool,
    error: Option<SharedString>,
}

impl EventEmitter<Dismissed> for TeamImportDialog {}

impl TeamImportDialog {
    pub fn new(
        teams: Entity<TeamsState>,
        snapshot: TeamSnapshot,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Self {
        Self {
            teams,
            snapshot,
            restore: false,
            keep: false,
            open: false,
            started: false,
            busy: false,
            done: false,
            error: None,
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(host) = team_ops::import_host(cx) else {
            self.error = Some("Agent creation is unavailable.".into());
            cx.notify();
            return;
        };
        let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
            self.error = Some("Set up your identity first.".into());
            cx.notify();
            return;
        };
        let identity = identity.clone();
        self.started = true;
        self.busy = true;
        self.error = None;
        cx.notify();
        let teams = self.teams.read(cx);
        let (kit, community) = (teams.kit.clone(), teams.community.clone());
        let options = ImportOptions {
            destination: community.read(cx).origin.websocket().to_string(),
            owner: identity.pubkey_hex(),
            keep_allowlist: self.keep,
            restore_memory: self.restore,
        };
        let snapshot = self.snapshot.clone();
        cx.spawn(async move |this, cx| {
            let result =
                team_ops::import_team(host, kit, community, identity, snapshot, options, cx)
                    .await
                    .and_then(|imported| {
                        if imported.memory_failures.is_empty() {
                            Ok(())
                        } else {
                            Err(imported.memory_failures.join("\n"))
                        }
                    });
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.done = true,
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for TeamImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let snapshot = &self.snapshot;
        let close = cx.listener(|_, _, _, cx| cx.emit(Dismissed));
        let mut dialog = Dialog::new(
            "team-import",
            if self.done {
                "Team imported"
            } else {
                "Import team snapshot"
            },
        );
        if !self.busy {
            dialog = dialog.on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        }
        dialog = if self.done {
            dialog.footer(Button::new("close-import", "Close").on_click(close))
        } else {
            dialog
                .footer(
                    Button::new("cancel-import", "Cancel")
                        .outline()
                        .disabled(self.busy)
                        .on_click(close),
                )
                .footer(
                    Button::new("confirm-import", "Import")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                )
        };
        let memories = snapshot
            .members
            .iter()
            .any(|member| !member.memory.entries.is_empty());
        let allowlists = snapshot
            .members
            .iter()
            .any(|member| !member.definition.respond_to_allowlist.is_empty());
        let subtle = |text: String| div().text_body_sm().text_color(c.text_subtle).child(text);
        let this = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                this.open = false;
                cx.notify();
            });
        });
        let choice = |keep: bool, label: &'static str, cx: &mut Context<Self>| {
            MenuItem::new(if keep { "keep" } else { "clear" }, label)
                .checked(self.keep == keep)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.keep = keep;
                    this.open = false;
                    cx.notify();
                }))
        };
        let options = Options::new("allowlist-options")
            .child(choice(
                false,
                "Clear — start with empty allowlists (safer)",
                cx,
            ))
            .child(choice(
                true,
                "Keep — copy source allowlists to new members",
                cx,
            ));
        let members = snapshot.members.iter().map(|member| {
            div()
                .flex()
                .flex_col()
                .gap(rems(0.25))
                .child(
                    div()
                        .text_label_sm()
                        .child(member.profile.display_name.clone()),
                )
                .children(
                    member
                        .definition
                        .system_prompt
                        .clone()
                        .filter(|text| !text.is_empty())
                        .map(subtle),
                )
        });
        dialog.child(
            div()
                .flex()
                .flex_col()
                .gap(rems(0.75))
                .child(div().text_label_sm().child(snapshot.team.name.clone()))
                .children(
                    snapshot
                        .team
                        .description
                        .clone()
                        .filter(|t| !t.is_empty())
                        .map(subtle),
                )
                .children(
                    snapshot
                        .team
                        .instructions
                        .clone()
                        .filter(|t| !t.is_empty())
                        .map(subtle),
                )
                .child(subtle(
                    "A new team will be created with fresh keypairs for all members. The \
                     imported team is independent of the source — identity never travels."
                        .into(),
                ))
                .child(
                    div()
                        .text_label_sm()
                        .child(format!("Members ({})", snapshot.members.len())),
                )
                .children(members)
                .when(memories, |this| {
                    let entity = cx.entity().downgrade();
                    this.child(
                        Checkbox::new("restore-memories", self.restore)
                            .label("Restore member memories from this snapshot")
                            .disabled(self.started)
                            .surface(Surface::Floating)
                            .on_change(move |checked, _, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    this.restore = checked;
                                    cx.notify();
                                });
                            }),
                    )
                })
                .when(allowlists, |this| {
                    this.child(SelectField {
                        id: "allowlist-select",
                        label: "Respond-to allowlist",
                        description: Some(
                            "This snapshot includes source-environment pubkey allowlists for one \
                             or more members. Those identities are not meaningful on your relay."
                                .into(),
                        ),
                        value: if self.keep {
                            "Keep — copy source allowlists to new members".into()
                        } else {
                            "Clear — start with empty allowlists (safer)".into()
                        },
                        open: self.open,
                        disabled: self.started,
                    }
                    .render(
                        cx.listener(|this, _, _, cx| {
                            this.open = !this.open;
                            cx.notify();
                        }),
                        options,
                        dismiss.clone(),
                        window,
                        cx,
                    ))
                })
                .when(self.busy, |this| {
                    this.child(subtle("Creating team…".into()))
                })
                .children(error_text(&self.error, cx)),
        )
    }
}

// ---------------------------------------------------------------------------
// Export

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Memory,
    Format,
}

pub struct TeamExportDialog {
    teams: Entity<TeamsState>,
    team: Team,
    format: Format,
    memory: MemoryLevel,
    confirmed: bool,
    open: Option<Picker>,
    busy: bool,
    error: Option<SharedString>,
}

impl EventEmitter<Dismissed> for TeamExportDialog {}

fn memory_label(level: MemoryLevel) -> &'static str {
    match level {
        MemoryLevel::None => "Team only",
        MemoryLevel::Core => "Team + core memory",
        MemoryLevel::Everything => "Team + all memories",
    }
}

impl TeamExportDialog {
    pub fn new(teams: Entity<TeamsState>, team: Team, _: &mut Context<Self>) -> Self {
        Self {
            teams,
            team,
            format: Format::Png,
            memory: MemoryLevel::None,
            confirmed: false,
            open: None,
            busy: false,
            error: None,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || (self.memory != MemoryLevel::None && !self.confirmed) {
            return;
        }
        if self.team.portable.is_none() {
            self.error = Some("This team has no portable definition".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let teams = self.teams.read(cx);
        let (kit, community) = (teams.kit.clone(), teams.community.clone());
        let (team, memory, format) = (self.team.clone(), self.memory, self.format);
        let name = team_file::file_name(&team.name, format);
        let dev = std::env::var_os("BUZZ_DEV_SAVE_FILE").map(std::path::PathBuf::from);
        cx.spawn(async move |this, cx| {
            let result: Result<bool, String> = async {
                let json = team_ops::export_team(kit, community, team, memory, cx).await?;
                let bytes = team_file::encode(&json, format)?;
                let path = match dev {
                    Some(path) => Some(path),
                    None => {
                        let directory = dirs::download_dir()
                            .or_else(dirs::home_dir)
                            .unwrap_or_else(|| ".".into());
                        let chosen =
                            cx.update(|cx| cx.prompt_for_new_path(&directory, Some(&name)));
                        match chosen.await {
                            Ok(Ok(path)) => path,
                            _ => None,
                        }
                    }
                };
                let Some(path) = path else {
                    return Ok(false);
                };
                cx.background_executor()
                    .spawn(async move { std::fs::write(&path, bytes) })
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(true)
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(true) => cx.emit(Dismissed),
                    Ok(false) => {}
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for TeamExportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let close = cx.listener(|_, _, _, cx| cx.emit(Dismissed));
        let this = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                this.open = None;
                cx.notify();
            });
        });
        let mut memories = Options::new("memory-options");
        for (index, level) in [
            MemoryLevel::None,
            MemoryLevel::Core,
            MemoryLevel::Everything,
        ]
        .into_iter()
        .enumerate()
        {
            memories = memories.child(
                MenuItem::new(("memory", index), memory_label(level))
                    .checked(self.memory == level)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.memory = level;
                        this.confirmed = false;
                        this.open = None;
                        cx.notify();
                    })),
            );
        }
        let mut formats = Options::new("format-options");
        for (index, (format, label)) in [(Format::Json, "JSON"), (Format::Png, "PNG")]
            .into_iter()
            .enumerate()
        {
            formats = formats.child(
                MenuItem::new(("format", index), label)
                    .checked(self.format == format)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.format = format;
                        this.open = None;
                        cx.notify();
                    })),
            );
        }
        let entity = cx.entity().downgrade();
        let mut dialog = Dialog::new("team-export", format!("Export {}", self.team.name));
        if !self.busy {
            dialog = dialog.on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        }
        dialog
            .footer(
                Button::new("cancel-export", "Cancel")
                    .outline()
                    .disabled(self.busy)
                    .on_click(close),
            )
            .footer(
                Button::new("confirm-export", "Export")
                    .disabled(self.busy || (self.memory != MemoryLevel::None && !self.confirmed))
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .child(
                        SelectField {
                            id: "memory-select",
                            label: "Memories",
                            description: None,
                            value: memory_label(self.memory).into(),
                            open: self.open == Some(Picker::Memory),
                            disabled: self.busy,
                        }
                        .render(
                            cx.listener(|this, _, _, cx| {
                                this.open =
                                    (this.open != Some(Picker::Memory)).then_some(Picker::Memory);
                                cx.notify();
                            }),
                            memories,
                            dismiss.clone(),
                            window,
                            cx,
                        ),
                    )
                    .when(self.memory != MemoryLevel::None, |this| {
                        this.child(div().text_body_sm().text_color(c.text_subtle).child(
                            "Memory is stored as plaintext in the snapshot. Only share it \
                                 with people you trust.",
                        ))
                        .child(
                            Checkbox::new("confirm-memory", self.confirmed)
                                .label("I confirm that I want to include memory in this snapshot.")
                                .disabled(self.busy)
                                .surface(Surface::Floating)
                                .on_change(move |checked, _, cx| {
                                    let _ = entity.update(cx, |this, cx| {
                                        this.confirmed = checked;
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                    .child(
                        SelectField {
                            id: "format-select",
                            label: "File format",
                            description: None,
                            value: match self.format {
                                Format::Json => "JSON".into(),
                                Format::Png => "PNG".into(),
                            },
                            open: self.open == Some(Picker::Format),
                            disabled: self.busy,
                        }
                        .render(
                            cx.listener(|this, _, _, cx| {
                                this.open =
                                    (this.open != Some(Picker::Format)).then_some(Picker::Format);
                                cx.notify();
                            }),
                            formats,
                            dismiss,
                            window,
                            cx,
                        ),
                    )
                    .children(error_text(&self.error, cx)),
            )
    }
}

// ---------------------------------------------------------------------------
// Deploy

pub struct TeamDeployDialog {
    teams: Entity<TeamsState>,
    team: Team,
    channel: Option<String>,
    /// A deploy started for this channel; retries keep it.
    attempted: bool,
    open: bool,
    busy: bool,
    error: Option<SharedString>,
    _observe: Subscription,
}

impl EventEmitter<Dismissed> for TeamDeployDialog {}

impl TeamDeployDialog {
    pub fn new(teams: Entity<TeamsState>, team: Team, cx: &mut Context<Self>) -> Self {
        let community = teams.read(cx).community.clone();
        let mut dialog = Self {
            _observe: cx.observe(&community, |_, _, cx| cx.notify()),
            teams,
            team,
            channel: None,
            attempted: false,
            open: false,
            busy: false,
            error: None,
        };
        dialog.channel = dialog.channels(cx).first().map(|(id, _)| id.clone());
        dialog
    }

    /// Channels the viewer can add members to (`canAddMembers`).
    fn channels(&self, cx: &App) -> Vec<(String, String)> {
        let community = self.teams.read(cx).community.clone();
        community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .filter(|channel| can_add_members(channel, true))
            .map(|channel| (channel.id.clone(), channel.name.clone()))
            .collect()
    }

    fn deploy(&mut self, cx: &mut Context<Self>) {
        let Some(channel) = self.channel.clone().filter(|_| !self.busy) else {
            return;
        };
        self.attempted = true;
        self.busy = true;
        self.error = None;
        cx.notify();
        let teams = self.teams.read(cx);
        let (kit, community) = (teams.kit.clone(), teams.community.clone());
        let team = self.team.clone();
        cx.spawn(async move |this, cx| {
            let result = team_ops::deploy_team(kit, community, team, channel, cx).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(Dismissed),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for TeamDeployDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let channels = self.channels(cx);
        let count = self.team.agents.len();
        let selected = self
            .channel
            .as_ref()
            .and_then(|id| channels.iter().find(|(known, _)| known == id))
            .map(|(_, name)| SharedString::from(name.clone()));
        let mut options = Options::new("channel-options");
        for (index, (id, name)) in channels.iter().enumerate() {
            let id = id.clone();
            options = options.child(
                MenuItem::new(("deploy-channel", index), name.clone())
                    .checked(self.channel.as_ref() == Some(&id))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.channel = Some(id.clone());
                        this.open = false;
                        cx.notify();
                    })),
            );
        }
        let this = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                this.open = false;
                cx.notify();
            });
        });
        let label = if self.busy {
            "Deploying...".to_string()
        } else {
            format!(
                "Deploy {count} {}",
                if count == 1 { "agent" } else { "agents" }
            )
        };
        let mut dialog = Dialog::new("team-deploy", "Deploy team to channel").description(format!(
            "Attach the saved agents of {} to the selected channel.",
            self.team.name
        ));
        if !self.busy {
            dialog = dialog.on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        }
        dialog
            .footer(
                Button::new("cancel-deploy", "Cancel")
                    .outline()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
            )
            .footer(
                Button::new("confirm-deploy", label)
                    .disabled(self.busy || self.channel.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.deploy(cx))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .child(div().text_label_sm().child(format!("Agents ({count})")))
                    .child(
                        SelectField {
                            id: "deploy-channel-select",
                            label: "Channel",
                            description: None,
                            value: selected.unwrap_or_else(|| "No channels available".into()),
                            open: self.open,
                            disabled: self.busy || self.attempted || channels.is_empty(),
                        }
                        .render(
                            cx.listener(|this, _, _, cx| {
                                this.open = !this.open;
                                cx.notify();
                            }),
                            options,
                            dismiss,
                            window,
                            cx,
                        ),
                    )
                    .children(error_text(&self.error, cx)),
            )
    }
}

// ---------------------------------------------------------------------------
// Share

/// The complete catalog projection of a saved team (`buildTeamCatalogContent`):
/// a portable team shares its saved description and instructions, and
/// refuses to share without them rather than publish a lossy copy.
pub fn team_content(teams: Entity<TeamsState>, team: Team) -> Content {
    Rc::new(move |cx: &mut App| {
        let state = teams.read(cx);
        let (kit, community) = (state.kit.clone(), state.community.clone());
        let team = team.clone();
        cx.spawn(async move |cx| {
            let (description, instructions) = match &team.portable {
                Some(_) => {
                    let loaded = kit.load_team(&team).await?;
                    let host = cx
                        .update(|cx| team_ops::import_host(cx))
                        .ok_or("Team preview is unavailable")?;
                    let snapshot = team_ops::preview(&host, loaded.to_string(), cx).await?;
                    (snapshot.team.description, snapshot.team.instructions)
                }
                None => (None, None),
            };
            // Inheriting members share the defaults of the host they run on.
            let members = cx.update(|cx| {
                community_agents(&community, cx)
                    .iter()
                    .map(|(host, agent)| {
                        let mut member = persona(agent);
                        member.session_policy =
                            member.session_policy.or_else(|| default_policy(host, cx));
                        member
                    })
                    .collect::<Vec<_>>()
            });
            team_catalog_content(
                &TeamSource {
                    id: team.id.clone(),
                    name: team.name.clone(),
                    agents: team.agents.clone(),
                    description,
                    instructions,
                },
                &members,
                None,
            )
        })
    })
}

/// "Share {team}": the team's catalog switch, and its export.
pub struct TeamShareDialog {
    team: Team,
    switch: Entity<CatalogShareSwitch>,
    pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for TeamShareDialog {}
impl EventEmitter<ExportTeam> for TeamShareDialog {}

impl TeamShareDialog {
    pub fn new(
        teams: Entity<TeamsState>,
        catalog: Entity<CatalogState>,
        team: Team,
        cx: &mut Context<Self>,
    ) -> Self {
        let switch = Self::switch(teams, catalog, &team, cx);
        Self {
            _subscriptions: vec![cx.subscribe(&switch, |this, _, SharePending(pending), cx| {
                this.pending = *pending;
                cx.notify();
            })],
            switch,
            team,
            pending: false,
        }
    }

    /// The saved team's catalog switch, for any share dialog to embed.
    pub fn switch(
        teams: Entity<TeamsState>,
        catalog: Entity<CatalogState>,
        team: &Team,
        cx: &mut App,
    ) -> Entity<CatalogShareSwitch> {
        let content = team_content(teams, team.clone());
        let (id, name) = (team.id.clone(), team.name.clone());
        cx.new(|cx| {
            CatalogShareSwitch::new(
                catalog,
                CatalogKind::Team,
                id,
                name,
                TEAM_SHARE_DESCRIPTION,
                content,
                cx,
            )
        })
    }

    pub fn team(&self) -> &Team {
        &self.team
    }
}

impl Render for TeamShareDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut dialog = Dialog::new("team-share", format!("Share {}", self.team.name));
        if !self.pending {
            dialog = dialog.on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)));
        }
        dialog.child(self.switch.clone()).footer(
            Button::new("export-team", "Export team")
                .disabled(self.pending)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ExportTeam))),
        )
    }
}
