//! Agent profile pictures: one file chosen, read off the UI thread and
//! uploaded to the agent's community.

use buzz_relay_client::upload::{UploadFile, sniff_image};
use gpui_kit::*;
use std::path::PathBuf;

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
