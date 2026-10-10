//! Moving snapshot files between people: the file policy shared by direct
//! messages and links, reading a received file, the copied-link clipboard
//! payload, and the journal entry that lets an unfinished send resume
//! (buzz-app `snapshot-file.ts`, `snapshot-preview.ts`,
//! `snapshot-link.ts`, `snapshot-clipboard.ts`, `snapshot-recovery.ts`).

use super::Kind;
use super::manifest::{MAX_JSON_BYTES, MAX_PNG_BYTES, MemoryLevel};
use buzz_relay_client::outbox::Operation;
use buzz_relay_client::{Attachment, Delivery, Media, RelayOrigin};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Team snapshot caps (`team-encoding.ts`).
pub const MAX_TEAM_JSON_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TEAM_PNG_BYTES: usize = 12 * 1024 * 1024;

/// An encoded snapshot ready to upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedSnapshot {
    pub bytes: Vec<u8>,
    pub file_name: String,
}

impl EncodedSnapshot {
    pub fn mime(&self) -> &'static str {
        if self.file_name.ends_with(".png") {
            "image/png"
        } else {
            "application/json"
        }
    }
}

/// What a file name says it is. Names select a preview only.
pub fn attachment_kind(name: &str) -> Option<Kind> {
    if name.ends_with(".agent.png") || name.ends_with(".agent.json") {
        Some(Kind::Agent)
    } else if name.ends_with(".team.png") || name.ends_with(".team.json") {
        Some(Kind::Team)
    } else {
        None
    }
}

/// The importer's cap for a file of this name, not the sender's claim.
pub fn file_limit(name: &str) -> usize {
    let json = name.ends_with(".json");
    match (name.contains(".team."), json) {
        (true, true) => MAX_TEAM_JSON_BYTES,
        (true, false) => MAX_TEAM_PNG_BYTES,
        (false, true) => MAX_JSON_BYTES,
        (false, false) => MAX_PNG_BYTES,
    }
}

/// `validateSnapshotFile`: the size and type a snapshot upload may have.
pub fn validate_file(file: &EncodedSnapshot) -> Result<(), String> {
    if file.bytes.is_empty() || file.bytes.len() > file_limit(&file.file_name) {
        return Err(buzz_relay_client::upload::UploadError::Size.to_string());
    }
    if !(file.file_name.ends_with(".png") || file.file_name.ends_with(".json")) {
        return Err(buzz_relay_client::upload::UploadError::Rejected.to_string());
    }
    Ok(())
}

static MEDIA_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://[^/]+/media/[0-9a-f]{64}(?:\.(?:png|json))?$").expect("media link")
});

/// Whether a link may offer "Preview snapshot": authenticated media of this
/// session's community (`canPreviewSnapshotLink`).
pub fn can_preview_link(media: &Media, url: &str) -> bool {
    MEDIA_LINK.is_match(url) && media.is_relay_media(url)
}

/// The SHA-256 a media URL names: `/media/<hash>[.ext]`, no query,
/// fragment or credentials.
pub fn media_hash(url: &str) -> Result<String, String> {
    static PATH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^/media/([0-9a-f]{64})(?:\.[a-z0-9]{1,8})?$").expect("media path")
    });
    const INVALID: &str = "Invalid snapshot media URL.";
    let parsed = url::Url::parse(url).map_err(|_| INVALID)?;
    if parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(INVALID.into());
    }
    PATH.captures(parsed.path())
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
        .ok_or_else(|| INVALID.into())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Reads a received snapshot through the community's authenticated media
/// and checks it against the hash its link names
/// (`readSnapshotAttachment`).
pub async fn read_attachment(
    media: &Media,
    url: &str,
    name: Option<&str>,
) -> Result<Vec<u8>, String> {
    if !media.is_relay_media(url) {
        return Err("Snapshot media is unavailable.".into());
    }
    let hash = media_hash(url)?;
    let limit = file_limit(name.unwrap_or_default());
    let bytes = media
        .fetch(url)
        .await
        .map_err(|_| "Couldn’t load snapshot. Check your connection and try again.".to_string())?;
    if bytes.len() > limit {
        return Err(super::manifest::TOO_LARGE.into());
    }
    if sha256_hex(&bytes) != hash {
        return Err("Snapshot checksum does not match the media link.".into());
    }
    Ok(bytes.to_vec())
}

/// The clipboard payload of a copied link: the visible link is the name,
/// the plain text is the media URL (`snapshotClipboardHtml`).
pub fn clipboard_html(attachment: &Attachment, display_name: &str) -> String {
    #[derive(Serialize)]
    struct Payload<'a> {
        version: u32,
        #[serde(rename = "displayName")]
        display_name: &'a str,
        filename: &'a str,
        sha256: &'a str,
        size: u64,
        #[serde(rename = "type")]
        mime: &'a str,
        url: &'a str,
    }
    let payload = serde_json::to_string(&Payload {
        version: 1,
        display_name,
        filename: &attachment.name,
        sha256: &attachment.sha256,
        size: attachment.size,
        mime: &attachment.mime,
        url: &attachment.url,
    })
    .unwrap_or_default();
    let escape = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<a data-buzz-agent-snapshot=\"{}\" href=\"{}\">{}</a>",
        encode_uri_component(&payload),
        escape(&attachment.url),
        escape(display_name)
    )
}

/// `encodeURIComponent`.
fn encode_uri_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The attachment a pasted snapshot link stands for. Clipboard metadata is
/// untrusted: only this session's authenticated media qualifies
/// (`parseSnapshotClipboard`).
pub fn parse_clipboard(html: &str, media: &Media, origin: &RelayOrigin) -> Option<Attachment> {
    static ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)\bdata-buzz-agent-snapshot=(?:"([^"]+)"|'([^']+)')"#)
            .expect("snapshot attribute")
    });
    static NAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\.(agent|team)\.(png|json)$").expect("snapshot name"));
    #[derive(Deserialize)]
    struct Payload {
        version: u32,
        #[serde(rename = "displayName")]
        display_name: String,
        filename: String,
        sha256: Option<String>,
        size: u64,
        #[serde(rename = "type")]
        mime: String,
        url: String,
    }
    if html.len() > 16_384 {
        return None;
    }
    let captures = ATTRIBUTE.captures(html)?;
    let encoded = captures.get(1).or(captures.get(2))?.as_str();
    let value: Payload = serde_json::from_str(&decode_uri_component(encoded)?).ok()?;
    let name = &value.filename;
    let png = name.ends_with(".png");
    if value.version != 1
        || value.display_name.trim().is_empty()
        || value.display_name.chars().count() > 200
        || name.chars().count() > 255
        || name.contains(['/', '\\'])
        || name.chars().any(|c| (c as u32) < 32)
        || !NAME.is_match(name)
        || !can_preview_link(media, &value.url)
        || value.size < 1
        || value.size as usize > file_limit(name)
        || value.mime != if png { "image/png" } else { "application/json" }
    {
        return None;
    }
    let hash = media_hash(&value.url).ok()?;
    if value.sha256.as_ref().is_some_and(|sha| *sha != hash) {
        return None;
    }
    let attachment = Attachment {
        name: value.filename,
        url: value.url,
        mime: value.mime,
        size: value.size,
        sha256: hash,
        voice_duration: None,
    };
    buzz_relay_client::messages::attachment_message("", std::slice::from_ref(&attachment), origin)
        .ok()?;
    Some(attachment)
}

/// A person a snapshot is sent to, as the recovery journal keeps them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub pubkey: String,
    pub name: String,
    #[serde(
        default,
        rename = "isAgent",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub is_agent: bool,
}

pub fn recovery_key(kind: Kind, source_id: &str) -> Result<String, String> {
    if source_id.is_empty() || source_id.encode_utf16().count() > 129 {
        return Err("Sharing requires a stable source identity.".into());
    }
    Ok(format!("snapshot-share:{}:{source_id}", kind.name()))
}

/// The intent a send journals with its message: who and what level. The
/// encoded file and any memory in it never enter the journal.
pub fn recovery_value(people: &[Recipient], level: MemoryLevel) -> String {
    #[derive(Serialize)]
    struct Value<'a> {
        people: Vec<Recipient>,
        level: &'a MemoryLevel,
    }
    serde_json::to_string(&Value {
        people: people
            .iter()
            .map(|p| Recipient {
                name: p.name.chars().take(500).collect(),
                ..p.clone()
            })
            .collect(),
        level: &level,
    })
    .unwrap_or_default()
}

/// A send journaled under `key`, waiting to be confirmed.
#[derive(Clone, Debug, PartialEq)]
pub struct Recovered {
    pub event_id: buzz_relay_client::EventId,
    pub channel: String,
    pub people: Vec<Recipient>,
    pub level: MemoryLevel,
    pub delivery: Delivery,
}

/// The unfinished send under `key` among the outbox's pending operations
/// (`recoverSnapshot`).
pub fn recover(
    operations: &[std::sync::Arc<Operation>],
    key: &str,
) -> Result<Option<Recovered>, String> {
    let matches: Vec<&Operation> = operations
        .iter()
        .map(|op| op.as_ref())
        .filter(|op| op.recovery.as_ref().is_some_and(|r| r.key == key))
        .collect();
    if matches.len() > 1 {
        return Err("Multiple sharing operations need reconciliation.".into());
    }
    let Some(op) = matches.first() else {
        return Ok(None);
    };
    const BROKEN: &str = "The saved sharing operation could not be restored.";
    #[derive(Deserialize)]
    struct Saved {
        people: Vec<Recipient>,
        level: MemoryLevel,
    }
    let saved: Saved = op
        .recovery
        .as_ref()
        .and_then(|r| serde_json::from_str(&r.value).ok())
        .ok_or(BROKEN)?;
    static KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{64}$").expect("key"));
    let unique: std::collections::HashSet<&str> =
        saved.people.iter().map(|p| p.pubkey.as_str()).collect();
    let channel = op.channel().map(str::to_owned);
    match channel {
        Some(channel)
            if op.kind() == 9
                && !saved.people.is_empty()
                && saved.people.len() <= 8
                && saved
                    .people
                    .iter()
                    .all(|p| KEY.is_match(&p.pubkey) && p.name.chars().count() <= 500)
                && unique.len() == saved.people.len() =>
        {
            Ok(Some(Recovered {
                event_id: op.id(),
                channel,
                people: saved.people,
                level: saved.level,
                delivery: op.delivery,
            }))
        }
        _ => Err(BROKEN.into()),
    }
}

/// "Ann", "Ann and Bo", "Ann, Bo, and Cy".
pub fn audience(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_limits() {
        assert_eq!(attachment_kind("Scout.agent.png"), Some(Kind::Agent));
        assert_eq!(attachment_kind("Ops.team.json"), Some(Kind::Team));
        assert_eq!(attachment_kind("photo.png"), None);
        assert_eq!(file_limit("a.agent.json"), MAX_JSON_BYTES);
        assert_eq!(file_limit("a.team.png"), MAX_TEAM_PNG_BYTES);
        assert_eq!(
            audience(&["A".into(), "B".into(), "C".into()]),
            "A, B, and C"
        );
        assert_eq!(audience(&["A".into(), "B".into()]), "A and B");
        assert!(
            validate_file(&EncodedSnapshot {
                bytes: Vec::new(),
                file_name: "a.agent.png".into()
            })
            .is_err()
        );
    }

    #[test]
    fn media_hashes_come_from_clean_urls() {
        let hash = "ab".repeat(32);
        assert_eq!(
            media_hash(&format!("https://r.example/media/{hash}.png")).unwrap(),
            hash
        );
        assert!(media_hash(&format!("https://r.example/media/{hash}.png?x=1")).is_err());
        assert!(media_hash(&format!("https://u:p@r.example/media/{hash}")).is_err());
        assert!(media_hash("https://r.example/other").is_err());
    }

    #[test]
    fn clipboard_payloads_round_trip_for_this_community_only() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let origin = RelayOrigin::parse("wss://relay.example").unwrap();
        let identity = std::sync::Arc::new(buzz_identity::Identity::ephemeral());
        let media = Media::new(runtime.handle().clone(), origin.clone(), identity.clone());
        let bytes = b"{}".to_vec();
        let hash = sha256_hex(&bytes);
        let attachment = Attachment {
            name: "Scout & co.agent.json".into(),
            url: format!("https://relay.example/media/{hash}.json"),
            mime: "application/json".into(),
            size: 2,
            sha256: hash.clone(),
            voice_duration: None,
        };
        let html = clipboard_html(&attachment, "Scout <3");
        assert!(html.ends_with(">Scout &lt;3</a>"));
        assert_eq!(
            parse_clipboard(&html, &media, &origin),
            Some(attachment.clone())
        );
        let other = RelayOrigin::parse("wss://other.example").unwrap();
        let elsewhere = Media::new(runtime.handle().clone(), other.clone(), identity);
        assert_eq!(parse_clipboard(&html, &elsewhere, &other), None);
        assert_eq!(parse_clipboard("<a href=x>y</a>", &media, &origin), None);
    }

    #[test]
    fn recovery_keeps_intent_only() {
        let people = vec![Recipient {
            pubkey: "cd".repeat(32),
            name: "Bob".into(),
            is_agent: false,
        }];
        let value = recovery_value(&people, MemoryLevel::Core);
        assert_eq!(
            value,
            format!(
                r#"{{"people":[{{"pubkey":"{}","name":"Bob"}}],"level":"core"}}"#,
                "cd".repeat(32)
            )
        );
        assert_eq!(
            recovery_key(Kind::Agent, "a1").unwrap(),
            "snapshot-share:agent:a1"
        );
        assert!(recovery_key(Kind::Team, "").is_err());
    }
}
