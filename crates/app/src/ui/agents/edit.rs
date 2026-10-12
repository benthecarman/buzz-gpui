//! The Edit agent dialog (ui.md §5a.8): buzz-app's `AgentEditor`.
//!
//! Also opened with a requested draft when an agent asks for changes to
//! itself (`AgentUpdateReview`).

use super::create::OpenHarnesses;
use super::draft::{self, Draft};
use super::editor::{Dismissed, Saved, saved_message};
use super::fields::{Fields, Owner};
use super::form::accordion_header;
use super::inventory;
use crate::community::AppState;
use crate::hosts::HostConnection;
use crate::hosts::control::{Lane, Status};
use crate::ui::keys::npub;
use buzz_host::api::{Agent, AgentAction, ProcessStatus, Request, Response};
use buzz_relay_client::upload::{UploadFile, sniff_image};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, Dialog, DialogSize, Surface, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;

pub struct EditDialog {
    host: Entity<HostConnection>,
    /// The agent as it was when the dialog opened (or last saved).
    agent: Agent,
    display_name: String,
    fields: Fields,
    notice: Option<SharedString>,
    uploading: bool,
    /// A save is in flight; closing leaves it to finish on the host.
    saving: bool,
    runtime_open: bool,
    technical_open: bool,
    _subscription: Subscription,
}

impl EventEmitter<Dismissed> for EditDialog {}
impl EventEmitter<Saved> for EditDialog {}
impl EventEmitter<OpenHarnesses> for EditDialog {}

impl Owner for EditDialog {
    fn fields(&mut self) -> &mut Fields {
        &mut self.fields
    }

    fn open_harnesses(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(OpenHarnesses);
    }
}

/// Asks for one picture file; `None` once the dialog is cancelled.
pub(crate) fn choose_picture(cx: &mut App) -> impl Future<Output = Option<PathBuf>> + use<> {
    // Development only: a headless session has no file dialog.
    let dev_file = std::env::var_os("BUZZ_DEV_PICK_FILE").map(PathBuf::from);
    let paths = dev_file.is_none().then(|| {
        cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a picture".into()),
        })
    });
    async move {
        match (dev_file, paths) {
            (Some(path), _) => Some(path),
            (None, Some(paths)) => match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                _ => None,
            },
            (None, None) => None,
        }
    }
}

/// Reads the picture at `path` off the UI thread and uploads it to
/// `media`, for the URL an agent's profile can show.
pub(crate) async fn upload_agent_picture(
    path: PathBuf,
    media: &buzz_relay_client::Media,
    executor: &BackgroundExecutor,
) -> Result<String, String> {
    let file = executor.spawn(async move { read_picture(&path) }).await?;
    let blob = buzz_relay_client::upload::upload(media, file)
        .await
        .map_err(|error| error.to_string())?;
    if !blob.mime.starts_with("image/") {
        Err("The server did not return an avatar image.".into())
    } else if buzz_relay_client::account::picture_error(&blob.url).is_some() {
        Err("The server returned a picture link that can’t be used.".into())
    } else {
        Ok(blob.url)
    }
}

/// Reads a chosen picture, as Settings → Profile does.
fn read_picture(path: &std::path::Path) -> Result<UploadFile, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("Couldn’t read that file: {error}"))?
        .len();
    if size == 0 || size > 50 * 1024 * 1024 {
        return Err("Choose an image smaller than 50 MiB.".into());
    }
    let bytes = std::fs::read(path).map_err(|error| format!("Couldn’t read that file: {error}"))?;
    let mime = sniff_image(&bytes)
        .filter(|mime| {
            matches!(
                *mime,
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            )
        })
        .ok_or("Choose a PNG, JPEG, GIF or WebP image.")?;
    Ok(UploadFile {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "avatar".into()),
        mime: mime.into(),
        bytes,
    })
}

impl EditDialog {
    pub fn new(
        host: Entity<HostConnection>,
        agent: Agent,
        display_name: String,
        initial: Option<Draft>,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let requested = initial.is_some();
        let draft = initial.unwrap_or_else(|| draft::from_agent(&agent));
        let mut fields = Fields::new(
            host.clone(),
            Some((agent.id.clone(), agent.revision)),
            agent.harness.environment_keys.clone(),
            draft,
            window,
            cx,
        );
        fields.dirty = requested;
        // Focus inside the dialog so Escape reaches it.
        fields.focus_name(window, cx);
        let subscription = cx.observe(&host, |_, _, cx| cx.notify());
        Self {
            host,
            agent,
            display_name,
            fields,
            notice: notice.map(Into::into),
            uploading: false,
            saving: false,
            runtime_open: false,
            technical_open: false,
            _subscription: subscription,
        }
    }

    fn current(&self, cx: &App) -> Agent {
        self.host
            .read(cx)
            .agent(&self.agent.id)
            .cloned()
            .unwrap_or_else(|| self.agent.clone())
    }

    fn can_close(&self, cx: &App) -> bool {
        let control = &self.host.read(cx).control;
        self.saving
            || !control.busy
            || control.pending_launch.is_some()
            || control.pending_credential_write
    }

    fn blocked(&self, cx: &App) -> bool {
        let host = self.host.read(cx);
        self.saving || host.control.busy || self.uploading || host.status() != Status::Ready
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.can_close(cx) {
            cx.emit(Dismissed);
        }
    }

    fn act(&mut self, action: AgentAction, cx: &mut Context<Self>) {
        self.notice = None;
        let owner = match &cx.global::<AppState>().identity {
            crate::community::IdentityState::Ready(identity) => Some(identity.pubkey_hex()),
            _ => None,
        };
        let id = self.agent.id.clone();
        self.host
            .update(cx, |host, cx| host.act(&id, action, owner, None, cx))
            .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let agent = self.current(cx);
        let stale = self.fields.draft.revision != agent.revision;
        if self.blocked(cx) || !self.fields.dirty || stale {
            return;
        }
        if let Some(picture) = &self.fields.draft.picture
            && let Some(error) = buzz_relay_client::account::picture_error(picture)
        {
            self.fields.error = Some(error.to_string().into());
            return cx.notify();
        }
        let edit = match draft::edit(&self.fields.draft, false) {
            Ok(edit) => edit,
            Err(error) => {
                self.fields.error = Some(error.into());
                return cx.notify();
            }
        };
        let id = agent.id.clone();
        let revision = self.fields.draft.revision;
        self.saving = true;
        cx.notify();
        let save = self.host.update(cx, |host, cx| {
            host.write(
                Request::Save {
                    id: id.clone(),
                    revision,
                    edit,
                },
                Lane {
                    credential_write: true,
                    ..Default::default()
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                cx.notify();
            });
            let snapshot = match result {
                Ok(Response::Agents(snapshot)) => snapshot,
                Ok(_) => return,
                Err(failure) => {
                    let _ = this.update(cx, |this, cx| {
                        this.fields.error = Some(failure.message.into());
                        cx.notify();
                    });
                    return;
                }
            };
            let pending = snapshot
                .agents
                .iter()
                .find(|agent| agent.id == id)
                .is_some_and(|agent| agent.profile_pending);
            let publish = this.update(cx, |this, cx| {
                if let Some(saved) = snapshot.agents.iter().find(|agent| agent.id == id) {
                    this.agent = saved.clone();
                    this.fields.saved = Some((saved.id.clone(), saved.revision));
                    this.fields.environment_keys = saved.harness.environment_keys.clone();
                }
                this.fields.dirty = false;
                this.fields.draft = Draft {
                    environment: Default::default(),
                    picture: None,
                    revision: this.agent.revision,
                    ..this.fields.draft.clone()
                };
                this.fields.error = None;
                cx.notify();
                pending.then(|| {
                    this.notice = Some("Settings saved. Publishing profile update…".into());
                    this.host.update(cx, |host, cx| {
                        host.write(
                            Request::PublishProfile { id: id.clone() },
                            Lane {
                                credential_write: true,
                                ..Default::default()
                            },
                            cx,
                        )
                    })
                })
            });
            if let Ok(Some(publish)) = publish
                && publish.await.is_err()
            {
                let _ = this.update(cx, |this, cx| {
                    this.notice = Some(
                        "Settings saved; profile publication is unconfirmed. Refresh status, \
                         then retry publication below or on the agent card."
                            .into(),
                    );
                    cx.notify();
                });
                return;
            }
            let message = saved_message(
                snapshot.restarted.unwrap_or(0),
                snapshot.restart_failures.unwrap_or(0),
            );
            let _ = this.update(cx, |this, cx| {
                if snapshot.restart_failures.unwrap_or(0) > 0 {
                    this.notice = Some(message.into());
                    cx.notify();
                } else {
                    cx.emit(Saved(message.into()));
                    cx.emit(Dismissed);
                }
            });
        })
        .detach();
    }

    fn retry_profile(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        let id = self.agent.id.clone();
        let publish = self.host.update(cx, |host, cx| {
            host.write(
                Request::PublishProfile { id },
                Lane {
                    credential_write: true,
                    ..Default::default()
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            if publish.await.is_ok() {
                let _ = this.update(cx, |this, cx| {
                    this.notice = Some("Profile published. Running work was not restarted.".into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Picks an image and uploads it to the agent's community.
    fn upload_picture(&mut self, window: &Window, cx: &mut Context<Self>) {
        let relay = inventory::origin(&self.agent.relay_url);
        let community = cx
            .global::<AppState>()
            .communities
            .iter()
            .find(|community| community.read(cx).origin.http() == relay)
            .cloned()
            .or_else(|| AppState::current(cx));
        let Some(community) = community else {
            self.fields.error =
                Some("Connect to the agent’s community to change its picture.".into());
            return cx.notify();
        };
        let session = community.read(cx).session.clone();
        let path = choose_picture(cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = path.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.uploading = true;
                cx.notify();
            });
            let result =
                upload_agent_picture(path, session.media(), cx.background_executor()).await;
            let _ = this.update(cx, |this, cx| {
                this.uploading = false;
                match result {
                    Ok(url) => {
                        this.fields.draft.picture = Some(url);
                        this.fields.dirty = true;
                        this.fields.error = None;
                        this.notice = None;
                    }
                    Err(error) => this.fields.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn accordion(
        &self,
        id: &'static str,
        title: &'static str,
        open: bool,
        cx: &mut Context<Self>,
        toggle: fn(&mut Self),
    ) -> Stateful<Div> {
        let c = *cx.colors();
        accordion_header(id, title, open, &c).on_click(cx.listener(move |this, _, _, cx| {
            toggle(this);
            cx.notify();
        }))
    }
}

impl Render for EditDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Escape closes an open select or the model list before the dialog.
        div()
            .capture_action(cx.listener(|this, _: &buzz_ui::Cancel, _, cx| {
                if this.fields.open.take().is_some() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(self.dialog(window, cx))
    }
}

impl EditDialog {
    fn dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Dialog {
        let c = *cx.colors();
        self.fields.sync(window, cx);
        let agent = self.current(cx);
        let host = self.host.read(cx);
        let status = host.status();
        let error_state = status == Status::Error;
        let state_error = host.control.error.clone();
        let busy = host.control.busy;
        let runtime_available = host.agents.runtime_available;
        let avatar_editing = host.agents.avatar_editing_available;
        let launch_blocked = host.launch_block(&agent).is_some() || self.fields.dirty;
        let can_stop = host.can_stop(&agent.id);
        let blocked = self.blocked(cx);
        let can_close = self.can_close(cx);
        let dirty = self.fields.dirty;
        let stale = self.fields.draft.revision != agent.revision;
        let unapplied = agent
            .running_revision
            .is_some_and(|running| running != agent.revision);
        let picture = self
            .fields
            .draft
            .picture
            .clone()
            .or_else(|| agent.picture.clone())
            .filter(|picture| !picture.is_empty());
        let mut avatar =
            Avatar::new(self.display_name.clone())
                .squircle()
                .size(if avatar_editing {
                    AvatarSize::Px(80.)
                } else {
                    AvatarSize::Large
                });
        if let Some(picture) = picture.clone() {
            avatar = avatar.image(crate::ui::media::any_avatar(&picture));
        }
        let identity = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(rems(0.75))
            .text_center()
            .child(avatar)
            .when(avatar_editing, |this| {
                this.child(
                    div()
                        .flex()
                        .gap(rems(0.5))
                        .child(
                            Button::new(
                                "change-picture",
                                if picture.is_some() {
                                    "Change picture"
                                } else {
                                    "Upload picture"
                                },
                            )
                            .size(ButtonSize::Sm)
                            .surface(Surface::Floating)
                            .loading(self.uploading)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.upload_picture(window, cx)),
                            ),
                        )
                        .when(picture.is_some(), |this| {
                            this.child(
                                Button::new("remove-picture", "Remove")
                                    .size(ButtonSize::Sm)
                                    .surface(Surface::Floating)
                                    .disabled(busy || self.uploading)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.fields.draft.picture = Some(String::new());
                                        this.fields.dirty = true;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
            })
            .child(div().text_label().child(self.display_name.clone()))
            .child(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(agent.relay_url.clone()),
            );
        let quiet = |text: String| div().text_body_sm().text_color(c.text_subtle).child(text);
        let mode = if !agent.configured {
            "Imported · close the editor and choose Use here before starting"
        } else if agent.enabled {
            if !runtime_available {
                "Enabled intent saved · execution unavailable"
            } else if agent.start_on_app_launch {
                "Enabled · starts with buzz-app"
            } else {
                "Enabled · manual-start only"
            }
        } else if agent.start_on_app_launch {
            "Start on launch enabled"
        } else {
            "Manual start · a later sent mention can start this agent"
        };
        let mut runtime = div()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .child(
                self.accordion("runtime-toggle", "Runtime", self.runtime_open, cx, |this| {
                    this.runtime_open = !this.runtime_open
                }),
            );
        if self.runtime_open {
            let stoppable = matches!(agent.status, ProcessStatus::Stopped | ProcessStatus::Failed);
            runtime = runtime
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(0.25))
                        .child(div().text_body_sm().child(format!(
                            "{}{}",
                            if error_state { "Last known: " } else { "" },
                            super::process_label(agent.status)
                        )))
                        .child(quiet(mode.into())),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(rems(0.5))
                        .when(stoppable, |this| {
                            this.child(
                                Button::new(
                                    "runtime-start",
                                    if agent.status == ProcessStatus::Failed {
                                        "Retry start"
                                    } else {
                                        "Start"
                                    },
                                )
                                .surface(Surface::Floating)
                                .disabled(launch_blocked)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.act(AgentAction::Start, cx)),
                                ),
                            )
                        })
                        .child(
                            Button::new("runtime-stop", "Stop")
                                .surface(Surface::Floating)
                                .disabled(!can_stop)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.act(AgentAction::Stop, cx)),
                                ),
                        )
                        .child(
                            Button::new(
                                "runtime-restart",
                                if unapplied {
                                    "Restart to apply"
                                } else {
                                    "Restart"
                                },
                            )
                            .surface(Surface::Floating)
                            .disabled(launch_blocked)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.act(AgentAction::Restart, cx)),
                            ),
                        ),
                )
                .child(quiet(format!(
                    "Saved revision {} · Running revision {}.{} Stop ends current work. After \
                     setup, a later sent mention can start it again.",
                    agent.revision,
                    agent
                        .running_revision
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "none".into()),
                    if unapplied {
                        " Saved changes are not running yet."
                    } else {
                        ""
                    }
                )));
        }
        let mut technical = div()
            .flex()
            .flex_col()
            .gap(rems(0.75))
            .child(self.accordion(
                "technical-toggle",
                "Technical details",
                self.technical_open,
                cx,
                |this| this.technical_open = !this.technical_open,
            ));
        if self.technical_open {
            let detail = |label: &'static str, value: String| {
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.25))
                    .child(div().text_body_sm().text_color(c.text_subtle).child(label))
                    .child(div().text_mono().child(value))
            };
            technical = technical
                .child(detail("Public key (npub)", npub(&agent.pubkey)))
                .child(detail("Relay", agent.relay_url.clone()))
                .when(!agent.diagnostics.is_empty(), |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(rems(0.5))
                            .child(div().text_label().child("Host diagnostics"))
                            .child(div().text_mono().child(agent.diagnostics.join("\n"))),
                    )
                });
        }
        let danger = |text: SharedString| div().text_color(c.text_danger).child(text);
        let mut body = div().flex().flex_col().gap(rems(2.));
        body = body
            .child(identity)
            .child(self.fields.render(busy, dirty, window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(runtime)
                    .child(technical),
            );
        let mut tail = div().flex().flex_col().gap(rems(0.75));
        if let Some(error) = agent.error.clone() {
            tail = tail.child(danger(error.into()));
        }
        if let Some(error) = state_error
            .map(SharedString::from)
            .or(self.fields.error.clone())
        {
            tail = tail.child(danger(error));
        }
        if agent.profile_pending {
            tail = tail.child(
                div().flex().child(
                    Button::new("retry-profile-publication", "Retry profile publication")
                        .surface(Surface::Floating)
                        .disabled(busy || status != Status::Ready)
                        .on_click(cx.listener(|this, _, _, cx| this.retry_profile(cx))),
                ),
            );
        }
        if error_state {
            let host = self.host.clone();
            tail = tail.child(
                div().flex().child(
                    Button::new("retry-status", "Retry status")
                        .surface(Surface::Floating)
                        .on_click(move |_, _, cx| {
                            host.update(cx, |host, cx| host.refresh(cx).detach())
                        }),
                ),
            );
        }
        if stale {
            tail = tail.child(danger(
                "The host has a newer saved revision. Your edits are still here; copy anything \
                 you need, then discard to load the latest settings."
                    .into(),
            ));
        }
        if self.saving {
            tail = tail.child(div().text_color(c.text_subtle).child("Saving changes…"));
        }
        if let Some(notice) = self.notice.clone() {
            tail = tail.child(div().text_color(c.text_subtle).child(notice));
        }
        body = body.child(tail);
        Dialog::new("edit-agent", "Edit agent")
            .size(DialogSize::Px(640.))
            .heading_title()
            // Escape and the close button keep unsaved edits open.
            .on_close(cx.listener(|this, _, _, cx| {
                if !this.fields.dirty && !this.uploading {
                    this.close(cx);
                }
            }))
            .child(body)
            .footer(
                Button::new("cancel-edit", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(!can_close)
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .when(stale, |this| {
                this.footer(
                    Button::new("discard-edit", "Discard changes")
                        .surface(Surface::Floating)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let agent = this.current(cx);
                            this.agent = agent.clone();
                            this.fields.saved = Some((agent.id.clone(), agent.revision));
                            this.fields.environment_keys = agent.harness.environment_keys.clone();
                            this.fields.reset(draft::from_agent(&agent), window, cx);
                            this.notice = None;
                            cx.notify();
                        })),
                )
            })
            .footer(
                Button::new("save-edit", "Save changes")
                    .prominent()
                    .disabled(blocked || !dirty || stale)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }
}
