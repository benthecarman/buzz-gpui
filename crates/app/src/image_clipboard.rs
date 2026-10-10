//! "Copy image" (buzz-app's `image_clipboard.rs`): one clipboard owned for
//! the app's lifetime, because on Linux a copied image is only available
//! while its owner runs. GPUI's own clipboard offers only text on Wayland,
//! so this goes through `arboard` as buzz-app does.

use std::sync::{Mutex, OnceLock};

/// The decoded image a clipboard takes.
#[derive(Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
}

/// Decodes PNG, JPEG, GIF or WebP bytes into RGBA.
pub fn decode(bytes: &[u8]) -> Result<Rgba, String> {
    let image = image::load_from_memory(bytes).map_err(|_| "Could not copy image".to_string())?;
    let rgba = image.to_rgba8();
    Ok(Rgba {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        bytes: rgba.into_raw(),
    })
}

/// Puts the image on the clipboard. Blocking: run it off the UI thread.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn write(bytes: &[u8]) -> Result<(), String> {
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    let image = decode(bytes)?;
    let mut stored = CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "Clipboard unavailable".to_string())?;
    if stored.is_none() {
        *stored = Some(arboard::Clipboard::new().map_err(|_| "Clipboard unavailable".to_string())?);
    }
    stored
        .as_mut()
        .expect("clipboard initialized")
        .set_image(arboard::ImageData {
            width: image.width,
            height: image.height,
            bytes: std::borrow::Cow::Owned(image.bytes),
        })
        .map_err(|_| "Could not copy image".to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
pub fn write(_bytes: &[u8]) -> Result<(), String> {
    let _ = (OnceLock::<()>::new(), Mutex::new(()));
    Err("Clipboard unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn decodes_to_rgba() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let rgba = decode(&png).unwrap();
        assert_eq!((rgba.width, rgba.height), (2, 3));
        assert_eq!(&rgba.bytes[..4], &[1, 2, 3, 255]);
        assert!(decode(b"not an image").is_err());
    }
}
