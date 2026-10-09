//! Media upload (`docs/port/relay.md` §5n.1-5n.3).
//!
//! A file is prepared (images lose their metadata), hashed and sent with
//! `PUT /upload` under a Blossom `upload` authorization for that hash. The
//! relay answers with a BUD-02 descriptor, which is checked against the
//! community origin before it can become an `imeta` tag. HEIC and video
//! conversions (ffmpeg in the reference) are not done here: such files
//! upload as they are.

use crate::media::{Media, server};
use crate::messages::{Attachment, media_byte_limit, validate_upload};
use buzz_identity::MediaVerb;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::sync::LazyLock;
use url::Url;

const MIB: u64 = 1024 * 1024;
/// `UPLOAD_MAX_BYTES`.
pub const MAX_UPLOAD: u64 = 500 * MIB;
/// Larger images are refused rather than decoded.
const MAX_PIXELS: u64 = 25_000_000;
/// The descriptor is small; anything bigger is not one.
const RESPONSE_LIMIT: usize = 8192;

/// `UPLOAD_FAILURES`, with the reference's wording.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UploadError {
    #[error(
        "File exceeds the supported limit: images 50 MiB, GIFs 10 MiB, documents 100 MiB, videos 500 MiB. The relay may enforce a lower limit."
    )]
    Size,
    #[error("This image could not be prepared for upload. Choose another copy or remove it.")]
    Image,
    #[error("Uploads are busy. Retry this file shortly.")]
    Capacity,
    #[error(
        "This file needs metadata cleanup before it can be uploaded. Choose an exported copy without metadata, or remove it for now."
    )]
    Metadata,
    #[error("The server could not accept this file. Its format or metadata may not be supported.")]
    Rejected,
    #[error("You don’t have permission to upload here.")]
    Denied,
    #[error("Upload did not finish. Retry or remove this file.")]
    Failed,
    #[error("The server returned an invalid upload result. Retry this file.")]
    Invalid,
}

/// A file as the user chose it.
#[derive(Clone, Debug)]
pub struct UploadFile {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// The image type from the first bytes (`imageType`).
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The type to declare: sniffed for images, else from the extension. The
/// relay sniffs again and ignores what the client says.
pub fn mime_type(name: &str, bytes: &[u8]) -> String {
    if let Some(image) = sniff_image(bytes) {
        return image.into();
    }
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "heic" => "image/heic",
        "heif" => "image/heif",
        _ => "application/octet-stream",
    }
    .into()
}

/// Strips image metadata (EXIF, ICC profiles, text chunks, comments) the
/// relay may refuse (`prepare-attachment.ts`). JPEGs are re-encoded at
/// quality 95 with their EXIF orientation applied; PNG, GIF and WebP keep
/// their pixels and lose only metadata chunks, animation included.
pub fn prepare(file: UploadFile) -> Result<UploadFile, UploadError> {
    let Some(kind) = sniff_image(&file.bytes) else {
        return Ok(file);
    };
    let bytes = match kind {
        "image/jpeg" => reencode_jpeg(&file.bytes),
        "image/png" => clean_png(&file.bytes),
        "image/gif" => clean_gif(&file.bytes),
        _ => clean_webp(&file.bytes),
    }
    .ok_or(UploadError::Image)?;
    Ok(UploadFile {
        name: file.name,
        mime: kind.into(),
        bytes,
    })
}

fn reencode_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    use image::codecs::jpeg::{JpegDecoder, JpegEncoder};
    use image::{DynamicImage, ImageDecoder};
    let mut decoder = JpegDecoder::new(Cursor::new(bytes)).ok()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 95)
        .encode_image(&image.into_rgb8())
        .ok()?;
    Some(out)
}

/// Keeps critical chunks and the ancillary ones that affect pixels or
/// animation; drops text, EXIF, ICC and time chunks.
fn clean_png(bytes: &[u8]) -> Option<Vec<u8>> {
    const KEEP: &[&[u8; 4]] = &[
        b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"sBIT", b"bKGD", b"pHYs", b"acTL", b"fcTL", b"fdAT",
    ];
    let mut out = bytes.get(..8)?.to_vec();
    let mut at = 8;
    let mut ended = false;
    while at < bytes.len() {
        let length = u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind: &[u8; 4] = bytes.get(at + 4..at + 8)?.try_into().ok()?;
        let end = at.checked_add(12)?.checked_add(length)?;
        let chunk = bytes.get(at..end)?;
        if kind == b"IHDR" {
            let width = u32::from_be_bytes(chunk.get(8..12)?.try_into().ok()?);
            let height = u32::from_be_bytes(chunk.get(12..16)?.try_into().ok()?);
            if u64::from(width) * u64::from(height) > MAX_PIXELS {
                return None;
            }
        }
        if kind[0].is_ascii_uppercase() || KEEP.contains(&kind) {
            out.extend_from_slice(chunk);
        }
        at = end;
        if kind == b"IEND" {
            ended = true;
            break;
        }
    }
    ended.then_some(out)
}

/// Copies the image data and the graphic-control and looping extensions;
/// drops comments and other application extensions.
fn clean_gif(bytes: &[u8]) -> Option<Vec<u8>> {
    let screen = bytes.get(..13)?;
    let width = u16::from_le_bytes([screen[6], screen[7]]);
    let height = u16::from_le_bytes([screen[8], screen[9]]);
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let color_table = |flags: u8| {
        if flags & 0x80 != 0 {
            3 << ((flags & 0x07) + 1)
        } else {
            0
        }
    };
    let mut at = 13 + color_table(screen[10]);
    let mut out = bytes.get(..at)?.to_vec();
    // The end of the sub-block chain starting at `from`.
    let blocks_end = |mut from: usize| -> Option<usize> {
        loop {
            let size = *bytes.get(from)? as usize;
            from += 1 + size;
            if size == 0 {
                return Some(from);
            }
        }
    };
    loop {
        match *bytes.get(at)? {
            0x3b => {
                out.push(0x3b);
                return Some(out);
            }
            0x2c => {
                let flags = *bytes.get(at + 9)?;
                let data = at + 10 + color_table(flags) + 1;
                let end = blocks_end(data)?;
                out.extend_from_slice(bytes.get(at..end)?);
                at = end;
            }
            0x21 => {
                let label = *bytes.get(at + 1)?;
                let end = blocks_end(at + 2)?;
                let keep = match label {
                    0xf9 | 0x01 => true,
                    0xff => {
                        let id = bytes.get(at + 3..at + 14)?;
                        id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0"
                    }
                    _ => false,
                };
                if keep {
                    out.extend_from_slice(bytes.get(at..end)?);
                }
                at = end;
            }
            _ => return None,
        }
    }
}

/// Drops the EXIF, XMP and ICC chunks and clears their `VP8X` flags.
fn clean_webp(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let mut at = 12;
    while at < bytes.len() {
        let kind = bytes.get(at..at + 4)?;
        let size = u32::from_le_bytes(bytes.get(at + 4..at + 8)?.try_into().ok()?) as usize;
        let end = at.checked_add(8)?.checked_add(size + (size & 1))?;
        let chunk = bytes.get(at..end.min(bytes.len()))?;
        match kind {
            b"EXIF" | b"XMP " | b"ICCP" => {}
            b"VP8X" => {
                let mut chunk = chunk.to_vec();
                *chunk.get_mut(8)? &= !(0x20 | 0x08 | 0x04);
                let width = u32::from_le_bytes([*chunk.get(12)?, chunk[13], chunk[14], 0]) + 1;
                let height = u32::from_le_bytes([*chunk.get(15)?, chunk[16], chunk[17], 0]) + 1;
                if u64::from(width) * u64::from(height) > MAX_PIXELS {
                    return None;
                }
                out.extend_from_slice(&chunk);
            }
            _ => out.extend_from_slice(chunk),
        }
        at = end;
    }
    let size = u32::try_from(out.len() - 8).ok()?;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Some(out)
}

static MIME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^[a-z0-9.+-]+/[a-z0-9.+-]+$").expect("mime pattern"));

#[derive(serde::Deserialize)]
struct Descriptor {
    url: String,
    sha256: String,
    size: u64,
    #[serde(rename = "type")]
    mime: String,
}

/// Prepares and uploads one file to the community, returning what a
/// message needs for its `imeta` tag. Runs on the session's runtime.
pub async fn upload(media: &Media, file: UploadFile) -> Result<Attachment, UploadError> {
    let task = media.clone();
    media
        .runtime()
        .spawn(async move { upload_now(&task, file).await })
        .await
        .map_err(|_| UploadError::Failed)?
}

async fn upload_now(media: &Media, file: UploadFile) -> Result<Attachment, UploadError> {
    if file.bytes.is_empty() || file.bytes.len() as u64 > MAX_UPLOAD {
        return Err(UploadError::Size);
    }
    let file = tokio::task::spawn_blocking(move || prepare(file))
        .await
        .map_err(|_| UploadError::Image)??;
    let size = file.bytes.len() as u64;
    if size > media_byte_limit(&file.mime.to_ascii_lowercase()) {
        return Err(UploadError::Size);
    }
    let sha256 = hex(&Sha256::digest(&file.bytes));
    let base = Url::parse(&media.origin().http()).map_err(|_| UploadError::Failed)?;
    let endpoint = base.join("/upload").map_err(|_| UploadError::Failed)?;
    let authorization = media
        .identity()
        .media_auth(MediaVerb::Upload, &server(&endpoint), Some(&sha256))
        .map_err(|_| UploadError::Failed)?;
    let content_type = if MIME.is_match(&file.mime) {
        file.mime.as_str()
    } else {
        "application/octet-stream"
    };
    let mut response = media
        .relay_client()
        .put(endpoint.as_str())
        .header("Authorization", authorization)
        .header("Content-Type", content_type)
        .header("X-SHA-256", &sha256)
        .body(file.bytes)
        .send()
        .await
        .map_err(|_| UploadError::Failed)?;
    let status = response.status().as_u16();
    match status {
        401 | 403 => return Err(UploadError::Denied),
        413 => return Err(UploadError::Size),
        429 => return Err(UploadError::Capacity),
        _ => {}
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| UploadError::Failed)? {
        if body.len() + chunk.len() > RESPONSE_LIMIT {
            return Err(UploadError::Invalid);
        }
        body.extend_from_slice(&chunk);
    }
    if !(200..300).contains(&status) {
        return Err(failure(status, &body));
    }
    let descriptor: Descriptor = serde_json::from_slice(&body).map_err(|_| UploadError::Invalid)?;
    if descriptor.size != size {
        return Err(UploadError::Invalid);
    }
    let mut attachment = Attachment {
        name: file.name,
        url: descriptor.url,
        mime: descriptor.mime,
        size: descriptor.size,
        sha256: descriptor.sha256,
        voice_duration: None,
    };
    let (url, mime) =
        validate_upload(&attachment, media.origin()).map_err(|_| UploadError::Invalid)?;
    attachment.url = url;
    attachment.mime = mime;
    Ok(attachment)
}

/// A refusal's code from its JSON body, else by status (`readUploadResponse`).
fn failure(status: u16, body: &[u8]) -> UploadError {
    let code = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("code")?.as_str().map(str::to_owned));
    match code.as_deref() {
        Some("size") => UploadError::Size,
        Some("capacity") => UploadError::Capacity,
        Some("metadata") => UploadError::Metadata,
        Some("rejected") => UploadError::Rejected,
        Some("denied") => UploadError::Denied,
        Some("invalid") => UploadError::Invalid,
        Some("image") => UploadError::Image,
        _ if matches!(status, 400 | 415 | 422) => {
            if String::from_utf8_lossy(body)
                .to_ascii_lowercase()
                .contains("metadata")
            {
                UploadError::Metadata
            } else {
                UploadError::Rejected
            }
        }
        _ => UploadError::Failed,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&[0, 0, 0, 0]);
        out
    }

    #[test]
    fn png_loses_text_exif_and_icc_but_keeps_pixels_and_animation() {
        let mut ihdr = 2u32.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&2u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in [
            (b"IHDR", &ihdr[..]),
            (b"tEXt", b"Author\0me"),
            (b"eXIf", b"MM"),
            (b"iCCP", b"p\0\0x"),
            (b"acTL", &[0; 8]),
            (b"IDAT", b"data"),
            (b"IEND", b""),
        ] {
            png.extend(png_chunk(kind, data));
        }
        let clean = clean_png(&png).unwrap();
        let mut expected = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, data) in [
            (b"IHDR", &ihdr[..]),
            (b"acTL", &[0; 8]),
            (b"IDAT", b"data"),
            (b"IEND", b""),
        ] {
            expected.extend(png_chunk(kind, data));
        }
        assert_eq!(clean, expected);
        assert_eq!(clean_png(&png[..png.len() - 4]), None);
    }

    #[test]
    fn gif_loses_comments_and_foreign_extensions() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0]); // 1x1, 2-colour table
        gif.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        let looping = [
            0x21, 0xff, 11, b'N', b'E', b'T', b'S', b'C', b'A', b'P', b'E', b'2', b'.', b'0', 3, 1,
            0, 0, 0,
        ];
        let xmp = [
            0x21, 0xff, 11, b'X', b'M', b'P', b' ', b'D', b'a', b't', b'a', b'X', b'M', b'P', 2,
            b'h', b'i', 0,
        ];
        let comment = [0x21, 0xfe, 3, b'y', b'o', b'u', 0];
        let control = [0x21, 0xf9, 4, 0, 0, 0, 0, 0];
        let image = [0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, 2, 0x44, 0x01, 0];
        for part in [&looping[..], &xmp, &comment, &control, &image, &[0x3b]] {
            gif.extend_from_slice(part);
        }
        let clean = clean_gif(&gif).unwrap();
        let mut expected = gif[..19].to_vec();
        for part in [&looping[..], &control, &image, &[0x3b]] {
            expected.extend_from_slice(part);
        }
        assert_eq!(clean, expected);
    }

    #[test]
    fn webp_loses_exif_and_its_flag() {
        let mut vp8x = vec![0x08 | 0x20 | 0x10, 0, 0, 0];
        vp8x.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut out = kind.to_vec();
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            if data.len() % 2 == 1 {
                out.push(0);
            }
            out
        };
        let mut body = b"WEBP".to_vec();
        body.extend(chunk(b"VP8X", &vp8x));
        body.extend(chunk(b"VP8L", b"pix"));
        body.extend(chunk(b"EXIF", b"MMexif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&(body.len() as u32).to_le_bytes());
        webp.extend(body);
        let clean = clean_webp(&webp).unwrap();
        assert_eq!(sniff_image(&clean), Some("image/webp"));
        assert!(!clean.windows(4).any(|w| w == b"EXIF"));
        assert_eq!(clean[20], 0x10, "only the alpha flag stays");
        assert_eq!(
            u32::from_le_bytes(clean[4..8].try_into().unwrap()) as usize,
            clean.len() - 8
        );
    }

    #[test]
    fn jpegs_are_reencoded() {
        let image = image::RgbImage::from_pixel(4, 3, image::Rgb([200, 10, 10]));
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode_image(&image)
            .unwrap();
        let prepared = prepare(UploadFile {
            name: "photo.jpg".into(),
            mime: "image/jpeg".into(),
            bytes: jpeg,
        })
        .unwrap();
        assert_eq!(prepared.mime, "image/jpeg");
        assert_eq!(sniff_image(&prepared.bytes), Some("image/jpeg"));
    }

    #[test]
    fn guesses_types_and_maps_refusals() {
        assert_eq!(mime_type("a.PDF", b"%PDF"), "application/pdf");
        assert_eq!(mime_type("x.bin", b"\x89PNG\r\n\x1a\nrest"), "image/png");
        assert_eq!(mime_type("noext", b""), "application/octet-stream");
        assert_eq!(
            failure(400, br#"{"code":"capacity"}"#),
            UploadError::Capacity
        );
        assert_eq!(
            failure(422, b"EXIF metadata refused"),
            UploadError::Metadata
        );
        assert_eq!(failure(415, b"nope"), UploadError::Rejected);
        assert_eq!(failure(500, b""), UploadError::Failed);
    }
}
