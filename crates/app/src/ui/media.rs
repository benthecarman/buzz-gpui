//! Attachment media: images, custom emoji and profile pictures, plus the
//! audio and video players and the media review viewer.
//!
//! GPUI's own URI loader goes through the app's HTTP client and cannot sign
//! the Blossom `get` authorization relay media needs (relay.md §5n.6), so
//! every remote image loads through the community's [`Media`] client as an
//! [`ImageSource::Custom`] instead. GPUI's asset cache keeps one load per
//! URL and redraws the view that asked once it lands.

mod audio;
mod engine;
mod review;
mod seek;
mod video;

pub use audio::{audio_attachment, voice_note};
pub use review::{open_review, review_overlay};
pub use video::video_preview;

use buzz_relay_client::Media;
use buzz_ui::{ActiveTheme, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a loaded thumbnail takes to fade in over its blurhash.
const FADE: Duration = Duration::from_millis(180);
/// Blurhash previews decode to this many pixels a side (`BLURHASH_SIZE`).
const BLURHASH_SIZE: u32 = 32;

/// One remote image request. Hashing by URL alone lets every view share a
/// load; a media client is tied to one community, and so is its URL.
#[derive(Clone)]
struct Remote {
    url: SharedString,
    /// Tried when `url` fails: avatars ask for the relay thumbnail first.
    fallback: Option<SharedString>,
    media: Media,
}

impl Hash for Remote {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.url.hash(state);
        self.fallback.hash(state);
    }
}

enum RemoteImage {}

impl Asset for RemoteImage {
    type Source = Remote;
    type Output = Result<Arc<Image>, SharedString>;

    // `async fn` would borrow the `App` argument past the call.
    #[allow(clippy::manual_async_fn)]
    fn load(source: Remote, _: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            let bytes = match source.media.fetch(&source.url).await {
                Ok(bytes) => bytes,
                Err(error) => match &source.fallback {
                    Some(fallback) => source.media.fetch(fallback).await,
                    None => Err(error),
                }
                .map_err(|error| SharedString::from(error.to_string()))?,
            };
            let format = image_format(&bytes).ok_or("Not a supported image")?;
            Ok(Arc::new(Image::from_bytes(format, bytes.to_vec())))
        }
    }
}

/// An image at `url`, at full size.
pub fn image(url: &str, media: &Media) -> ImageSource {
    source(Remote {
        url: url.to_string().into(),
        fallback: None,
        media: media.clone(),
    })
}

/// A profile picture: the relay's small thumbnail when it is relay media
/// (`mediaUrl(url, "small")`), falling back to the original.
pub fn avatar(url: &str, media: &Media) -> ImageSource {
    match media.thumbnail(url) {
        Some(thumbnail) => source(Remote {
            url: thumbnail.into(),
            fallback: Some(url.to_string().into()),
            media: media.clone(),
        }),
        None => image(url, media),
    }
}

fn source(remote: Remote) -> ImageSource {
    ImageSource::Custom(Arc::new(move |window, cx| {
        decoded(&remote, window, cx).map(|result| result.map_err(ImageCacheError::Asset))
    }))
}

/// The decoded image, `None` while it loads.
fn decoded(
    remote: &Remote,
    window: &mut Window,
    cx: &mut App,
) -> Option<Result<Arc<RenderImage>, SharedString>> {
    match window.use_asset::<RemoteImage>(remote, cx)? {
        Ok(image) => image.use_render_image(window, cx).map(Ok),
        Err(error) => Some(Err(error)),
    }
}

/// The pixel size of the image at `url` once it has loaded.
fn natural_size(url: &str, media: &Media, window: &mut Window, cx: &mut App) -> Option<Size<f32>> {
    let remote = Remote {
        url: url.to_string().into(),
        fallback: None,
        media: media.clone(),
    };
    let image = decoded(&remote, window, cx)?.ok()?;
    let size = image.size(0);
    Some(gpui_kit::size(size.width.0 as f32, size.height.0 as f32))
}

/// Packs BGRA pixels into the image type GPUI paints.
fn render_image(width: u32, height: u32, bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

/// A blurhash decoded to a small image (`paintBlurhash`). The hash was
/// validated when the event was folded.
enum Blurhash {}

impl Asset for Blurhash {
    type Source = SharedString;
    type Output = Option<Arc<RenderImage>>;

    #[allow(clippy::manual_async_fn)]
    fn load(
        hash: SharedString,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move { blurhash_image(&hash) }
    }
}

fn blurhash_image(hash: &str) -> Option<Arc<RenderImage>> {
    let mut pixels = blurhash::decode(hash, BLURHASH_SIZE, BLURHASH_SIZE, 1.).ok()?;
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    render_image(BLURHASH_SIZE, BLURHASH_SIZE, pixels)
}

/// Fade-in progress of one thumbnail, kept while it stays on screen.
struct Fade {
    started: Option<Instant>,
    /// Shown at full opacity: it was already loaded when first drawn, or
    /// its fade finished.
    settled: bool,
}

/// A thumbnail's pixels (`ImagePixels`): the blurhash, when there is one,
/// while the original loads, then the original fading in over it. Fills
/// its parent with `object-fit: cover`.
pub fn thumbnail(url: &str, blurhash: Option<&str>, media: &Media) -> impl IntoElement {
    let remote = Remote {
        url: url.to_string().into(),
        fallback: None,
        media: media.clone(),
    };
    let hash = blurhash.map(SharedString::from);
    Lazy::new("pixels", move |window, cx| {
        let c = *cx.colors();
        let image = decoded(&remote, window, cx);
        if let Some(Err(_)) = image {
            return unavailable(&c).into_any_element();
        }
        let loaded = image.is_some();
        let fade = window.use_keyed_state("fade", cx, |_, _| Fade {
            started: None,
            settled: loaded,
        });
        let opacity = fade.update(cx, |fade, cx| {
            if fade.settled || cx.reduce_motion() {
                return 1.;
            }
            if !loaded {
                return 0.;
            }
            let started = *fade.started.get_or_insert_with(Instant::now);
            let progress = started.elapsed().as_secs_f32() / FADE.as_secs_f32();
            fade.settled = progress >= 1.;
            progress.min(1.)
        });
        // Loading itself redraws the view once it lands; only the fade ticks.
        if loaded && opacity < 1. {
            window.request_animation_frame();
        }
        let preview = (opacity < 1.)
            .then(|| {
                hash.as_ref()
                    .and_then(|hash| window.use_asset::<Blurhash>(hash, cx).flatten())
            })
            .flatten();
        div()
            .relative()
            .size_full()
            .when_some(preview, |this, preview| {
                this.child(img(preview).absolute().inset_0().size_full())
            })
            .when_some(image.and_then(Result::ok), |this, image| {
                this.child(
                    img(image)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .opacity(opacity),
                )
            })
            .into_any_element()
    })
}

/// The "Image unavailable" tile (`.imageUnavailable`).
fn unavailable(c: &buzz_ui::Colors) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(4.))
        .rounded(px(8.))
        .bg(c.affordance_subtle_hover)
        .text_caption()
        .text_color(c.text_subtle)
        .child("Image unavailable")
}

/// Builds its content during layout, where the window is available for
/// asset loads and per-element state; row renderers only get `&App`.
pub struct Lazy {
    id: ElementId,
    build: Option<Build>,
}

type Build = Box<dyn FnOnce(&mut Window, &mut App) -> AnyElement>;

impl Lazy {
    pub fn new(
        id: impl Into<ElementId>,
        build: impl FnOnce(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            build: Some(Box::new(build)),
        }
    }
}

impl IntoElement for Lazy {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Lazy {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let build = self.build.take().expect("Lazy laid out twice");
        let mut element = build(window, cx);
        (element.request_layout(window, cx), element)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

/// `formatMediaTime`: `m:ss`, or `h:mm:ss` from an hour.
fn media_time(seconds: f64) -> String {
    let total = if seconds.is_finite() {
        seconds.max(0.).floor() as u64
    } else {
        0
    };
    let (hours, minutes, rest) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{rest:02}")
    } else {
        format!("{minutes}:{rest:02}")
    }
}

/// `videoTime`: [`media_time`] padded to `00:00`.
fn video_time(seconds: f64) -> String {
    format!("{:0>5}", media_time(seconds))
}

/// Tabular figures, so running times do not jitter.
fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// Sniffs the formats GPUI decodes from their magic bytes. Senders' MIME
/// claims are not trusted.
fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if starts(b"\xff\xd8\xff") {
        Some(ImageFormat::Jpeg)
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some(ImageFormat::Gif)
    } else if bytes.len() >= 12 && starts(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::Webp)
    } else if starts(b"BM") {
        Some(ImageFormat::Bmp)
    } else if starts(b"II*\0") || starts(b"MM\0*") {
        Some(ImageFormat::Tiff)
    } else if starts(b"\0\0\x01\0") {
        Some(ImageFormat::Ico)
    } else {
        None
    }
}

/// Saves an attachment to the Downloads folder without overwriting
/// anything there, then opens it with the system handler
/// (`media_download` in the reference host).
pub fn download(url: String, name: Option<String>, media: Media, cx: &mut App) {
    let directory = dirs::download_dir().unwrap_or_else(std::env::temp_dir);
    cx.spawn(async move |cx| {
        let bytes = match media.fetch(&url).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, "attachment download failed");
                return;
            }
        };
        let name = file_name(name.as_deref(), &url);
        let path = cx
            .background_executor()
            .spawn(async move {
                let path = unused_path(&directory, &name);
                std::fs::write(&path, &bytes).map(|()| path)
            })
            .await;
        match path {
            Ok(path) => {
                cx.update(|cx| cx.open_with_system(&path));
            }
            Err(error) => tracing::warn!(%error, "could not save the attachment"),
        }
    })
    .detach();
}

/// A safe file name: the sender's name without path separators or control
/// characters, else the URL's last segment.
fn file_name(name: Option<&str>, url: &str) -> String {
    let clean = |value: &str| {
        value
            .chars()
            .map(|c| {
                if c.is_control() || matches!(c, '/' | '\\') {
                    ' '
                } else {
                    c
                }
            })
            .collect::<String>()
            .trim()
            .trim_start_matches('.')
            .trim()
            .to_string()
    };
    name.map(clean)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            url.rsplit('/')
                .next()
                .map(clean)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| "File".into())
}

fn unused_path(directory: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| directory.join(format!("{stem} ({n}){extension}")))
        .find(|path| !path.exists())
        .expect("some name is free")
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn sniffs_formats() {
        assert_eq!(
            image_format(b"\x89PNG\r\n\x1a\nrest"),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            image_format(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(ImageFormat::Webp)
        );
        assert_eq!(image_format(b"<svg"), None);
    }

    #[test]
    fn names_downloads_safely() {
        assert_eq!(file_name(Some("../a/b.pdf"), "https://x/y"), "a b.pdf");
        assert_eq!(file_name(None, "https://x/media/abc.png"), "abc.png");
        assert_eq!(file_name(Some("  "), "https://x/"), "File");
    }

    #[test]
    fn formats_media_times_like_the_reference() {
        assert_eq!(media_time(0.), "0:00");
        assert_eq!(media_time(65.9), "1:05");
        assert_eq!(media_time(3_725.), "1:02:05");
        assert_eq!(media_time(f64::NAN), "0:00");
        assert_eq!(video_time(8.03), "00:08");
        assert_eq!(video_time(605.), "10:05");
        assert_eq!(video_time(3_725.), "1:02:05");
    }

    #[test]
    fn decodes_blurhash_previews() {
        let image = blurhash_image("LEHV6nWB2yk8pyo0adR*.7kCMdnj").expect("a valid hash decodes");
        assert_eq!(image.size(0), size(DevicePixels(32), DevicePixels(32)));
        // Swapped to BGRA, the order GPUI paints.
        let rgba = blurhash::decode("LEHV6nWB2yk8pyo0adR*.7kCMdnj", 32, 32, 1.).unwrap();
        let bgra = &image.as_bytes(0).unwrap()[..4];
        assert_eq!(bgra, [rgba[2], rgba[1], rgba[0], rgba[3]]);
    }
}
