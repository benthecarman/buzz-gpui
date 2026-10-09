//! Images from message attachments, custom emoji and profile pictures.
//!
//! GPUI's own URI loader goes through the app's HTTP client and cannot sign
//! the Blossom `get` authorization relay media needs (relay.md §5n.6), so
//! every remote image loads through the community's [`Media`] client as an
//! [`ImageSource::Custom`] instead. GPUI's asset cache keeps one load per
//! URL and redraws the view that asked once it lands.

use buzz_relay_client::Media;
use gpui_kit::*;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

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
        match window.use_asset::<RemoteImage>(&remote, cx)? {
            Ok(image) => image.use_render_image(window, cx).map(Ok),
            Err(error) => Some(Err(ImageCacheError::Asset(error))),
        }
    }))
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
}
