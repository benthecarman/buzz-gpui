//! Avatar files before upload (`avatar-upload.ts`): only PNG, JPEG, GIF
//! and WebP up to 50 MiB. Metadata stripping happens in the relay
//! client's upload, as for message attachments.

use buzz_relay_client::upload::{UploadFile, sniff_image};
use std::path::Path;

const MAX_BYTES: u64 = 50 * 1024 * 1024;

/// Reads a chosen avatar, refusing anything but a still or animated web
/// image.
pub fn read(path: &Path) -> Result<UploadFile, String> {
    let size = std::fs::metadata(path)
        .map_err(|error| format!("Couldn’t read that file: {error}"))?
        .len();
    if size == 0 || size > MAX_BYTES {
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
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "avatar".into());
    Ok(UploadFile {
        name,
        mime: mime.into(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_images_and_refuses_other_files() {
        let dir = std::env::temp_dir().join(format!("buzz-avatar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        std::fs::write(
            &png,
            include_bytes!("../../../../../assets/icons/buzz-128.png"),
        )
        .unwrap();
        let file = read(&png).unwrap();
        assert_eq!(
            (file.name.as_str(), file.mime.as_str()),
            ("a.png", "image/png")
        );
        let text = dir.join("a.txt");
        std::fs::write(&text, b"hello").unwrap();
        assert!(read(&text).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
