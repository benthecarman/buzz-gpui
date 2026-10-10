//! Team snapshot files (`team-encoding.ts`): plain JSON, or a PNG whose
//! one `buzz_team_snapshot` tEXt chunk carries the JSON in base64. Only
//! the container is checked here; the host validates the snapshot itself
//! (`Request::TeamPreview`).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

pub const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PNG_BYTES: usize = 16 * 1024 * 1024;
const KEYWORD: &[u8] = b"buzz_team_snapshot\0";
const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
/// A transparent 1×1 PNG that carries the snapshot.
const PLACEHOLDER: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR42mNgAAIAAAUAAaX1ZFcAAAAASUVORK5CYII=";
const TOO_LARGE: &str = "Team snapshot exceeds the size limit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Png,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Png => "png",
        }
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
        }
    }
    crc ^ 0xffff_ffff
}

/// One PNG chunk: its type and the whole raw chunk (length, type, data,
/// CRC).
struct Chunk<'a> {
    kind: &'a [u8],
    raw: &'a [u8],
}

impl Chunk<'_> {
    fn data(&self) -> &[u8] {
        &self.raw[8..self.raw.len() - 4]
    }
}

fn chunks(bytes: &[u8]) -> Result<Vec<Chunk<'_>>, String> {
    let invalid = || "Invalid PNG".to_string();
    let mut chunks = Vec::new();
    let mut at = SIGNATURE.len();
    while at < bytes.len() {
        let length = bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(invalid)?;
        let end = at
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .filter(|end| *end <= bytes.len())
            .ok_or_else(invalid)?;
        chunks.push(Chunk {
            kind: &bytes[at + 4..at + 8],
            raw: &bytes[at..end],
        });
        at = end;
    }
    Ok(chunks)
}

/// The file for a snapshot's JSON (`encodeTeam`).
pub fn encode(json: &str, format: Format) -> Result<Vec<u8>, String> {
    if json.len() > MAX_JSON_BYTES {
        return Err(TOO_LARGE.into());
    }
    if format == Format::Json {
        return Ok(json.as_bytes().to_vec());
    }
    let mut payload = KEYWORD.to_vec();
    payload.extend_from_slice(STANDARD.encode(json).as_bytes());
    let mut chunk = Vec::with_capacity(payload.len() + 12);
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(&payload);
    let crc = crc32(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    let image = STANDARD.decode(PLACEHOLDER).expect("placeholder decodes");
    let mut file = SIGNATURE.to_vec();
    for item in chunks(&image)? {
        file.extend_from_slice(item.raw);
        if item.kind == b"IHDR" {
            file.extend_from_slice(&chunk);
        }
    }
    if file.len() > MAX_PNG_BYTES {
        return Err(TOO_LARGE.into());
    }
    Ok(file)
}

/// The snapshot JSON a JSON or PNG team file carries (`decodeTeamFile`).
pub fn decode(bytes: &[u8]) -> Result<String, String> {
    if !bytes.starts_with(&SIGNATURE) {
        if bytes.len() > MAX_JSON_BYTES {
            return Err(TOO_LARGE.into());
        }
        return String::from_utf8(bytes.to_vec()).map_err(|_| "Invalid team snapshot JSON".into());
    }
    if bytes.len() > MAX_PNG_BYTES {
        return Err(TOO_LARGE.into());
    }
    let all = chunks(bytes)?;
    let matches: Vec<_> = all
        .iter()
        .filter(|chunk| chunk.kind == b"tEXt" && chunk.data().starts_with(KEYWORD))
        .collect();
    let [chunk] = matches.as_slice() else {
        return Err("PNG does not contain a buzz_team_snapshot tEXt chunk".into());
    };
    let stored = u32::from_be_bytes(
        chunk.raw[chunk.raw.len() - 4..]
            .try_into()
            .expect("4 bytes"),
    );
    if stored != crc32(&chunk.raw[4..chunk.raw.len() - 4]) {
        return Err("Invalid PNG snapshot checksum".into());
    }
    let encoded = String::from_utf8_lossy(&chunk.data()[KEYWORD.len()..]);
    let encoded = encoded.trim();
    if encoded.len() > MAX_JSON_BYTES.div_ceil(3) * 4 {
        return Err(TOO_LARGE.into());
    }
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| "Invalid PNG snapshot".to_string())?;
    if decoded.len() > MAX_JSON_BYTES {
        return Err(TOO_LARGE.into());
    }
    String::from_utf8(decoded).map_err(|_| "Invalid team snapshot JSON".into())
}

/// The download name (`{team}.team.{fmt}`): characters other than ASCII
/// letters, digits, `_` and `-` become `-`.
pub fn file_name(team: &str, format: Format) -> String {
    let safe: String = team
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{safe}.team.{}", format.extension())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"format":"buzz-team-snapshot","version":1}"#;

    #[test]
    fn json_and_png_round_trip() {
        assert_eq!(decode(&encode(JSON, Format::Json).unwrap()).unwrap(), JSON);
        let png = encode(JSON, Format::Png).unwrap();
        assert!(png.starts_with(&SIGNATURE));
        assert_eq!(decode(&png).unwrap(), JSON);
        // The tEXt chunk follows IHDR, and the image stays a valid PNG.
        let kinds: Vec<_> = chunks(&png)
            .unwrap()
            .iter()
            .map(|c| c.kind.to_vec())
            .collect();
        assert_eq!(kinds[0], b"IHDR");
        assert_eq!(kinds[1], b"tEXt");
        assert_eq!(kinds.last().unwrap(), b"IEND");
    }

    #[test]
    fn a_damaged_png_is_refused() {
        let mut png = encode(JSON, Format::Png).unwrap();
        let at = png.windows(4).position(|w| w == b"tEXt").unwrap() + 30;
        png[at] ^= 1;
        assert_eq!(decode(&png).unwrap_err(), "Invalid PNG snapshot checksum");
        let plain = STANDARD.decode(PLACEHOLDER).unwrap();
        assert_eq!(
            decode(&plain).unwrap_err(),
            "PNG does not contain a buzz_team_snapshot tEXt chunk"
        );
        assert_eq!(
            decode(&SIGNATURE).unwrap_err(),
            "PNG does not contain a buzz_team_snapshot tEXt chunk"
        );
        assert_eq!(
            decode(&[&SIGNATURE[..], &[0, 0]].concat()).unwrap_err(),
            "Invalid PNG"
        );
    }

    #[test]
    fn size_limits_apply_before_decoding() {
        let large = "x".repeat(MAX_JSON_BYTES + 1);
        assert_eq!(encode(&large, Format::Json).unwrap_err(), TOO_LARGE);
        assert_eq!(decode(large.as_bytes()).unwrap_err(), TOO_LARGE);
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Ops crew/2", Format::Png), "Ops-crew-2.team.png");
        assert_eq!(file_name("crew", Format::Json), "crew.team.json");
    }
}
