//! Snapshots received in chat (buzz-app `SnapshotPreview.tsx`,
//! `SnapshotAttachment.tsx`, `snapshot-preview.ts`,
//! `snapshot-clipboard.ts`): the "Add agent" button under a snapshot
//! attachment, "Preview snapshot" on a media link, the dialog that reads the
//! file, and a pasted snapshot link becoming its attachment again.
//!
//! The message list, link menu and composer call in through the small
//! hooks here; the rest stays in this module.

use super::snapshots::{self, LayerDialog, alert, paragraph};
use crate::hosts::Hosts;
use crate::snapshot::{Kind, png, share};
use buzz_relay_client::{Attachment, Media, Session};
use buzz_ui::{ActiveTheme, Button, ButtonSize, Dialog, Surface};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// What to preview: a snapshot attachment, or a bare media link whose kind
/// the file decides.
#[derive(Clone, Debug)]
pub(crate) struct PreviewRequest {
    pub url: String,
    pub name: Option<String>,
    pub kind: Option<Kind>,
}

/// Opens a received team snapshot. Teams register one with
/// [`set_team_preview`]; without it a team reads "Team preview is
/// unavailable."
pub type TeamPreview = Rc<dyn Fn(Vec<u8>, &mut Window, &mut App) -> Result<(), String>>;

struct TeamHook(TeamPreview);

impl Global for TeamHook {}

/// Hook for team snapshots: what "Add team" opens once the file is read.
pub(crate) fn set_team_preview(preview: TeamPreview, cx: &mut App) {
    cx.set_global(TeamHook(preview));
}

/// Whether a message attachment is a snapshot file, by name
/// (`snapshotAttachmentKind`). Such attachments show as a file card with
/// "Add agent" or "Add team", whatever their media kind.
pub(crate) fn is_snapshot(attachment: &buzz_relay_client::fold::Attachment) -> bool {
    attachment
        .name
        .as_deref()
        .and_then(share::attachment_kind)
        .is_some()
}

/// A snapshot attachment's file card with its "Add agent" / "Add team"
/// button below.
pub(crate) fn attachment_card(
    tile: AnyElement,
    attachment: &buzz_relay_client::fold::Attachment,
    id: SharedString,
    media: &Media,
) -> AnyElement {
    let kind = attachment
        .name
        .as_deref()
        .and_then(share::attachment_kind)
        .unwrap_or(Kind::Agent);
    let available = !attachment.insecure && media.is_relay_media(&attachment.url);
    let request = PreviewRequest {
        url: attachment.url.clone(),
        name: attachment.name.clone(),
        kind: Some(kind),
    };
    let media = media.clone();
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap(px(8.))
        .child(tile)
        .child(
            Button::new(
                id,
                if kind == Kind::Team {
                    "Add team"
                } else {
                    "Add agent"
                },
            )
            .size(ButtonSize::Sm)
            .surface(Surface::Floating)
            .disabled(!available)
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                open_preview(request.clone(), media.clone(), window, cx)
            }),
        )
        .into_any_element()
}

/// Whether a link's menu offers "Preview snapshot": this community's
/// authenticated media (`canPreviewSnapshotLink`).
pub(crate) fn can_preview_link(session: &Session, url: &str) -> bool {
    share::can_preview_link(session.media(), url)
}

/// "Preview snapshot" on a link (`requestSnapshotLinkPreview`).
pub(crate) fn preview_link(session: &Session, url: &str, window: &mut Window, cx: &mut App) {
    if !can_preview_link(session, url) {
        crate::ui::notice::error("Snapshot preview is unavailable.", cx);
        return;
    }
    let request = PreviewRequest {
        url: url.to_owned(),
        name: None,
        kind: None,
    };
    open_preview(request, session.media().clone(), window, cx);
}

/// The attachment a pasted snapshot link stands for, if the clipboard
/// carries one for this community.
pub(crate) fn pasted_attachment(item: &ClipboardItem, session: &Session) -> Option<Attachment> {
    item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::String(text) => text
            .metadata
            .as_deref()
            .and_then(|html| share::parse_clipboard(html, session.media(), session.origin())),
        _ => None,
    })
}

/// Reads the file, then hands it to the importer for its kind.
pub(crate) fn open_preview(
    request: PreviewRequest,
    media: Media,
    window: &mut Window,
    cx: &mut App,
) {
    let dialog = cx.new(|cx| PreviewDialog::new(request, media, window, cx));
    snapshots::show(dialog, cx);
}

/// "Preview snapshot": loading, or the error with Retry.
pub struct PreviewDialog {
    request: PreviewRequest,
    media: Media,
    error: Option<SharedString>,
    _read: Option<Task<()>>,
}

impl LayerDialog for PreviewDialog {
    fn closable(&self) -> bool {
        true
    }
}

impl PreviewDialog {
    fn new(
        request: PreviewRequest,
        media: Media,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut dialog = Self {
            request,
            media,
            error: None,
            _read: None,
        };
        dialog.read(window, cx);
        dialog
    }

    fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        let (media, request) = (self.media.clone(), self.request.clone());
        self._read = Some(cx.spawn_in(window, async move |this, cx| {
            let result: Result<(Kind, Vec<u8>), String> = async {
                let name = request.name.as_deref();
                let bytes = share::read_attachment(&media, &request.url, name).await?;
                let kind = match request.kind {
                    Some(kind) => kind,
                    None => png::kind_of(&bytes)?,
                };
                Ok((kind, bytes))
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((Kind::Agent, bytes)) => {
                    let hosts = cx
                        .try_global::<Hosts>()
                        .map(|hosts| hosts.hosts.clone())
                        .unwrap_or_default();
                    snapshots::open_import(hosts, Some(bytes), window, cx);
                }
                Ok((Kind::Team, bytes)) => {
                    let opened = cx
                        .try_global::<TeamHook>()
                        .map(|hook| hook.0.clone())
                        .ok_or_else(|| "Team preview is unavailable.".to_string())
                        .and_then(|preview| preview(bytes, window, cx));
                    if let Err(error) = opened {
                        this.error = Some(error.into());
                        cx.notify();
                    }
                }
                Err(error) => {
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }
}

impl Render for PreviewDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let failed = self.error.is_some();
        Dialog::new("preview-snapshot", "Preview snapshot")
            .on_close(|_, _, cx| snapshots::close(cx))
            .child(match self.error.clone() {
                Some(error) => alert(error, &c),
                None => paragraph("Loading snapshot…"),
            })
            .footer(
                Button::new("preview-cancel", "Cancel")
                    .outline()
                    .on_click(|_, _, cx| snapshots::close(cx)),
            )
            .when(failed, |this| {
                this.footer(
                    Button::new("preview-retry", "Retry")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, window, cx| this.read(window, cx))),
                )
            })
    }
}
