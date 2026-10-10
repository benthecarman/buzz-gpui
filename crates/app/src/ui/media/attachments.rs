//! Attachments under a message (ui.md §4.7): image strips, inline video,
//! audio and voice notes, and file cards. Message rows call
//! [`attachments`]; everything about how media renders lives here.

use super::super::message::Rows;
use super::super::renderers;
use buzz_relay_client::fold::{Attachment, AttachmentKind, MessageRow};
use buzz_ui::{ActiveTheme, Icon, IconName, Tooltip, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

/// Seeks a page's video to a time chip's seconds.
pub type OnTime = Rc<dyn Fn(f64, &mut Window, &mut App)>;
/// Opens an attachment of a row in the viewer, at a time for videos.
pub type OnOpen = Rc<dyn Fn(&Arc<MessageRow>, &Attachment, f64, &mut Window, &mut App)>;

/// How a page's rows treat media, beyond the timeline default.
#[derive(Clone, Default)]
pub struct RowMedia {
    /// Time chips are buttons calling this (`onMediaTime`); without it
    /// they are labels.
    pub on_time: Option<OnTime>,
    /// Replaces opening a new viewer (`onOpenMediaReview`).
    pub on_open: Option<OnOpen>,
    /// The viewer's comments: chips sit on the text's first line.
    pub inline_time: bool,
}

/// Attachments in send order; adjacent images share a strip and adjacent
/// files a group (§4.7). Videos and audio stand alone.
pub fn attachments(row: &Arc<MessageRow>, rows: &Rc<Rows>, cx: &App) -> impl IntoElement {
    let mut groups: Vec<(AttachmentKind, Vec<(usize, &Attachment)>)> = Vec::new();
    for (index, attachment) in row.attachments.iter().enumerate() {
        match groups.last_mut() {
            Some((kind, members)) if *kind == attachment.kind => members.push((index, attachment)),
            _ => groups.push((attachment.kind, vec![(index, attachment)])),
        }
    }
    let c = *cx.colors();
    let media = &rows.directory.media;
    div()
        .flex()
        .flex_col()
        .children(groups.into_iter().map(|(kind, members)| {
            if kind == AttachmentKind::Video {
                div()
                    .flex()
                    .flex_col()
                    .children(members.into_iter().map(|(index, attachment)| {
                        let (target, item, owner) = (row.clone(), attachment.clone(), rows.clone());
                        super::video_preview(
                            SharedString::from(format!("video-{}-{index}", row.id.to_hex())),
                            attachment,
                            &rows.directory.media,
                            Rc::new(move |window, cx| open(&owner, &target, &item, 0., window, cx)),
                        )
                    }))
                    .into_any_element()
            } else if kind == AttachmentKind::Audio {
                // Only relay media plays inline; other links stay file cards.
                div()
                    .flex()
                    .flex_col()
                    .children(members.into_iter().map(|(index, attachment)| {
                        let id = SharedString::from(format!("audio-{}-{index}", row.id.to_hex()));
                        if !media.is_relay_media(&attachment.url) {
                            file_tile(row, index, attachment, rows, &c)
                        } else if let Some(custom) =
                            renderers::attachment(attachment, id.clone(), media, cx)
                        {
                            custom
                        } else {
                            super::audio_attachment(id, attachment, media, cx)
                        }
                    }))
                    .into_any_element()
            } else if kind == AttachmentKind::Image {
                let count = members.len();
                div()
                    .mt(px(8.))
                    .flex()
                    .flex_col()
                    .child(div().flex().flex_wrap().gap(px(8.)).p(px(4.)).children(
                        members.into_iter().map(|(index, attachment)| {
                            image_tile(row, index, attachment, rows, &c)
                        }),
                    ))
                    .when(count > 1, |d| {
                        d.child(
                            div()
                                .mt(px(4.))
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child(format!("{count} images")),
                        )
                    })
                    .into_any_element()
            } else {
                div()
                    .mt(px(8.))
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .children(
                        members
                            .into_iter()
                            .map(|(index, attachment)| file_tile(row, index, attachment, rows, &c)),
                    )
                    .into_any_element()
            }
        }))
}

fn image_tile(
    row: &Arc<MessageRow>,
    index: usize,
    attachment: &Attachment,
    rows: &Rc<Rows>,
    c: &buzz_ui::Colors,
) -> AnyElement {
    let media = rows.directory.media.clone();
    let (target, item, rows) = (row.clone(), attachment.clone(), rows.clone());
    div()
        .id(SharedString::from(format!(
            "image-{}-{index}",
            row.id.to_hex()
        )))
        .relative()
        .size(px(72.))
        .flex_none()
        .rounded(px(16.))
        .overflow_hidden()
        .bg(c.surface_inset)
        .cursor_pointer()
        .child(super::thumbnail(
            &attachment.url,
            attachment.blurhash.as_deref(),
            &media,
        ))
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(px(16.))
                .border_1()
                .border_color(c.border_image),
        )
        .on_click(move |_, window, cx| open(&rows, &target, &item, 0., window, cx))
        .into_any_element()
}

/// Opens `attachment` in the viewer, or hands it to the page's opener.
fn open(
    rows: &Rows,
    row: &Arc<MessageRow>,
    attachment: &Attachment,
    seconds: f64,
    window: &mut Window,
    cx: &mut App,
) {
    match &rows.media.on_open {
        Some(on_open) => on_open(row, attachment, seconds, window, cx),
        None => super::open_review(row, &attachment.url, &rows.directory.media, window, cx),
    }
}

/// Files, and audio that does not play inline: a card that downloads
/// relay media to the Downloads folder or opens other links. Plain-HTTP
/// media cannot be fetched, so its card only says so.
fn file_tile(
    row: &Arc<MessageRow>,
    index: usize,
    attachment: &Attachment,
    rows: &Rc<Rows>,
    c: &buzz_ui::Colors,
) -> AnyElement {
    let media = rows.directory.media.clone();
    let insecure = attachment.insecure;
    let relay = !insecure && media.is_relay_media(&attachment.url);
    let name = attachment
        .name
        .clone()
        .or_else(|| mime_label(attachment.mime.as_deref()))
        .unwrap_or_else(|| "File".into());
    let meta = match attachment.size {
        _ if insecure => "File unavailable".into(),
        Some(size) => file_size(size),
        None if relay => "Download file".into(),
        None => "Open file".into(),
    };
    let icon = match attachment.kind {
        AttachmentKind::Video => IconName::VideoCamera,
        AttachmentKind::Audio => IconName::SpeakerHigh,
        _ => IconName::FileText,
    };
    let (url, download_name) = (attachment.url.clone(), attachment.name.clone());
    div()
        .id(SharedString::from(format!(
            "file-{}-{index}",
            row.id.to_hex()
        )))
        .flex()
        .items_center()
        .gap(px(8.))
        .max_w(px(384.))
        .min_w_0()
        .px(px(12.))
        .py(px(8.))
        .border_1()
        .border_color(c.border_standard)
        .rounded(px(12.))
        .bg(c.affordance_subtle)
        .when(!insecure, |d| {
            d.hover(|s| s.bg(c.affordance_subtle_hover))
                .cursor_pointer()
        })
        .tooltip(Tooltip::text(name.clone()))
        .child(Icon::new(icon).size(24.))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(name),
                )
                .child(div().text_caption().text_color(c.text_subtle).child(meta)),
        )
        .when(relay, |d| d.child(Icon::new(IconName::Download).size(20.)))
        .on_click(move |_, _, cx| {
            if insecure {
                return;
            }
            if relay {
                super::download(url.clone(), download_name.clone(), media.clone(), cx);
            } else {
                cx.open_url(&url);
            }
        })
        .into_any_element()
}

/// `formatFileSize`.
fn file_size(size: u64) -> String {
    const K: u64 = 1024;
    let round = |unit: u64| (size as f64 / unit as f64).round() as u64;
    match size {
        s if s < K => format!("{s} B"),
        s if s < K * K => format!("{} KB", round(K)),
        s if s < K * K * K => format!("{} MB", round(K * K)),
        _ => format!("{} GB", round(K * K * K)),
    }
}

fn mime_label(mime: Option<&str>) -> Option<String> {
    let (kind, subtype) = mime?.split_once('/')?;
    if kind.is_empty() || subtype.is_empty() {
        return None;
    }
    if subtype == "pdf" {
        return Some("PDF file".into());
    }
    subtype
        .bytes()
        .all(|b| b.is_ascii_alphanumeric())
        .then(|| format!("{} file", subtype.to_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn formats_file_sizes_like_the_reference() {
        assert_eq!(file_size(900), "900 B");
        assert_eq!(file_size(13 * 1024 + 100), "13 KB");
        assert_eq!(file_size(5 * 1024 * 1024), "5 MB");
        assert_eq!(
            mime_label(Some("application/pdf")).as_deref(),
            Some("PDF file")
        );
        assert_eq!(
            mime_label(Some("text/plain")).as_deref(),
            Some("PLAIN file")
        );
        assert_eq!(mime_label(Some("application/vnd.x+y")), None);
    }
}
