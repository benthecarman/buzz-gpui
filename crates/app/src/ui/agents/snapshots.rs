//! Agent snapshot export and import (buzz-app `AgentSnapshots.tsx`), and
//! the layer every snapshot dialog opens in.
//!
//! Snapshot dialogs open from the Agents page, the Add agent dialog, chat
//! attachments and links, so they draw in a layer of their own over the
//! workspace rather than in one page's modal slot.

use super::editor::{connected, create_agent};
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::runtime::Runtime;
use crate::snapshot::manifest::{
    self, AgentSnapshot, MAX_PNG_BYTES, MemoryEntry, MemoryLevel, TOO_LARGE, build_agent_snapshot,
    legacy_agent_file_error, parse_agent_snapshot, snapshot_import_edit, snapshot_limitations,
};
use crate::snapshot::memory;
use crate::snapshot::png::{self, Format};
use crate::ui::overlay::{self, Align, Dismiss};
use base64::Engine;
use buzz_host::api::{Agent, MemoryRestore, Request, Response};
use buzz_relay_client::RelayOrigin;
use buzz_ui::{
    ActiveTheme, Button, Checkbox, Colors, Dialog, Icon, IconName, Menu, MenuItem, MenuSize,
    Select, Surface, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// The layer

struct Open {
    view: AnyView,
    /// False while the dialog must stay open (`preventClose`).
    closable: Rc<dyn Fn(&App) -> bool>,
}

#[derive(Default)]
struct Layer {
    open: Option<Open>,
}

struct LayerGlobal(Entity<Layer>);

impl Global for LayerGlobal {}

fn layer_entity(cx: &mut App) -> Entity<Layer> {
    if let Some(layer) = cx.try_global::<LayerGlobal>() {
        return layer.0.clone();
    }
    let layer = cx.new(|_| Layer::default());
    cx.set_global(LayerGlobal(layer.clone()));
    layer
}

/// A dialog the layer can hold.
pub(crate) trait LayerDialog: Render {
    /// Whether a press outside or Escape may close it now.
    fn closable(&self) -> bool;
}

/// Opens `view` over the window, replacing any snapshot dialog.
pub(crate) fn show<V: LayerDialog>(view: Entity<V>, cx: &mut App) {
    let weak = view.downgrade();
    let closable = Rc::new(move |cx: &App| weak.upgrade().is_none_or(|v| v.read(cx).closable()));
    layer_entity(cx).update(cx, |layer, cx| {
        layer.open = Some(Open {
            view: view.into(),
            closable,
        });
        cx.notify();
    });
}

/// Closes the open snapshot dialog.
pub(crate) fn close(cx: &mut App) {
    layer_entity(cx).update(cx, |layer, cx| {
        layer.open = None;
        cx.notify();
    });
}

/// The open snapshot dialog over its scrim, for the workspace to draw.
pub(crate) fn layer(window: &Window, cx: &mut App) -> Option<AnyElement> {
    dev_open(window, cx);
    let layer = layer_entity(cx);
    let open = layer.read(cx).open.as_ref()?;
    let (view, closable) = (open.view.clone(), open.closable.clone());
    let dismiss: Dismiss = Rc::new(move |_, cx| {
        if closable(cx) {
            close(cx);
        }
    });
    Some(overlay::modal(window, view, Some(dismiss)).into_any_element())
}

/// Development only: `BUZZ_DEV_SNAPSHOT` opens a snapshot dialog once the
/// first host lists an agent, for headless screenshots: `export`, `share`,
/// `import`, `import:<file>` (as if chosen) or `preview:<media URL>`.
fn dev_open(window: &Window, cx: &mut App) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Ok(what) = std::env::var("BUZZ_DEV_SNAPSHOT") else {
        return;
    };
    if STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(500))
                .await;
            let opened = handle.update(cx, |_, window, cx| {
                let hosts = cx
                    .try_global::<crate::hosts::Hosts>()
                    .map(|hosts| hosts.hosts.clone())
                    .unwrap_or_default();
                let agent = hosts.iter().find_map(|host| {
                    host.read(cx)
                        .agents
                        .agents
                        .first()
                        .map(|agent| (host.clone(), agent.id.clone()))
                });
                let needs_agent = matches!(what.as_str(), "export" | "share");
                if needs_agent && agent.is_none() || hosts.is_empty() {
                    return false;
                }
                match (what.split_once(':'), agent) {
                    (Some(("import", path)), _) => {
                        let bytes = std::fs::read(path).unwrap_or_default();
                        let dialog = cx.new(|cx| {
                            let mut dialog = ImportDialog::new(hosts, None, cx);
                            dialog.load(bytes, cx);
                            dialog
                        });
                        show(dialog, cx);
                    }
                    (Some(("preview", url)), _) => {
                        if let Some(community) = AppState::current(cx) {
                            let media = community.read(cx).session.media().clone();
                            super::snapshot_preview::open_preview(
                                super::snapshot_preview::PreviewRequest {
                                    url: url.to_owned(),
                                    name: None,
                                    kind: None,
                                },
                                media,
                                window,
                                cx,
                            );
                        }
                    }
                    (_, Some((host, id))) if what == "export" => open_export(host, id, cx),
                    (_, Some((host, id))) if what == "share" => {
                        super::share::open_agent(host, id, window, cx)
                    }
                    _ => open_import(hosts, None, window, cx),
                }
                true
            });
            if opened.unwrap_or(true) {
                break;
            }
        }
    })
    .detach();
}

// ---------------------------------------------------------------------------
// Shared pieces

/// A menu pick or button action that needs the window.
pub(super) type WindowAction = Rc<dyn Fn(&mut Window, &mut App)>;

/// A `<p>` at body size.
pub(super) fn paragraph(text: impl Into<SharedString>) -> Div {
    div().text_body().child(text.into())
}

/// A `role="alert"` paragraph.
pub(super) fn alert(text: impl Into<SharedString>, c: &Colors) -> Div {
    div()
        .text_body()
        .text_color(c.text_danger)
        .child(text.into())
}

/// A `<pre>` that wraps and scrolls past `max_height` rems.
pub(super) fn pre(
    id: impl Into<ElementId>,
    text: String,
    max_height: f32,
    c: &Colors,
) -> Stateful<Div> {
    div()
        .id(id)
        .max_h(rems(max_height))
        .overflow_y_scroll()
        .p(rems(0.5))
        .rounded(buzz_ui::tokens::radius::CONTROL)
        .bg(c.surface_inset)
        .text_mono()
        .child(text)
}

/// A `<details>` summary row.
pub(super) fn summary(
    id: impl Into<ElementId>,
    label: &'static str,
    open: bool,
    c: &Colors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(rems(0.25))
        .cursor_pointer()
        .text_body()
        .child(
            Icon::new(if open {
                IconName::CaretDown
            } else {
                IconName::CaretRight
            })
            .size(14.)
            .color(c.text_subtle),
        )
        .child(label)
}

/// "Memory is stored as **plaintext** in the snapshot. …"
pub(super) fn plaintext_warning(c: &Colors) -> AnyElement {
    let text =
        "Memory is stored as plaintext in the snapshot. Only share it with people you trust.";
    let start = "Memory is stored as ".len();
    div()
        .text_body()
        .text_color(c.text_danger)
        .child(StyledText::new(text).with_highlights([(
            start..start + "plaintext".len(),
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
        )]))
        .into_any_element()
}

/// A labelled inline select (`<label>Memories <select>…</label>`) whose
/// menu hangs below it while `open`.
#[allow(clippy::too_many_arguments)]
pub(super) fn inline_select(
    id: &'static str,
    label: &'static str,
    value: SharedString,
    open: bool,
    disabled: bool,
    options: Vec<(SharedString, bool, WindowAction)>,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    dismiss: Dismiss,
    window: &Window,
) -> Div {
    let menu = options.into_iter().enumerate().fold(
        Menu::new().size(MenuSize::Wide),
        |menu, (index, (text, checked, pick))| {
            menu.child(
                MenuItem::new((id, index), text)
                    .checked(checked)
                    .on_click(move |_, window, cx| pick(window, cx)),
            )
        },
    );
    div()
        .flex()
        .items_center()
        .gap(rems(0.5))
        .when(!label.is_empty(), |this| {
            this.child(div().text_body().child(label))
        })
        .child(
            div()
                .relative()
                .w(rems(14.))
                .child(
                    Select::new(id)
                        .value(value)
                        .open(open)
                        .disabled(disabled)
                        .surface(Surface::Floating)
                        .on_click(toggle),
                )
                .when(open && !disabled, |this| {
                    this.child(overlay::dropdown(window, Align::Start, menu, dismiss))
                }),
        )
}

/// A checkbox handler that updates the view.
pub(super) fn checked<T: 'static>(
    cx: &Context<T>,
    f: impl Fn(&mut T, bool, &mut Context<T>) + 'static,
) -> impl Fn(bool, &mut Window, &mut App) + 'static {
    let weak = cx.entity().downgrade();
    move |value, _, cx| {
        let _ = weak.update(cx, |this, cx| f(this, value, cx));
    }
}

pub(super) fn level_label(kind: crate::snapshot::Kind, level: MemoryLevel) -> SharedString {
    let item = kind.title();
    match level {
        MemoryLevel::None => format!("{item} only"),
        MemoryLevel::Core => format!("{item} + core memory"),
        MemoryLevel::Everything => format!("{item} + all memories"),
    }
    .into()
}

pub(super) const LEVELS: [MemoryLevel; 3] = [
    MemoryLevel::None,
    MemoryLevel::Core,
    MemoryLevel::Everything,
];

fn find_agent(host: &Entity<HostConnection>, id: &str, cx: &App) -> Option<Agent> {
    host.read(cx)
        .agents
        .agents
        .iter()
        .find(|agent| agent.id == id)
        .cloned()
}

fn origin_of(url: &str) -> Option<String> {
    RelayOrigin::parse(url)
        .ok()
        .map(|origin| origin.websocket().to_string())
}

/// The selected community: `(websocket URL, origin)`.
fn destination(cx: &App) -> Option<(String, RelayOrigin)> {
    let community = AppState::current(cx)?;
    let origin = community.read(cx).origin.clone();
    Some((origin.websocket().to_string(), origin))
}

fn identity(cx: &App) -> Option<Arc<buzz_identity::Identity>> {
    match &cx.global::<AppState>().identity {
        IdentityState::Ready(identity) => Some(identity.clone()),
        _ => None,
    }
}

/// Runs a tokio future from GPUI.
pub(super) fn on_tokio<T, F>(
    cx: &App,
    future: F,
) -> impl std::future::Future<Output = Result<T, String>> + use<T, F>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, String>> + Send + 'static,
{
    let handle = Runtime::handle(cx).spawn(future);
    async move {
        handle
            .await
            .unwrap_or_else(|_| Err("The request was cancelled.".into()))
    }
}

/// The portable configuration the review shows, or why there is none.
pub(super) fn review(
    agent: &Agent,
    host: &Entity<HostConnection>,
    cx: &App,
) -> Result<AgentSnapshot, String> {
    let defaults = host
        .read(cx)
        .defaults
        .as_ref()
        .and_then(|defaults| defaults.session_policy);
    build_agent_snapshot(agent, MemoryLevel::None, &[], defaults)
}

/// Reads an agent's memories as `level` includes them, in the agent's
/// community.
pub(super) fn read_memories(
    agent: &Agent,
    level: MemoryLevel,
    cx: &App,
) -> impl std::future::Future<Output = Result<Vec<MemoryEntry>, String>> + use<> {
    let origin = RelayOrigin::parse(&agent.relay_url);
    let identity = identity(cx);
    let pubkey = agent.pubkey.clone();
    on_tokio(cx, async move {
        if level == MemoryLevel::None {
            return Ok(Vec::new());
        }
        let origin = origin.map_err(|_| "Memory reads require the source agent's community.")?;
        let identity = identity.ok_or("Set up your identity first.")?;
        let listing = memory::read_complete(origin, identity, &pubkey).await?;
        Ok(listing
            .entries
            .into_iter()
            .filter(|entry| level.includes(&entry.slug))
            .map(|entry| MemoryEntry {
                slug: entry.slug,
                body: entry.body,
            })
            .collect())
    })
}

// ---------------------------------------------------------------------------
// Export

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportPicker {
    Memories,
    Format,
}

/// "Export {name}" (`AgentSnapshotExport`).
pub struct ExportDialog {
    host: Entity<HostConnection>,
    id: String,
    name: String,
    level: MemoryLevel,
    format: Format,
    open: Option<ExportPicker>,
    details: bool,
    /// The review key the reviewer approved, and the level they confirmed
    /// memory for under it.
    reviewed: Option<String>,
    confirmed: Option<(String, MemoryLevel)>,
    /// The review key last rendered; a change resets the approvals.
    current: Option<String>,
    changed_during_export: bool,
    pending: bool,
    error: Option<SharedString>,
    _observe: Subscription,
}

impl LayerDialog for ExportDialog {
    fn closable(&self) -> bool {
        !self.pending
    }
}

pub(crate) fn open_export(host: Entity<HostConnection>, id: String, cx: &mut App) {
    let dialog = cx.new(|cx| ExportDialog::new(host, id, cx));
    show(dialog, cx);
}

impl ExportDialog {
    fn new(host: Entity<HostConnection>, id: String, cx: &mut Context<Self>) -> Self {
        host.update(cx, |host, cx| host.load_defaults(cx));
        let name = find_agent(&host, &id, cx)
            .map(|agent| agent.name)
            .unwrap_or_default();
        Self {
            _observe: cx.observe(&host, |_, _, cx| cx.notify()),
            host,
            id,
            name,
            level: MemoryLevel::None,
            format: Format::Png,
            open: None,
            details: false,
            reviewed: None,
            confirmed: None,
            current: None,
            changed_during_export: false,
            pending: false,
            error: None,
        }
    }

    /// Approval covers the artwork source too: a PNG may embed an HTTPS
    /// picture the portable JSON leaves out.
    fn review_key(agent: &Agent, review: &AgentSnapshot, destination: &str) -> String {
        serde_json::to_string(&(
            &agent.id,
            &agent.pubkey,
            &agent.relay_url,
            destination,
            &agent.picture,
            review,
        ))
        .unwrap_or_default()
    }

    fn memory_allowed(agent: &Agent, cx: &App) -> bool {
        destination(cx).is_some_and(|(url, _)| origin_of(&agent.relay_url).as_ref() == Some(&url))
    }

    fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent) = find_agent(&self.host, &self.id, cx) else {
            return;
        };
        let destination_url = destination(cx).map(|(url, _)| url).unwrap_or_default();
        let Ok(review) = review(&agent, &self.host, cx) else {
            return;
        };
        let approved = Self::review_key(&agent, &review, &destination_url);
        let reviewed = self.reviewed.as_ref() == Some(&approved);
        let confirmed = self.confirmed.as_ref() == Some(&(approved.clone(), self.level));
        if self.pending || !reviewed || (self.level != MemoryLevel::None && !confirmed) {
            return;
        }
        let (level, format) = (self.level, self.format);
        if level != MemoryLevel::None && !Self::memory_allowed(&agent, cx) {
            self.error = Some("Memory reads require the source agent's community.".into());
            cx.notify();
            return;
        }
        self.pending = true;
        self.changed_during_export = false;
        self.error = None;
        cx.notify();
        let memories = read_memories(&agent, level, cx);
        let media = AppState::current(cx).map(|c| c.read(cx).session.media().clone());
        let picture = agent.picture.clone().filter(|p| !p.is_empty());
        let file_name = manifest::file_name(&agent.name, format.extension());
        let dev_file = std::env::var_os("BUZZ_DEV_SAVE_FILE").map(PathBuf::from);
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<(), String> = async {
                let entries = memories.await?;
                let snapshot = AgentSnapshot {
                    memory: manifest::SnapshotMemory { level, entries },
                    ..review.clone()
                };
                // Memory joins only after consent; validate the whole file
                // as well as the reviewed configuration.
                let json = serde_json::to_vec(&snapshot).map_err(|_| manifest::INVALID_MANIFEST)?;
                parse_agent_snapshot(&json, false)?;
                // Missing or unreadable artwork uses the placeholder.
                let artwork = match (format, picture, media) {
                    (Format::Png, Some(picture), Some(media)) => {
                        let bytes = media.fetch(&picture).await.ok();
                        cx.background_executor()
                            .spawn(async move { bytes.and_then(|b| png::render_artwork(&b)) })
                            .await
                    }
                    _ => None,
                };
                // A change while memory or artwork loaded must not ship an
                // unreviewed source under the earlier approval.
                let unchanged = this
                    .update(cx, |this, _| {
                        !this.changed_during_export && this.current.as_ref() == Some(&approved)
                    })
                    .unwrap_or(false);
                if !unchanged {
                    return Err(
                        "Agent configuration changed. Review it again before exporting.".into(),
                    );
                }
                let bytes = png::encode(&snapshot, format, artwork.as_deref())?;
                let path = match dev_file {
                    Some(path) => Some(path),
                    None => {
                        let directory = dirs::download_dir()
                            .or_else(dirs::home_dir)
                            .unwrap_or_else(|| PathBuf::from("."));
                        let prompt = cx
                            .update(|_, cx| cx.prompt_for_new_path(&directory, Some(&file_name)))
                            .map_err(|_| "Export failed.")?;
                        prompt
                            .await
                            .map_err(|_| "Export failed.")?
                            .map_err(|error| error.to_string())?
                    }
                };
                // Cancelling the save dialog keeps the dialog open.
                let Some(path) = path else {
                    return Ok(());
                };
                cx.background_executor()
                    .spawn(async move { std::fs::write(&path, bytes) })
                    .await
                    .map_err(|error| format!("Couldn’t save the snapshot: {error}"))?;
                let _ = cx.update(|_, cx| close(cx));
                Ok(())
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                if let Err(error) = result {
                    this.error = Some(error.into());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dismiss(cx: &mut Context<Self>) -> Dismiss {
        let weak = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.open = None;
                cx.notify();
            });
        })
    }

    fn toggle(
        picker: ExportPicker,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _, _, cx| {
            this.open = (this.open != Some(picker)).then_some(picker);
            cx.notify();
        })
    }
}

impl Render for ExportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let agent = find_agent(&self.host, &self.id, cx);
        if let Some(agent) = &agent {
            self.name = agent.name.clone();
        }
        let destination_url = destination(cx).map(|(url, _)| url).unwrap_or_default();
        let review = agent
            .as_ref()
            .ok_or_else(|| "This agent no longer exists.".to_string())
            .and_then(|agent| review(agent, &self.host, cx));
        let key = match (&agent, &review) {
            (Some(agent), Ok(review)) => Some(Self::review_key(agent, review, &destination_url)),
            _ => None,
        };
        if self.current != key {
            self.current = key.clone();
            self.changed_during_export = true;
            self.reviewed = None;
            self.confirmed = None;
        }
        let reviewed = key.is_some() && self.reviewed == key;
        let confirmed = reviewed
            && key
                .as_ref()
                .is_some_and(|key| self.confirmed == Some((key.clone(), self.level)));
        let pending = self.pending;
        let memory_disabled = pending
            || !agent
                .as_ref()
                .is_some_and(|agent| Self::memory_allowed(agent, cx));
        let level = self.level;
        let format = self.format;
        let entity = cx.entity().downgrade();
        let pick_level = |next: MemoryLevel| {
            let weak = entity.clone();
            Rc::new(move |_: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    this.confirmed = None;
                    this.level = next;
                    this.open = None;
                    cx.notify();
                });
            }) as WindowAction
        };
        let pick_format = |next: Format| {
            let weak = entity.clone();
            Rc::new(move |_: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |this, cx| {
                    this.format = next;
                    this.open = None;
                    cx.notify();
                });
            }) as WindowAction
        };
        let memories = inline_select(
            "export-memories",
            "Memories",
            level_label(crate::snapshot::Kind::Agent, level),
            self.open == Some(ExportPicker::Memories),
            memory_disabled,
            LEVELS
                .iter()
                .map(|&l| {
                    (
                        level_label(crate::snapshot::Kind::Agent, l),
                        l == level,
                        pick_level(l),
                    )
                })
                .collect(),
            Self::toggle(ExportPicker::Memories, cx),
            Self::dismiss(cx),
            window,
        );
        let formats = inline_select(
            "export-format",
            "File format",
            match format {
                Format::Png => "PNG",
                Format::Json => "JSON",
            }
            .into(),
            self.open == Some(ExportPicker::Format),
            pending,
            vec![
                (
                    "PNG".into(),
                    format == Format::Png,
                    pick_format(Format::Png),
                ),
                (
                    "JSON".into(),
                    format == Format::Json,
                    pick_format(Format::Json),
                ),
            ],
            Self::toggle(ExportPicker::Format, cx),
            Self::dismiss(cx),
            window,
        );
        let review_json = review
            .as_ref()
            .map(AgentSnapshot::pretty)
            .unwrap_or_else(|_| "null".into());
        let artwork_source = agent
            .as_ref()
            .and_then(|agent| agent.picture.clone())
            .filter(|p| !p.is_empty())
            .filter(|_| {
                review
                    .as_ref()
                    .map_or(true, |review| review.profile.avatar_url.is_none())
            });
        let details = self.details;
        let review_key = key.clone();
        let confirm_key = key.clone();
        let body = div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(memories)
            .child(formats)
            .child(paragraph(
                "Portable configuration includes the agent name, instructions, model, provider, \
                 worker count and avatar URL when present. PNG exports may also embed artwork \
                 from the source avatar URL shown below. Review the actual values before \
                 sharing: free text can contain secrets that automated checks cannot detect.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(
                        summary(
                            "export-review",
                            "Review portable configuration",
                            details,
                            &c,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.details = !this.details;
                            cx.notify();
                        })),
                    )
                    .when(details, |this| {
                        this.child(pre("export-review-json", review_json, 12., &c))
                            .children(
                                artwork_source
                                    .map(|source| paragraph(format!("Artwork source: {source}"))),
                            )
                    }),
            )
            .child(
                Checkbox::new("export-reviewed", reviewed)
                    .label(
                        "I reviewed the portable configuration and understand it may contain \
                         private information.",
                    )
                    .disabled(pending)
                    .on_change(checked(cx, move |this, checked, cx| {
                        this.reviewed = if checked { review_key.clone() } else { None };
                        cx.notify();
                    })),
            )
            .when(level != MemoryLevel::None, |this| {
                this.child(plaintext_warning(&c))
                    .child(paragraph(match level {
                        MemoryLevel::Core => "Core memory will be included.",
                        _ => "Core memory and all mem/* entries will be included.",
                    }))
                    .child(
                        Checkbox::new("export-confirmed", confirmed)
                            .label("I confirm that I want to include memory in this snapshot.")
                            .disabled(pending)
                            .on_change(checked(cx, move |this, checked, cx| {
                                this.confirmed = match (&confirm_key, checked) {
                                    (Some(key), true) if this.reviewed.as_ref() == Some(key) => {
                                        Some((key.clone(), this.level))
                                    }
                                    _ => None,
                                };
                                cx.notify();
                            })),
                    )
            })
            .children(review.as_ref().err().map(|error| alert(error.clone(), &c)))
            .children(self.error.clone().map(|error| alert(error, &c)));
        Dialog::new("export-agent", format!("Export {}", self.name))
            .when(!pending, |this| {
                this.on_close(cx.listener(|_, _, _, cx| close(cx)))
            })
            .child(body)
            .footer(
                Button::new("export-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(pending)
                    .on_click(|_, _, cx| close(cx)),
            )
            .footer(
                Button::new("export-confirm", "Export")
                    .prominent()
                    .loading(pending)
                    .disabled(
                        pending
                            || review.is_err()
                            || !reviewed
                            || (level != MemoryLevel::None && !confirmed),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
            )
    }
}

// ---------------------------------------------------------------------------
// Import

/// "Import agent snapshot" (`AgentSnapshotImport`). With `received` bytes
/// (a chat attachment or link) it previews them; importing still takes an
/// explicit click.
pub struct ImportDialog {
    hosts: Vec<Entity<HostConnection>>,
    host: Option<Entity<HostConnection>>,
    host_open: bool,
    received: bool,
    snapshot: Option<AgentSnapshot>,
    artwork: Option<Vec<u8>>,
    restore_memory: bool,
    /// Whether the chosen host can restore memory; `None` while asking,
    /// or until the host answers.
    restore_supported: Option<bool>,
    probing: bool,
    manifest_open: bool,
    file_error: Option<SharedString>,
    memory_error: Option<SharedString>,
    profile_result: Option<SharedString>,
    profile_published: bool,
    busy: bool,
    result: Option<SharedString>,
    created: Option<String>,
    memory_retry: Option<(String, Vec<MemoryEntry>)>,
    /// Identifies this create to the host, so a retry reuses the key.
    request_id: String,
    _observers: Vec<Subscription>,
}

impl LayerDialog for ImportDialog {
    fn closable(&self) -> bool {
        !self.busy
    }
}

/// Opens the import dialog; with `received`, previews those bytes.
pub(crate) fn open_import(
    hosts: Vec<Entity<HostConnection>>,
    received: Option<Vec<u8>>,
    _: &mut Window,
    cx: &mut App,
) {
    let dialog = cx.new(|cx| ImportDialog::new(hosts, received, cx));
    show(dialog, cx);
}

impl ImportDialog {
    fn new(
        hosts: Vec<Entity<HostConnection>>,
        received: Option<Vec<u8>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = hosts
            .iter()
            .find(|host| connected(host, cx))
            .or(hosts.first())
            .cloned();
        let observers = hosts
            .iter()
            .map(|host| {
                cx.observe(host, |this: &mut Self, host, cx| {
                    // Ask again once a host that could not answer connects.
                    if this.restore_supported.is_none()
                        && !this.probing
                        && this.host.as_ref().map(Entity::entity_id) == Some(host.entity_id())
                        && connected(&host, cx)
                    {
                        this.probe_restore(cx);
                    }
                    cx.notify();
                })
            })
            .collect();
        let mut dialog = Self {
            hosts,
            host: None,
            host_open: false,
            received: received.is_some(),
            snapshot: None,
            artwork: None,
            restore_memory: false,
            restore_supported: None,
            probing: false,
            manifest_open: false,
            file_error: None,
            memory_error: None,
            profile_result: None,
            profile_published: false,
            busy: false,
            result: None,
            created: None,
            memory_retry: None,
            request_id: uuid::Uuid::new_v4().to_string(),
            _observers: observers,
        };
        if let Some(host) = host {
            dialog.set_host(host, cx);
        }
        if let Some(bytes) = received {
            dialog.load(bytes, cx);
        }
        dialog
    }

    fn set_host(&mut self, host: Entity<HostConnection>, cx: &mut Context<Self>) {
        if self.host.as_ref().map(Entity::entity_id) == Some(host.entity_id()) {
            return;
        }
        self.host = Some(host.clone());
        self.request_id = uuid::Uuid::new_v4().to_string();
        self.restore_supported = None;
        self.restore_memory = false;
        self.probe_restore(cx);
        cx.notify();
    }

    /// Asks the chosen host whether it can restore memory.
    fn probe_restore(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        self.probing = true;
        let probe = host.update(cx, |host, cx| {
            host.request(
                Request::RestoreMemory {
                    id: String::new(),
                    owner: String::new(),
                    entries: Vec::new(),
                },
                cx,
            )
        });
        cx.spawn(async move |this, cx| {
            // Hosts from before snapshots answer the probe as unsupported;
            // one that could not answer stays unknown.
            let supported = match probe.await {
                Ok(Response::MemoryRestored { .. }) => Some(true),
                Ok(_) | Err(buzz_host_rpc::RpcError::Failed(_)) => Some(false),
                Err(_) => None,
            };
            let _ = this.update(cx, |this, cx| {
                if this.host.as_ref().map(Entity::entity_id) == Some(host.entity_id()) {
                    this.probing = false;
                    this.restore_supported = supported;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn supports_restore(&self) -> bool {
        self.restore_supported == Some(true)
    }

    /// Parses a file's bytes into the preview.
    fn load(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.snapshot = None;
        self.artwork = None;
        self.restore_memory = false;
        self.file_error = None;
        self.request_id = uuid::Uuid::new_v4().to_string();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let parsed = cx
                .background_executor()
                .spawn(async move {
                    let snapshot = parse_agent_snapshot(&bytes, false)?;
                    let artwork = match snapshot.profile.avatar_data_url {
                        Some(_) => None,
                        None => png::artwork(&bytes)?,
                    };
                    Ok::<_, String>((snapshot, artwork))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match parsed {
                    Ok((snapshot, artwork)) => {
                        this.snapshot = Some(snapshot);
                        this.artwork = artwork;
                    }
                    Err(error) => this.file_error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Development only: a headless session has no file dialog.
        let dev_file = std::env::var_os("BUZZ_DEV_PICK_FILE").map(PathBuf::from);
        let paths = dev_file.is_none().then(|| {
            cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Choose file".into()),
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match (dev_file, paths) {
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
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(error) = legacy_agent_file_error(&name) {
                let _ = this.update(cx, |this, cx| {
                    this.snapshot = None;
                    this.artwork = None;
                    this.file_error = Some(error.into());
                    cx.notify();
                });
                return;
            }
            let bytes = cx
                .background_executor()
                .spawn(async move {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    if size > MAX_PNG_BYTES as u64 {
                        return Err(TOO_LARGE.to_string());
                    }
                    std::fs::read(&path).map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| match bytes {
                Ok(bytes) => this.load(bytes, cx),
                Err(error) => {
                    this.snapshot = None;
                    this.file_error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn unsupported(&self) -> Vec<String> {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot_limitations(snapshot, false))
            .unwrap_or_default()
    }

    fn can_import(&self, cx: &App) -> Result<(), String> {
        let snapshot = self.snapshot.as_ref().ok_or("")?;
        let host = self.host.as_ref().ok_or("Choose a host.")?;
        if !connected(host, cx) {
            return Err(format!("{} is not connected.", host.read(cx).label));
        }
        let (url, _) = destination(cx).ok_or("Choose a community.")?;
        if !url.starts_with("wss://") {
            return Err("Agents can only join wss:// communities.".into());
        }
        identity(cx).ok_or("Set up your identity first.")?;
        if self.created.is_some()
            || !self.unsupported().is_empty()
            || (self.restore_memory
                && !snapshot.memory.entries.is_empty()
                && !self.supports_restore())
        {
            return Err(String::new());
        }
        Ok(())
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.can_import(cx).is_err() {
            return;
        }
        let (Some(snapshot), Some(host), Some((relay_url, _)), Some(identity), Some(community)) = (
            self.snapshot.clone(),
            self.host.clone(),
            destination(cx),
            identity(cx),
            AppState::current(cx),
        ) else {
            return;
        };
        self.busy = true;
        self.file_error = None;
        cx.notify();
        let harnesses = host.read(cx).harnesses.clone().unwrap_or_default();
        let media = community.read(cx).session.media().clone();
        let artwork = self.artwork.clone();
        let request_id = self.request_id.clone();
        let restore = self.restore_memory && !snapshot.memory.entries.is_empty();
        cx.spawn(async move |this, cx| {
            let created: Result<String, String> = async {
                let mut edit = snapshot_import_edit(&snapshot, &harnesses, false)?;
                // Embedded pictures become relay media first.
                let upload = match (&snapshot.profile.avatar_data_url, artwork) {
                    (Some(data), _) => Some(avatar_file(data)?),
                    (None, Some(artwork)) => Some(buzz_relay_client::upload::UploadFile {
                        name: "snapshot-avatar.png".into(),
                        mime: "image/png".into(),
                        bytes: artwork,
                    }),
                    _ => None,
                };
                if let Some(file) = upload {
                    let uploaded = buzz_relay_client::upload::upload(&media, file)
                        .await
                        .map_err(|error| error.to_string())?;
                    edit.picture = Some(uploaded.url);
                }
                cx.update(|cx| create_agent(&host, request_id, relay_url, identity, edit, cx))
                    .await
            }
            .await;
            let _ =
                this.update(cx, |this, cx| {
                    match created {
                        Ok(id) => {
                            this.created = Some(id.clone());
                            this.result = Some(
                                format!(
                                    "{} was created successfully. Review settings and configure \
                                 local credentials in Edit, publish its profile, then Start the \
                                 agent.",
                                    snapshot.profile.display_name
                                )
                                .into(),
                            );
                            if restore {
                                this.restore(id, snapshot.memory.entries.clone(), cx);
                                return;
                            }
                        }
                        Err(error) => {
                            if let Some(host) = &this.host {
                                host.update(cx, |host, cx| host.refresh(cx).detach());
                            }
                            this.file_error = Some(
                            format!(
                                "{} Check Agents before retrying to avoid a duplicate identity.",
                                if error.is_empty() { "Import failed." } else { &error }
                            )
                            .into(),
                        );
                        }
                    }
                    this.busy = false;
                    cx.notify();
                });
        })
        .detach();
    }

    /// Restores `entries` under agent `id` and confirms them by reading the
    /// memories back.
    fn restore(&mut self, id: String, entries: Vec<MemoryEntry>, cx: &mut Context<Self>) {
        let (Some(host), Some(identity)) = (self.host.clone(), identity(cx)) else {
            return;
        };
        let Some(agent) = find_agent(&host, &id, cx) else {
            self.memory_retry = Some((id, entries.clone()));
            self.memory_error = Some(
                format!(
                    "Memory restoration unconfirmed for {} entries. The agent exists; check its \
                     memories before retrying.",
                    entries.len()
                )
                .into(),
            );
            self.busy = false;
            cx.notify();
            return;
        };
        self.busy = true;
        cx.notify();
        let origin = RelayOrigin::parse(&agent.relay_url).map_err(|_| "Invalid agent community");
        let pubkey = agent.pubkey.clone();
        let owner = identity.pubkey_hex();
        let read = {
            let (origin, identity, pubkey) = (origin.clone(), identity.clone(), pubkey.clone());
            on_tokio(
                cx,
                async move { memory::read(origin?, identity, &pubkey).await },
            )
        };
        let expected = entries.len();
        cx.spawn(async move |this, cx| {
            let outcome: Result<(usize, Vec<String>), String> = async {
                let previous = read.await?;
                if previous.partial {
                    return Err("Memory listing is incomplete; restore was not started".into());
                }
                // A retry keeps entries that are already current.
                let mut unchanged = 0;
                let mut writes = Vec::new();
                for entry in &entries {
                    match previous.get(&entry.slug) {
                        Some(current) if current.body == entry.body => unchanged += 1,
                        current => writes.push(MemoryRestore {
                            slug: entry.slug.clone(),
                            body: entry.body.clone(),
                            after: current.map(|c| c.created_at),
                        }),
                    }
                }
                if writes.is_empty() {
                    return Ok((unchanged, Vec::new()));
                }
                let request = Request::RestoreMemory {
                    id: id.clone(),
                    owner,
                    entries: writes,
                };
                let response = cx
                    .update(|cx| host.update(cx, |host, cx| host.request(request, cx)))
                    .await
                    .map_err(|error| error.to_string())?;
                let Response::MemoryRestored {
                    published,
                    mut errors,
                } = response
                else {
                    return Err("The host sent an unexpected reply.".into());
                };
                let listing = cx
                    .update(|cx| {
                        on_tokio(
                            cx,
                            async move { memory::read(origin?, identity, &pubkey).await },
                        )
                    })
                    .await?;
                let mut confirmed = 0;
                for restored in published {
                    if !listing.partial
                        && listing
                            .get(&restored.slug)
                            .is_some_and(|current| current.event_id == restored.event_id)
                    {
                        confirmed += 1;
                    } else {
                        errors.push(format!(
                            "{}: memory restore failed; retry this agent",
                            restored.slug
                        ));
                    }
                }
                Ok((unchanged + confirmed, errors))
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok((written, errors)) if written == expected && errors.is_empty() => {
                        this.memory_retry = None;
                        this.memory_error = None;
                    }
                    Ok((written, errors)) => {
                        this.memory_retry = Some((id, entries));
                        this.memory_error = Some(
                            format!(
                                "Memory partially restored: {written} of {expected} entries \
                                 confirmed. The agent exists but some memory entries were not \
                                 confirmed. {}",
                                errors.join("; ")
                            )
                            .trim_end()
                            .to_string()
                            .into(),
                        );
                    }
                    Err(error) => {
                        this.memory_retry = Some((id, entries));
                        this.memory_error = Some(
                            format!(
                                "Memory restoration unconfirmed for {expected} entries. The \
                                 agent exists; check its memories before retrying. {error}"
                            )
                            .into(),
                        );
                    }
                }
                this.busy = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn retry_memory(&mut self, cx: &mut Context<Self>) {
        let Some((id, entries)) = self.memory_retry.clone() else {
            return;
        };
        if self.busy || self.created.as_ref() != Some(&id) {
            return;
        }
        if !self.host.as_ref().is_some_and(|host| connected(host, cx)) {
            self.memory_error = Some(
                "Could not confirm local agent status. Check your connection and retry memory \
                 restore for this agent."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.restore(id, entries, cx);
    }

    fn publish_profile(&mut self, cx: &mut Context<Self>) {
        let (Some(host), Some(id)) = (self.host.clone(), self.created.clone()) else {
            return;
        };
        self.busy = true;
        cx.notify();
        let response = host.update(cx, |host, cx| {
            host.request(Request::PublishProfile { id }, cx)
        });
        cx.spawn(async move |this, cx| {
            let ok = response.await.is_ok();
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.profile_published = ok;
                this.profile_result = Some(
                    if ok {
                        "Profile published."
                    } else {
                        "Profile publication unconfirmed. Review agent status and retry from \
                         Manage."
                    }
                    .into(),
                );
                cx.notify();
            });
        })
        .detach();
    }

    fn host_select(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if self.hosts.len() < 2 {
            return None;
        }
        let options = self
            .hosts
            .iter()
            .map(|host| {
                let state = host.read(cx);
                let label: SharedString = if connected(host, cx) {
                    state.label.clone()
                } else {
                    format!("{} (offline)", state.label).into()
                };
                let checked = self.host.as_ref().map(Entity::entity_id) == Some(host.entity_id());
                let weak = cx.entity().downgrade();
                let pick = host.clone();
                let action: WindowAction = Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.host_open = false;
                        this.set_host(pick.clone(), cx);
                    });
                });
                (label, checked, action)
            })
            .collect();
        let value = self
            .host
            .as_ref()
            .map(|host| host.read(cx).label.clone())
            .unwrap_or_else(|| "Choose a host".into());
        let weak = cx.entity().downgrade();
        Some(inline_select(
            "import-host",
            "Host",
            value,
            self.host_open,
            self.busy,
            options,
            cx.listener(|this, _, _, cx| {
                this.host_open = !this.host_open;
                cx.notify();
            }),
            Rc::new(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.host_open = false;
                    cx.notify();
                });
            }),
            window,
        ))
    }

    fn preview(&self, snapshot: &AgentSnapshot, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let d = &snapshot.definition;
        let entries = snapshot.memory.entries.len();
        let unsupported = self.unsupported();
        let supported = self.supports_restore();
        let manifest_open = self.manifest_open;
        div()
            .flex()
            .flex_col()
            .gap(rems(1.))
            .child(
                div()
                    .text_label()
                    .child(snapshot.profile.display_name.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(paragraph("Agent instructions"))
                    .child(paragraph(
                        "Review the instructions this agent will follow after import.",
                    ))
                    .child(pre(
                        "import-prompt",
                        d.system_prompt
                            .clone()
                            .filter(|p| !p.is_empty())
                            .unwrap_or_else(|| "No system prompt included.".into()),
                        12.,
                        &c,
                    )),
            )
            .child(paragraph(
                "A new agent will be created with a fresh keypair. The imported agent is \
                 independent of the source — identity never travels.",
            ))
            .child(if entries > 0 {
                let scope = if snapshot.memory.level == MemoryLevel::Core {
                    "core"
                } else {
                    "all"
                };
                let noun = if entries == 1 { "entry" } else { "entries" };
                let strong = format!("{entries} {scope} memory {noun}");
                let text = format!(
                    "This snapshot includes {strong}. Memory is plaintext in the file. It is \
                     omitted from import unless you explicitly choose to restore it."
                );
                let start = "This snapshot includes ".len();
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(div().text_body().text_color(c.text_danger).child(
                        StyledText::new(text).with_highlights([(
                            start..start + strong.len(),
                            HighlightStyle {
                                font_weight: Some(FontWeight::BOLD),
                                ..Default::default()
                            },
                        )]),
                    ))
                    .child(
                        Checkbox::new("import-restore-memory", self.restore_memory)
                            .label("Restore memory under the new agent identity")
                            .disabled(self.busy || !supported)
                            .on_change(checked(cx, |this, checked, cx| {
                                this.restore_memory = checked;
                                cx.notify();
                            })),
                    )
                    .when(self.restore_supported == Some(false), |this| {
                        this.child(alert(
                            "Memory restoration is unavailable on this host. You can still \
                             import the agent configuration without memory.",
                            &c,
                        ))
                    })
                    .into_any_element()
            } else {
                paragraph("No memory included — config only.").into_any_element()
            })
            .when(!unsupported.is_empty(), |this| {
                this.child(alert(
                    format!(
                        "Import is blocked: {}. These settings cannot be applied faithfully on \
                         this host.",
                        unsupported.join("; ")
                    ),
                    &c,
                ))
            })
            .child(paragraph(format!(
                "Runtime: {}. Response policy: {}. Parallelism: {}. Session policy: {}.",
                d.runtime.as_deref().unwrap_or("unspecified"),
                d.respond_to.as_deref().unwrap_or("unspecified"),
                d.parallelism
                    .map_or_else(|| "unspecified".to_string(), |n| n.to_string()),
                d.session_policy.as_deref().unwrap_or("channel"),
            )))
            .child(paragraph(
                "Review imported model and harness settings in Edit. Local provider \
                 credentials and environment overrides must be configured on this device.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(
                        summary(
                            "import-manifest",
                            "Full embedded manifest",
                            manifest_open,
                            &c,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.manifest_open = !this.manifest_open;
                            cx.notify();
                        })),
                    )
                    .when(manifest_open, |this| {
                        this.child(paragraph(
                            "This is the complete portable payload decoded from the file. \
                             Source identity is not part of the snapshot format.",
                        ))
                        .child(pre(
                            "import-manifest-json",
                            snapshot.pretty(),
                            16.,
                            &c,
                        ))
                    }),
            )
    }
}

/// The upload for a snapshot's `data:` avatar.
fn avatar_file(data: &str) -> Result<buzz_relay_client::upload::UploadFile, String> {
    const MALFORMED: &str = "Snapshot avatar data is malformed.";
    let (header, encoded) = data.split_once(',').ok_or(MALFORMED)?;
    let mime = header
        .strip_prefix("data:")
        .and_then(|rest| rest.split(';').next())
        .filter(|mime| !mime.is_empty())
        .ok_or(MALFORMED)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| MALFORMED)?;
    Ok(buzz_relay_client::upload::UploadFile {
        name: "snapshot-avatar".into(),
        mime: mime.into(),
        bytes,
    })
}

impl Render for ImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let busy = self.busy;
        let title = if self.result.is_some() {
            "Agent imported"
        } else {
            "Import agent snapshot"
        };
        let mut dialog = Dialog::new("import-snapshot", title).when(!busy, |this| {
            this.on_close(cx.listener(|_, _, _, cx| close(cx)))
        });
        if let Some(result) = self.result.clone() {
            let publish = self.created.is_some() && !self.profile_published;
            dialog = dialog
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(1.))
                        .child(paragraph(result))
                        .when(publish, |this| {
                            this.child(
                                div().flex().child(
                                    Button::new("import-publish", "Publish profile")
                                        .surface(Surface::Floating)
                                        .disabled(busy)
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.publish_profile(cx)),
                                        ),
                                ),
                            )
                        })
                        .children(self.file_error.clone().map(|e| alert(e, &c)))
                        .children(self.memory_error.clone().map(|e| alert(e, &c)))
                        .children(self.profile_result.clone().map(|text| {
                            if self.profile_published {
                                paragraph(text)
                            } else {
                                alert(text, &c)
                            }
                        })),
                )
                .when(self.memory_retry.is_some(), |this| {
                    this.footer(
                        Button::new("import-retry-memory", "Retry memory restore")
                            .surface(Surface::Floating)
                            .disabled(busy || !self.supports_restore())
                            .on_click(cx.listener(|this, _, _, cx| this.retry_memory(cx))),
                    )
                })
                .footer(
                    Button::new("import-close", "Close")
                        .surface(Surface::Floating)
                        .disabled(busy)
                        .on_click(|_, _, cx| close(cx)),
                );
            return dialog;
        }
        let blocked = self.can_import(cx);
        let host_select = self.host_select(window, cx);
        let preview = self
            .snapshot
            .clone()
            .map(|snapshot| self.preview(&snapshot, cx));
        let location_error = match &blocked {
            Err(error) if !error.is_empty() && self.snapshot.is_some() => Some(error.clone()),
            _ => None,
        };
        dialog
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(1.))
                    .children(host_select)
                    .when(!self.received, |this| {
                        this.child(
                            div().flex().child(
                                Button::new("import-choose", "Choose file")
                                    .outline()
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.choose_file(window, cx)
                                    })),
                            ),
                        )
                    })
                    .children(preview)
                    .children(location_error.map(|e| alert(e, &c)))
                    .when(busy, |this| this.child(paragraph("Creating agent…")))
                    .children(self.file_error.clone().map(|e| alert(e, &c))),
            )
            .footer(
                Button::new("import-confirm", "Import")
                    .prominent()
                    .loading(busy)
                    .disabled(busy || blocked.is_err())
                    .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
            )
            .footer(
                Button::new("import-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(busy)
                    .on_click(|_, _, cx| close(cx)),
            )
    }
}
