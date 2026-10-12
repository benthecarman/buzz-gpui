//! Snapshot files: the manifest as JSON, or base64 JSON in a PNG `tEXt`
//! chunk with the keyword `buzz_agent_snapshot` (RFC 2083), the same
//! container buzz-app and block/buzz write.

use super::manifest::{AgentSnapshot, MAX_PNG_BYTES, TOO_LARGE, parse_agent_snapshot};
use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

pub const MAGIC: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const AGENT_KEYWORD: &[u8] = b"buzz_agent_snapshot\0";
const TEAM_KEYWORD: &[u8] = b"buzz_team_snapshot\0";
/// The artwork size an export draws the avatar at.
pub const ARTWORK_SIZE: u32 = 512;

const INVALID: &str = "Invalid PNG snapshot.";
const INVALID_ARTWORK: &str = "Invalid snapshot artwork.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Json,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Json => "json",
        }
    }
}

/// The CRC-32 of each byte value (RFC 2083, 15).
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut value = n as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ if value & 1 != 0 { 0xedb8_8320 } else { 0 };
            bit += 1;
        }
        table[n] = value;
        n += 1;
    }
    table
};

/// A chunk's CRC-32, over its type and data.
pub fn crc(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |value, &byte| {
        CRC_TABLE[((value ^ u32::from(byte)) & 0xff) as usize] ^ (value >> 8)
    })
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// One whole chunk: length, type, data and CRC.
pub fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let check = crc(&out[4..]);
    out.extend_from_slice(&check.to_be_bytes());
    out
}

/// One chunk of a PNG: its type, payload and full bytes.
pub struct Chunk<'a> {
    pub kind: [u8; 4],
    pub payload: &'a [u8],
    pub raw: &'a [u8],
}

impl Chunk<'_> {
    /// Whether the stored CRC matches the type and data.
    pub fn crc_ok(&self) -> bool {
        u32_at(self.raw, self.raw.len() - 4) == Some(crc(&self.raw[4..self.raw.len() - 4]))
    }
}

/// Every chunk after the signature as framed, without checking CRCs or
/// `IEND`. `None` when a chunk runs past the end.
pub fn framed(bytes: &[u8]) -> Option<Vec<Chunk<'_>>> {
    let mut out = Vec::new();
    let mut at = MAGIC.len();
    while at < bytes.len() {
        let length = u32_at(bytes, at)? as usize;
        let end = at.checked_add(12)?.checked_add(length)?;
        if end > bytes.len() {
            return None;
        }
        out.push(Chunk {
            kind: bytes[at + 4..at + 8].try_into().ok()?,
            payload: &bytes[at + 8..at + 8 + length],
            raw: &bytes[at..end],
        });
        at = end;
    }
    Some(out)
}

/// The chunks through `IEND`, CRCs checked. `None` for anything malformed.
pub fn chunks(bytes: &[u8]) -> Option<Vec<Chunk<'_>>> {
    if !bytes.starts_with(&MAGIC) {
        return None;
    }
    let out = framed(bytes)?;
    let end = out.iter().position(|c| &c.kind == b"IEND")?;
    let valid = end + 1 == out.len()
        && out
            .iter()
            .all(|c| c.payload.len() <= MAX_PNG_BYTES && c.crc_ok());
    valid.then_some(out)
}

fn lenient_base64() -> GeneralPurpose {
    GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
}

/// The manifest bytes a snapshot PNG carries (`pngManifest`).
pub fn manifest(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut offset = 8;
    let mut found: Option<Vec<u8>> = None;
    let mut image = false;
    let mut ended = false;
    while offset + 12 <= bytes.len() {
        let length = u32_at(bytes, offset).ok_or(INVALID)? as usize;
        if length > MAX_PNG_BYTES || offset + 12 + length > bytes.len() {
            return Err(INVALID.into());
        }
        let kind = &bytes[offset + 4..offset + 8];
        let payload = &bytes[offset + 8..offset + 8 + length];
        if u32_at(bytes, offset + 8 + length) != Some(crc(&bytes[offset + 4..offset + 8 + length]))
        {
            return Err(INVALID.into());
        }
        if offset == 8 {
            let (width, height) = (
                u32_at(payload, 0).unwrap_or(0),
                u32_at(payload, 4).unwrap_or(0),
            );
            if kind != b"IHDR"
                || length != 13
                || width == 0
                || height == 0
                || width > 2048
                || height > 2048
            {
                return Err(INVALID.into());
            }
        }
        if kind == b"IDAT" {
            image = true;
        }
        if kind == b"tEXt" && payload.starts_with(AGENT_KEYWORD) {
            if found.is_some() {
                return Err(INVALID.into());
            }
            let encoded = std::str::from_utf8(&payload[AGENT_KEYWORD.len()..])
                .map_err(|_| INVALID.to_string())?;
            let body = encoded.trim_end_matches('=');
            if body.is_empty()
                || !body
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
            {
                return Err(INVALID.into());
            }
            found = Some(
                lenient_base64()
                    .decode(encoded)
                    .map_err(|_| INVALID.to_string())?,
            );
        }
        offset += length + 12;
        if kind == b"IEND" {
            ended = true;
            break;
        }
    }
    match found {
        Some(found) if image && ended && offset == bytes.len() => Ok(found),
        _ => Err("PNG does not contain a buzz_agent_snapshot tEXt chunk.".into()),
    }
}

/// The transparent 1×1 placeholder image: filter 0 and one RGBA pixel in a
/// stored zlib block (Adler-32 0x00050001).
fn placeholder() -> Vec<Vec<u8>> {
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    let pixel = [
        0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, 0, 0, 0, 0, 0, 0x00, 0x05, 0x00, 0x01,
    ];
    vec![
        chunk(b"IHDR", &header),
        chunk(b"IDAT", &pixel),
        chunk(b"IEND", &[]),
    ]
}

/// Encodes a snapshot file (`encodeAgentSnapshot`). PNG `artwork` must be a
/// PNG this app drew; only its critical chunks are copied.
pub fn encode(
    snapshot: &AgentSnapshot,
    format: Format,
    artwork: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let manifest = snapshot.file_json();
    parse_agent_snapshot(&manifest, false)?;
    if format == Format::Json {
        return Ok(manifest);
    }
    let image = match artwork {
        Some(artwork) => {
            if artwork.len() > MAX_PNG_BYTES {
                return Err(INVALID_ARTWORK.into());
            }
            let parts: Vec<Vec<u8>> = chunks(artwork)
                .ok_or(INVALID_ARTWORK)?
                .into_iter()
                .filter(|c| matches!(&c.kind, b"IHDR" | b"IDAT" | b"IEND"))
                .map(|c| c.raw.to_vec())
                .collect();
            let first = parts.first().map(|c| &c[4..8]);
            let last = parts.last().map(|c| &c[4..8]);
            if parts.len() < 3 || first != Some(b"IHDR") || last != Some(b"IEND") {
                return Err(INVALID_ARTWORK.into());
            }
            parts
        }
        None => placeholder(),
    };
    let mut text = AGENT_KEYWORD.to_vec();
    text.extend_from_slice(
        base64::engine::general_purpose::STANDARD
            .encode(&manifest)
            .as_bytes(),
    );
    let mut png = MAGIC.to_vec();
    png.extend_from_slice(&image[0]);
    png.extend_from_slice(&chunk(b"tEXt", &text));
    for part in &image[1..] {
        png.extend_from_slice(part);
    }
    if png.len() > MAX_PNG_BYTES {
        return Err(TOO_LARGE.into());
    }
    parse_agent_snapshot(&png, false)?;
    Ok(png)
}

/// The PNG without its snapshot chunks, for use as an avatar: the manifest
/// (and any memory in it) must not travel with a public picture.
pub fn without_snapshot(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = MAGIC.to_vec();
    for c in chunks(bytes)? {
        let snapshot = &c.kind == b"tEXt"
            && (c.payload.starts_with(AGENT_KEYWORD) || c.payload.starts_with(TEAM_KEYWORD));
        if !snapshot {
            out.extend_from_slice(c.raw);
        }
    }
    Some(out)
}

/// The artwork a snapshot PNG carries, or `None` for the transparent
/// placeholder pixel and for JSON (`snapshotPngArtwork`). The caller has
/// parsed the manifest.
pub fn artwork(bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
    if !bytes.starts_with(&MAGIC) {
        return Ok(None);
    }
    let parts = chunks(bytes).ok_or(INVALID_ARTWORK)?;
    let animated = parts.iter().any(|c| &c.kind == b"acTL");
    let header = parts
        .first()
        .filter(|c| &c.kind == b"IHDR")
        .ok_or(INVALID_ARTWORK)?;
    let single = !animated
        && header.payload.len() == 13
        && u32_at(header.payload, 0) == Some(1)
        && u32_at(header.payload, 4) == Some(1);
    if single {
        let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map_err(|_| INVALID_ARTWORK)?;
        if image.to_rgba8().get_pixel(0, 0).0[3] == 0 {
            return Ok(None);
        }
    }
    without_snapshot(bytes)
        .map(Some)
        .ok_or(INVALID_ARTWORK.into())
}

/// Draws an avatar at 512×512 as PNG, as the reference's canvas does.
pub fn render_artwork(source: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory(source).ok()?;
    let resized = image::imageops::resize(
        &image.to_rgba8(),
        ARTWORK_SIZE,
        ARTWORK_SIZE,
        image::imageops::FilterType::Triangle,
    );
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(resized)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// Agent or team, from the container alone (`snapshotKind`). The matching
/// importer still validates the whole manifest.
pub fn kind_of(bytes: &[u8]) -> Result<super::Kind, String> {
    const UNSUPPORTED: &str = "Unsupported snapshot format.";
    if bytes.starts_with(&MAGIC) {
        let parts = chunks(bytes).ok_or(UNSUPPORTED)?;
        for (keyword, kind) in [
            (AGENT_KEYWORD, super::Kind::Agent),
            (TEAM_KEYWORD, super::Kind::Team),
        ] {
            if parts
                .iter()
                .any(|c| &c.kind == b"tEXt" && c.payload.starts_with(keyword))
            {
                return Ok(kind);
            }
        }
        return Err(UNSUPPORTED.into());
    }
    let value: serde_json::Value = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or(UNSUPPORTED)?;
    match value.get("format").and_then(|f| f.as_str()) {
        Some("buzz-agent-snapshot") => Ok(super::Kind::Agent),
        Some("buzz-team-snapshot") => Ok(super::Kind::Team),
        _ => Err(UNSUPPORTED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::manifest::{MemoryEntry, MemoryLevel};

    #[test]
    fn crc_matches_the_standard_check_values() {
        assert_eq!(crc(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc(b"IEND"), 0xae42_6082);
        assert_eq!(crc(b""), 0);
    }

    fn snapshot() -> AgentSnapshot {
        let json = br#"{"format":"buzz-agent-snapshot","version":1,
            "definition":{"name":"Scout","systemPrompt":"Hi","runtime":"buzz-agent"},
            "profile":{"displayName":"Scout"},"memory":{"level":"none"}}"#;
        parse_agent_snapshot(json, false).unwrap()
    }

    #[test]
    fn png_round_trips_with_the_placeholder() {
        let png = encode(&snapshot(), Format::Png, None).unwrap();
        assert!(png.starts_with(&MAGIC));
        assert_eq!(parse_agent_snapshot(&png, false).unwrap(), snapshot());
        // The placeholder is no artwork.
        assert_eq!(artwork(&png).unwrap(), None);
        assert_eq!(kind_of(&png).unwrap(), crate::snapshot::Kind::Agent);
        let json = encode(&snapshot(), Format::Json, None).unwrap();
        assert_eq!(kind_of(&json).unwrap(), crate::snapshot::Kind::Agent);
    }

    #[test]
    fn artwork_is_drawn_embedded_and_stripped() {
        let mut source = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            3,
            2,
            image::Rgba([200, 10, 10, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut source),
            image::ImageFormat::Png,
        )
        .unwrap();
        let drawn = render_artwork(&source).unwrap();
        let decoded = image::load_from_memory(&drawn).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (512, 512));
        let mut with_memory = snapshot();
        with_memory.memory.level = MemoryLevel::Core;
        with_memory.memory.entries = vec![MemoryEntry {
            slug: "core".into(),
            body: "secret-ish".into(),
        }];
        let png = encode(&with_memory, Format::Png, Some(&drawn)).unwrap();
        assert_eq!(parse_agent_snapshot(&png, false).unwrap(), with_memory);
        let avatar = artwork(&png).unwrap().unwrap();
        assert!(manifest(&avatar).is_err(), "the avatar loses the manifest");
        assert_eq!(image::load_from_memory(&avatar).unwrap().width(), 512);
    }

    #[test]
    fn malformed_pngs_are_refused() {
        let mut png = encode(&snapshot(), Format::Png, None).unwrap();
        let last = png.len() - 1;
        png[last] ^= 1;
        assert_eq!(
            parse_agent_snapshot(&png, false).unwrap_err(),
            "Invalid PNG snapshot."
        );
        let plain = placeholder().concat();
        let mut no_text = MAGIC.to_vec();
        no_text.extend_from_slice(&plain);
        assert_eq!(
            parse_agent_snapshot(&no_text, false).unwrap_err(),
            "PNG does not contain a buzz_agent_snapshot tEXt chunk."
        );
    }
}
