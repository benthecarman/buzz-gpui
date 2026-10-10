//! The portable `buzz-agent-snapshot` v1 manifest (buzz-app
//! `features/agents/snapshot.ts`). No saved identity or local execution
//! state crosses this boundary: export copies an explicit allowlist, and
//! import fails closed on unknown fields, so a future writer cannot smuggle
//! credentials in as "config".

use buzz_host::api::{Agent, AgentEdit, Harness, HarnessEdit, HarnessKind, SessionPolicy};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

pub const MAX_JSON_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_PNG_BYTES: usize = 10 * 1024 * 1024;
/// Team members arrive inside a validated team envelope.
const MAX_TEAM_MEMBER_BYTES: usize = 8 * 1024 * 1024;
/// NIP-44 v2 switches to an extended length prefix at 65,536 plaintext
/// bytes, past the native reader's ciphertext cap.
const MAX_MEMORY_PLAINTEXT_BYTES: usize = 65_535;
const MIB: usize = 1024 * 1024;

const LEGACY_SUFFIXES: [&str; 4] = [".persona.md", ".persona.json", ".persona.png", ".zip"];
const LEGACY_MESSAGE: &str = "This agent file is from old Buzz and can't be imported directly. If old Buzz is installed on this computer, find that agent on the Agents page under Available to import and click Import.";

pub const INVALID_JSON: &str = "Invalid snapshot JSON.";
pub const INVALID_MANIFEST: &str = "Invalid snapshot manifest.";
pub const TOO_LARGE: &str = "Snapshot exceeds the size limit.";

/// Old Buzz persona files and archives are refused by name, with advice.
pub fn legacy_agent_file_error(file_name: &str) -> Option<&'static str> {
    let lower = file_name.to_lowercase();
    LEGACY_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
        .then_some(LEGACY_MESSAGE)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryLevel {
    #[default]
    None,
    Core,
    Everything,
}

impl MemoryLevel {
    pub fn includes(self, slug: &str) -> bool {
        match self {
            Self::None => false,
            Self::Core => slug == "core",
            Self::Everything => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshot {
    pub format: String,
    pub version: u32,
    pub definition: Definition,
    pub profile: SnapshotProfile,
    pub memory: SnapshotMemory,
}

/// Field order follows buzz-app's `buildAgentSnapshot`, so an export is
/// byte-identical to the reference's.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Definition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_is_builtin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub respond_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub respond_to_allowlist: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_pool: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turn_duration_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotProfile {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_data_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SnapshotMemory {
    pub level: MemoryLevel,
    pub entries: Vec<MemoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoryEntry {
    pub slug: String,
    pub body: String,
}

impl AgentSnapshot {
    /// The manifest as buzz-app's `JSON.stringify(snapshot, null, 2)`
    /// shows it in a review.
    pub fn pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// The file's JSON: memory without entries leaves the key out.
    pub fn file_json(&self) -> Vec<u8> {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if self.memory.entries.is_empty()
            && let Some(memory) = value.get_mut("memory").and_then(Value::as_object_mut)
        {
            memory.remove("entries");
        }
        serde_json::to_vec_pretty(&value).unwrap_or_default()
    }
}

fn utf8_len(value: &str) -> usize {
    value.len()
}

/// `text`: a string of at most `max` UTF-8 bytes without NUL.
fn text(value: Option<&Value>, max: usize) -> bool {
    matches!(value, Some(Value::String(s)) if utf8_len(s) <= max && !s.contains('\0'))
}

fn optional_text(value: Option<&Value>, max: usize) -> bool {
    value.is_none() || text(value, max)
}

fn string(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

/// `Number.isSafeInteger`: integral and within ±(2^53 − 1).
fn safe_integer(value: &Value) -> Option<i64> {
    const MAX: i64 = (1 << 53) - 1;
    if let Some(n) = value.as_i64() {
        return (-MAX..=MAX).contains(&n).then_some(n);
    }
    if value.as_u64().is_some() {
        return None;
    }
    let float = value.as_f64()?;
    (float.fract() == 0.0 && float.abs() <= MAX as f64).then_some(float as i64)
}

fn only_keys(record: &Map<String, Value>, allowed: &[&str]) -> bool {
    record.keys().all(|key| allowed.contains(&key.as_str()))
}

static PICTOGRAPHIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Extended_Pictographic}$").expect("pictographic pattern"));
static SCRIPT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{L}\p{M}]$").expect("script pattern"));
static SPACE_OR_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\s|\p{P})$").expect("punctuation pattern"));
static EFFORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]{1,64}$").expect("effort pattern"));
static AVATAR_DATA_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^data:image/(png|jpeg|gif|webp);base64,[a-zA-Z0-9+/]+=*$")
        .expect("avatar data pattern")
});
static HTTPS_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^https://[^\s@?#]+$").expect("https pattern"));
static CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)-----BEGIN (?:[A-Z ]* )?PRIVATE KEY-----|(?-u:\b)(?:sk-[A-Za-z0-9_-]{16,}|(?:api[_-]?key|access[_-]?token|secret|password)\s*[:=]\s*[^\s,;]{8,})",
    )
    .expect("credential pattern")
});

fn pictographic(c: Option<&char>) -> bool {
    c.is_some_and(|c| PICTOGRAPHIC.is_match(c.encode_utf8(&mut [0; 4])))
}

fn script(c: Option<&char>) -> bool {
    c.is_some_and(|c| SCRIPT.is_match(c.encode_utf8(&mut [0; 4])))
}

/// Default-ignorable and format characters a reviewer cannot see.
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0xad | 0x34f | 0x61c | 0x115f..=0x1160 | 0x17b4..=0x17b5 | 0x180b..=0x180f
        | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x206f | 0x3164 | 0xfe00..=0xfe0f
        | 0xfeff | 0xffa0 | 0xfff0..=0xfff8 | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a
        | 0xe0000..=0xe0fff)
}

/// Reviewable text (`visibleSnapshotText`): no control or invisible
/// characters, except script and emoji joiners in context and, in prompts,
/// ordinary line endings and tabs.
pub fn visible_snapshot_text(value: &str, prompt: bool) -> bool {
    let chars: Vec<char> = value.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let previous = i.checked_sub(1).and_then(|p| chars.get(p));
        let next = chars.get(i + 1);
        if c.is_control() && !(prompt && matches!(c, '\n' | '\r' | '\t')) {
            return false;
        }
        if !invisible(c) {
            continue;
        }
        if matches!(c, '\u{fe0f}' | '\u{fe0e}')
            && (previous.is_some_and(|p| matches!(p, '#' | '*' | '0'..='9'))
                || pictographic(previous))
        {
            continue;
        }
        if matches!(c, '\u{200c}' | '\u{200d}')
            && previous.is_some_and(|p| *p as u32 > 127)
            && next.is_some_and(|n| *n as u32 > 127)
            && script(previous)
            && script(next)
        {
            continue;
        }
        // Malayalam legacy chillu: consonant + virama + ZWJ at a word end.
        if c == '\u{200d}'
            && i >= 2
            && matches!(chars[i - 2] as u32, 0x0d15..=0x0d39)
            && chars[i - 1] == '\u{0d4d}'
            && next.is_none_or(|n| SPACE_OR_PUNCTUATION.is_match(n.encode_utf8(&mut [0; 4])))
        {
            continue;
        }
        if c == '\u{200d}' && pictographic(next) {
            let base = chars[..i].iter().rev().find(|p| {
                !matches!(p, '\u{fe0f}' | '\u{fe0e}') && !matches!(**p as u32, 0x1f3fb..=0x1f3ff)
            });
            if pictographic(base) {
                continue;
            }
        }
        return false;
    }
    true
}

/// A heuristic only; the export UI also discloses residual free-text risk.
pub fn credential_like(value: &str) -> bool {
    CREDENTIAL.is_match(value)
}

/// NIP-AE memory address grammar (`memorySlug`).
pub fn memory_slug(slug: &str) -> bool {
    buzz_desktop::relay::agent::slug(slug)
}

/// Whether a memory entry fits one NIP-44 v2 envelope as the agent writes
/// it (`restorableMemoryEntry`).
pub fn restorable_memory_entry(slug: &str, body: &str) -> bool {
    let plaintext = if slug == "core" {
        serde_json::json!({ "slug": slug, "profile": body })
    } else {
        serde_json::json!({ "slug": slug, "value": body })
    };
    plaintext.to_string().len() <= MAX_MEMORY_PLAINTEXT_BYTES
}

/// The native reader counts each retained DTO with its event ID and the
/// longest timestamp.
fn reader_entry_bytes(slug: &str, body: &str) -> usize {
    serde_json::json!({ "slug": slug, "body": body, "eventId": "0".repeat(64), "createdAt": 0 })
        .to_string()
        .len()
        + 19
}

fn team_avatar_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
    })
}

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&super::png::MAGIC)
}

/// Serde reads `null` as an absent optional; so does the reference.
fn without_null(source: &Value, optional: &[&str]) -> Value {
    let Some(record) = source.as_object() else {
        return source.clone();
    };
    let mut normalized = record.clone();
    for field in optional {
        if normalized.get(*field).is_some_and(Value::is_null) {
            normalized.remove(*field);
        }
    }
    Value::Object(normalized)
}

/// Validates and decodes a snapshot file, JSON or PNG (`parseAgentSnapshot`).
pub fn parse_agent_snapshot(bytes: &[u8], team_member: bool) -> Result<AgentSnapshot, String> {
    let png = is_png(bytes);
    if team_member && png {
        return Err("Team members must be decoded from a validated team envelope.".into());
    }
    let limit = if team_member {
        MAX_TEAM_MEMBER_BYTES
    } else if png {
        MAX_PNG_BYTES
    } else {
        MAX_JSON_BYTES
    };
    if bytes.len() > limit {
        return Err(TOO_LARGE.into());
    }
    let raw = if png {
        super::png::manifest(bytes)?
    } else {
        bytes.to_vec()
    };
    let text = std::str::from_utf8(&raw).map_err(|_| INVALID_JSON)?;
    // TextDecoder drops a leading byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value: Value = serde_json::from_str(text).map_err(|_| INVALID_JSON)?;
    let root = value.as_object().ok_or(INVALID_JSON)?;
    if root.get("format").and_then(Value::as_str) != Some("buzz-agent-snapshot") {
        return Err("Unsupported snapshot format.".into());
    }
    if root.get("version").and_then(safe_integer) != Some(1) {
        return Err("Unsupported snapshot version.".into());
    }
    validate(root, team_member)
        .then(|| decode(root))
        .flatten()
        .ok_or_else(|| INVALID_MANIFEST.into())
}

fn validate(root: &Map<String, Value>, team: bool) -> bool {
    let null = Value::Null;
    let d = without_null(
        root.get("definition").unwrap_or(&null),
        &[
            "systemPrompt",
            "runtime",
            "model",
            "provider",
            "parallelism",
            "respondTo",
            "idleTimeoutSeconds",
            "maxTurnDurationSeconds",
            "effort",
        ],
    );
    let p = without_null(
        root.get("profile").unwrap_or(&null),
        &["about", "avatarDataUrl", "avatarUrl"],
    );
    let (Some(d), Some(p), Some(m)) = (
        d.as_object(),
        p.as_object(),
        root.get("memory").and_then(Value::as_object),
    ) else {
        return false;
    };
    if !only_keys(
        root,
        &["format", "version", "definition", "profile", "memory"],
    ) || !only_keys(
        d,
        &[
            "name",
            "sourceIsBuiltin",
            "systemPrompt",
            "runtime",
            "model",
            "provider",
            "parallelism",
            "sessionPolicy",
            "respondTo",
            "respondToAllowlist",
            "namePool",
            "idleTimeoutSeconds",
            "maxTurnDurationSeconds",
            "effort",
        ],
    ) || !only_keys(p, &["displayName", "about", "avatarDataUrl", "avatarUrl"])
        || !only_keys(m, &["level", "entries"])
    {
        return false;
    }
    let named = |value: Option<&Value>| {
        text(value, 256) && string(value).is_some_and(|s| !s.trim().is_empty())
    };
    if !named(d.get("name")) || !named(p.get("displayName")) {
        return false;
    }
    if d.get("sourceIsBuiltin").is_some_and(|v| !v.is_boolean()) {
        return false;
    }
    let prompt = d.get("systemPrompt");
    if !optional_text(prompt, if team { 128 } else { 64 } * 1024)
        || string(prompt).is_some_and(|s| !visible_snapshot_text(s, true))
        || !visible_snapshot_text(string(d.get("name")).unwrap_or_default(), false)
        || !visible_snapshot_text(string(p.get("displayName")).unwrap_or_default(), false)
    {
        return false;
    }
    for (field, max) in [("runtime", 128), ("model", 512), ("provider", 128)] {
        let value = d.get(field);
        if !optional_text(value, max)
            || string(value).is_some_and(|s| !visible_snapshot_text(s, false))
        {
            return false;
        }
    }
    if d.get("effort")
        .is_some_and(|v| !v.as_str().is_some_and(|s| EFFORT.is_match(s)))
    {
        return false;
    }
    let one_of = |value: Option<&Value>, allowed: &[&str]| match value {
        None => true,
        Some(Value::String(s)) => allowed.contains(&s.as_str()),
        Some(_) => false,
    };
    if !one_of(d.get("sessionPolicy"), &["channel", "thread"])
        || !one_of(d.get("respondTo"), &["owner-only", "allowlist", "anyone"])
    {
        return false;
    }
    static HEX_KEY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[0-9a-f]{64}$").expect("key pattern"));
    if let Some(list) = d.get("respondToAllowlist") {
        let Some(list) = list.as_array() else {
            return false;
        };
        if list.len() > if team { 2000 } else { 128 }
            || !list.iter().all(|v| {
                if team {
                    v.as_str().is_some_and(|s| HEX_KEY.is_match(s))
                } else {
                    text(Some(v), 256)
                }
            })
        {
            return false;
        }
    }
    if let Some(pool) = d.get("namePool") {
        let Some(pool) = pool.as_array() else {
            return false;
        };
        if pool.len() > if team { 256 } else { 128 }
            || !pool
                .iter()
                .all(|v| text(Some(v), 256) && (!team || v.as_str().is_some_and(|s| !s.is_empty())))
        {
            return false;
        }
    }
    if d.get("parallelism")
        .is_some_and(|v| !safe_integer(v).is_some_and(|n| (1..=0xffff_ffff).contains(&n)))
    {
        return false;
    }
    for field in ["idleTimeoutSeconds", "maxTurnDurationSeconds"] {
        if d.get(field)
            .is_some_and(|v| !safe_integer(v).is_some_and(|n| n >= 0))
        {
            return false;
        }
    }
    let about = p.get("about");
    if !optional_text(about, if team { 8 * MIB } else { 2048 })
        || string(about).is_some_and(|s| !visible_snapshot_text(s, true))
    {
        return false;
    }
    if p.get("avatarDataUrl").is_some_and(|v| {
        !v.as_str()
            .is_some_and(|s| AVATAR_DATA_URL.is_match(s) && s.len() <= 2_800_000)
    }) {
        return false;
    }
    if p.get("avatarUrl").is_some_and(|v| {
        !v.as_str().is_some_and(|s| {
            (if team {
                team_avatar_url(s)
            } else {
                HTTPS_URL.is_match(s)
            }) && s.len() <= 2048
        })
    }) {
        return false;
    }
    let level = m.get("level").and_then(Value::as_str);
    if !matches!(level, Some("none" | "core" | "everything")) {
        return false;
    }
    let Some(entries) = m.get("entries") else {
        return true;
    };
    let Some(entries) = entries.as_array() else {
        return false;
    };
    if entries.len() > if team { 256 } else { 128 } {
        return false;
    }
    let mut pairs = Vec::new();
    for entry in entries {
        let Some(record) = entry.as_object() else {
            return false;
        };
        let (Some(slug), Some(body)) = (string(record.get("slug")), record.get("body")) else {
            return false;
        };
        let body_ok = if team {
            body.is_string()
        } else {
            text(Some(body), 64 * 1024)
        };
        let Some(body) = body.as_str().filter(|_| body_ok) else {
            return false;
        };
        if !only_keys(record, &["slug", "body"])
            || !memory_slug(slug)
            || !visible_snapshot_text(body, true)
            || !(team || restorable_memory_entry(slug, body))
        {
            return false;
        }
        pairs.push((slug, body));
    }
    let unique: HashSet<&str> = pairs.iter().map(|(slug, _)| *slug).collect();
    let budget = if team {
        pairs.iter().map(|(s, b)| s.len() + b.len()).sum::<usize>()
    } else {
        serde_json::to_string(entries).map_or(usize::MAX, |s| s.len())
    };
    !((level == Some("none") && !pairs.is_empty())
        || (level == Some("core") && pairs.iter().any(|(slug, _)| *slug != "core"))
        || unique.len() != pairs.len()
        || (!team
            && pairs
                .iter()
                .map(|(s, b)| reader_entry_bytes(s, b))
                .sum::<usize>()
                > MIB)
        || budget > MIB)
}

/// Builds the typed snapshot from a validated manifest.
fn decode(root: &Map<String, Value>) -> Option<AgentSnapshot> {
    let d = root.get("definition")?.as_object()?;
    let p = root.get("profile")?.as_object()?;
    let m = root.get("memory")?.as_object()?;
    let text = |record: &Map<String, Value>, key: &str| {
        record.get(key).and_then(Value::as_str).map(str::to_owned)
    };
    let list = |key: &str| {
        d.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
    };
    let number = |key: &str| {
        d.get(key)
            .and_then(safe_integer)
            .and_then(|n| u64::try_from(n).ok())
    };
    let level = match m.get("level")?.as_str()? {
        "core" => MemoryLevel::Core,
        "everything" => MemoryLevel::Everything,
        _ => MemoryLevel::None,
    };
    let entries = m
        .get("entries")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(MemoryEntry {
                        slug: entry.get("slug")?.as_str()?.to_owned(),
                        body: entry.get("body")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(AgentSnapshot {
        format: "buzz-agent-snapshot".into(),
        version: 1,
        definition: Definition {
            name: text(d, "name")?,
            source_is_builtin: d.get("sourceIsBuiltin").and_then(Value::as_bool),
            system_prompt: text(d, "systemPrompt"),
            model: text(d, "model"),
            provider: text(d, "provider"),
            parallelism: number("parallelism"),
            effort: text(d, "effort"),
            runtime: text(d, "runtime"),
            respond_to: text(d, "respondTo"),
            respond_to_allowlist: list("respondToAllowlist"),
            session_policy: text(d, "sessionPolicy"),
            name_pool: list("namePool"),
            idle_timeout_seconds: number("idleTimeoutSeconds"),
            max_turn_duration_seconds: number("maxTurnDurationSeconds"),
        },
        profile: SnapshotProfile {
            display_name: text(p, "displayName")?,
            about: text(p, "about"),
            avatar_data_url: text(p, "avatarDataUrl"),
            avatar_url: text(p, "avatarUrl"),
        },
        memory: SnapshotMemory { level, entries },
    })
}

/// The session policy an agent runs with: its own, else its defaults'.
pub fn effective_session_policy(
    agent: &Agent,
    default: Option<SessionPolicy>,
) -> Option<SessionPolicy> {
    agent.session_policy.or(default)
}

fn policy_name(policy: SessionPolicy) -> &'static str {
    match policy {
        SessionPolicy::Channel => "channel",
        SessionPolicy::Thread => "thread",
    }
}

const UNFAITHFUL: &str =
    "This agent has runtime, response, or environment settings that cannot be exported faithfully.";
const LIMITATION_NAMES: [&str; 4] = [
    "team instructions",
    "idle timeout",
    "turn timeout",
    "behavioral environment overrides",
];

fn present(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| !v.is_empty())
}

/// The portable configuration of `agent` (`buildAgentSnapshot`): an explicit
/// allowlist. Arguments, workspace, environment, imported blobs and the
/// identity never leave.
pub fn build_agent_snapshot(
    agent: &Agent,
    level: MemoryLevel,
    memories: &[MemoryEntry],
    default_session_policy: Option<SessionPolicy>,
) -> Result<AgentSnapshot, String> {
    let session_policy = effective_session_policy(agent, default_session_policy);
    let keys = &agent.harness.environment_keys;
    let workers = keys.iter().any(|key| key == "BUZZ_ACP_AGENTS");
    if agent.harness.command != "buzz-agent"
        || agent.respond_to.as_deref() != Some("owner-only")
        || present(&agent.launch_model_env)
        || present(&agent.launch_provider_env)
        || present(&agent.backend)
        || agent.harness.databricks.is_some()
        || keys.iter().any(|key| key != "BUZZ_ACP_AGENTS")
        || (workers && agent.launch_parallelism.is_none())
        || agent
            .launch_parallelism
            .is_some_and(|n| !(1..=32).contains(&n))
        || session_policy.is_none()
    {
        return Err(UNFAITHFUL.into());
    }
    // Only fixed reason names from the host may reach the user.
    let Some(limitations) = &agent.snapshot_export_limitations else {
        return Err("This agent cannot be exported faithfully on this host.".into());
    };
    if !limitations.is_empty() {
        if limitations
            .iter()
            .any(|name| !LIMITATION_NAMES.contains(&name.as_str()))
        {
            return Err("This agent cannot be exported faithfully.".into());
        }
        return Err(format!(
            "This agent cannot be exported faithfully because of: {}. Remove the listed settings before exporting.",
            limitations.join(", ")
        ));
    }
    let picture = agent.picture.clone().unwrap_or_default();
    for value in [
        agent.name.as_str(),
        agent.system_prompt.as_str(),
        agent.launch_model.as_deref().unwrap_or_default(),
        agent.launch_provider.as_deref().unwrap_or_default(),
        picture.as_str(),
    ] {
        if credential_like(value) {
            return Err(
                "Portable configuration appears to contain a credential. Remove it before export."
                    .into(),
            );
        }
    }
    let entries = memories
        .iter()
        .filter(|entry| level.includes(&entry.slug))
        .cloned()
        .collect();
    let snapshot = AgentSnapshot {
        format: "buzz-agent-snapshot".into(),
        version: 1,
        definition: Definition {
            name: agent.name.clone(),
            system_prompt: Some(agent.system_prompt.clone()),
            model: agent.launch_model.clone().filter(|m| !m.is_empty()),
            provider: agent.launch_provider.clone().filter(|p| !p.is_empty()),
            parallelism: agent.launch_parallelism.map(u64::from),
            effort: agent.launch_effort.clone().filter(|e| !e.is_empty()),
            runtime: Some("buzz-agent".into()),
            respond_to: Some("owner-only".into()),
            session_policy: session_policy.map(|p| policy_name(p).into()),
            ..Default::default()
        },
        profile: SnapshotProfile {
            display_name: agent.name.clone(),
            avatar_url: Some(picture).filter(|p| HTTPS_URL.is_match(p)),
            ..Default::default()
        },
        memory: SnapshotMemory { level, entries },
    };
    let json = serde_json::to_vec(&snapshot).map_err(|_| INVALID_MANIFEST)?;
    parse_agent_snapshot(&json, false)?;
    Ok(snapshot)
}

/// Settings a valid manifest can describe that a created agent cannot keep
/// (`snapshotLimitations`).
pub fn snapshot_limitations(snapshot: &AgentSnapshot, team_member: bool) -> Vec<String> {
    let d = &snapshot.definition;
    let mut out = Vec::new();
    let mut add = |condition: bool, text: &str| {
        if condition {
            out.push(text.to_string());
        }
    };
    add(
        d.name != snapshot.profile.display_name,
        "definition and profile names disagree",
    );
    add(
        d.runtime
            .as_deref()
            .is_some_and(|r| runtime_kind(r).is_none()),
        "runtime (unsupported)",
    );
    add(
        !team_member && d.respond_to.as_deref().is_some_and(|r| r != "owner-only"),
        "response policy",
    );
    add(
        !team_member
            && d.respond_to_allowlist
                .as_ref()
                .is_some_and(|l| !l.is_empty()),
        "source response allowlist",
    );
    // Listener workers accept 1..=32; more cannot be clamped silently.
    add(
        d.parallelism.is_some_and(|n| n > 32),
        "parallelism (native supports 1–32 workers)",
    );
    add(
        !team_member && d.name_pool.as_ref().is_some_and(|p| !p.is_empty()),
        "name pool",
    );
    add(
        !team_member && d.idle_timeout_seconds.is_some(),
        "idle timeout",
    );
    add(
        !team_member && d.max_turn_duration_seconds.is_some(),
        "turn timeout",
    );
    add(
        !team_member
            && snapshot
                .profile
                .about
                .as_deref()
                .is_some_and(|a| !a.is_empty()),
        "profile about",
    );
    out
}

/// The snapshot runtime names and the harnesses they mean.
pub fn runtime_kind(runtime: &str) -> Option<HarnessKind> {
    match runtime {
        "buzz-agent" => Some(HarnessKind::BuzzAgent),
        "goose" => Some(HarnessKind::Goose),
        "pi" => Some(HarnessKind::Pi),
        "claude" => Some(HarnessKind::Claude),
        "hermes" => Some(HarnessKind::Hermes),
        _ => None,
    }
}

/// The create request for a snapshot on a host with `harnesses`
/// (`snapshotImportEdit`): the portable definition is the only authority,
/// and the harness comes from what the host has installed, never from the
/// publisher. `picture` is the avatar already uploaded, if any.
pub fn snapshot_import_edit(
    snapshot: &AgentSnapshot,
    harnesses: &[Harness],
    team_member: bool,
) -> Result<AgentEdit, String> {
    let limitations = snapshot_limitations(snapshot, team_member);
    if !limitations.is_empty() {
        return Err(format!("Import is blocked: {}.", limitations.join("; ")));
    }
    let runtime = snapshot
        .definition
        .runtime
        .clone()
        .unwrap_or_else(|| "buzz-agent".into());
    let kind = runtime_kind(&runtime);
    let command = harnesses
        .iter()
        .find(|harness| Some(harness.kind) == kind)
        .and_then(|harness| harness.command.clone())
        .ok_or_else(|| format!("{runtime} is unavailable. Install it in Settings first."))?;
    let model = snapshot.definition.model.clone().unwrap_or_default();
    let provider = snapshot.definition.provider.clone().unwrap_or_default();
    // External harnesses take no provider; Pi needs a model with one.
    if matches!(kind, Some(HarnessKind::Claude | HarnessKind::Hermes)) && !provider.is_empty() {
        return Err("Import is blocked: external harness provider selector.".into());
    }
    if kind == Some(HarnessKind::Pi) && !provider.is_empty() && model.is_empty() {
        return Err("Import is blocked: Pi provider requires a model.".into());
    }
    let session_policy = match snapshot.definition.session_policy.as_deref() {
        Some("thread") => SessionPolicy::Thread,
        _ => SessionPolicy::Channel,
    };
    let mut environment = BTreeMap::new();
    if !team_member && let Some(workers) = snapshot.definition.parallelism {
        environment.insert("BUZZ_ACP_AGENTS".into(), Some(workers.to_string()));
    }
    let picture = if team_member {
        snapshot
            .profile
            .avatar_data_url
            .clone()
            .or(snapshot.profile.avatar_url.clone())
    } else {
        snapshot.profile.avatar_url.clone()
    };
    Ok(AgentEdit {
        name: snapshot.profile.display_name.clone(),
        picture,
        system_prompt: snapshot
            .definition
            .system_prompt
            .clone()
            .unwrap_or_default(),
        session_policy: Some(Some(session_policy)),
        workspace: String::new(),
        harness: HarnessEdit {
            command,
            args: Vec::new(),
            model,
            provider,
            databricks: None,
        },
        environment,
        effort: snapshot
            .definition
            .effort
            .clone()
            .filter(|e| !e.is_empty())
            .map(Some),
    })
}

/// The file name a snapshot downloads as: `{name}.agent.{png,json}`.
pub fn file_name(name: &str, extension: &str) -> String {
    let mut stem = String::new();
    let mut run = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            stem.push(c);
            run = false;
        } else if !run {
            stem.push('-');
            run = true;
        }
    }
    format!("{stem}.agent.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_host::api::HarnessSettings;

    fn agent() -> Agent {
        Agent {
            id: "a1".into(),
            pubkey: "ab".repeat(32),
            relay_url: "wss://relay.example".into(),
            name: "Scout".into(),
            picture: Some("https://relay.example/media/x.png".into()),
            system_prompt: "Be brief.".into(),
            session_policy: Some(SessionPolicy::Thread),
            harness: HarnessSettings {
                command: "buzz-agent".into(),
                environment_keys: vec!["BUZZ_ACP_AGENTS".into()],
                ..Default::default()
            },
            respond_to: Some("owner-only".into()),
            launch_model: Some("gpt-5".into()),
            launch_provider: Some("openai".into()),
            launch_parallelism: Some(4),
            launch_effort: Some("high".into()),
            snapshot_export_limitations: Some(Vec::new()),
            ..Default::default()
        }
    }

    #[test]
    fn exports_match_the_reference_bytes() {
        let snapshot = build_agent_snapshot(&agent(), MemoryLevel::None, &[], None).unwrap();
        let json = String::from_utf8(snapshot.file_json()).unwrap();
        assert_eq!(
            json,
            r#"{
  "format": "buzz-agent-snapshot",
  "version": 1,
  "definition": {
    "name": "Scout",
    "systemPrompt": "Be brief.",
    "model": "gpt-5",
    "provider": "openai",
    "parallelism": 4,
    "effort": "high",
    "runtime": "buzz-agent",
    "respondTo": "owner-only",
    "sessionPolicy": "thread"
  },
  "profile": {
    "displayName": "Scout",
    "avatarUrl": "https://relay.example/media/x.png"
  },
  "memory": {
    "level": "none"
  }
}"#
        );
        assert_eq!(
            parse_agent_snapshot(json.as_bytes(), false).unwrap(),
            snapshot
        );
        assert!(snapshot.pretty().contains("\"entries\": []"));
    }

    #[test]
    fn export_eligibility_follows_the_reference() {
        let refuse = |change: &dyn Fn(&mut Agent)| {
            let mut agent = agent();
            change(&mut agent);
            build_agent_snapshot(&agent, MemoryLevel::None, &[], None).unwrap_err()
        };
        assert_eq!(refuse(&|a| a.harness.command = "goose".into()), UNFAITHFUL);
        assert_eq!(
            refuse(&|a| a.respond_to = Some("anyone".into())),
            UNFAITHFUL
        );
        assert_eq!(refuse(&|a| a.backend = Some("x".into())), UNFAITHFUL);
        assert_eq!(
            refuse(&|a| a.launch_model_env = Some("GOOSE_MODEL".into())),
            UNFAITHFUL
        );
        assert_eq!(
            refuse(&|a| a.harness.environment_keys.push("OPENAI_API_KEY".into())),
            UNFAITHFUL
        );
        assert_eq!(refuse(&|a| a.launch_parallelism = Some(33)), UNFAITHFUL);
        assert_eq!(refuse(&|a| a.session_policy = None), UNFAITHFUL);
        assert_eq!(
            refuse(&|a| a.snapshot_export_limitations = None),
            "This agent cannot be exported faithfully on this host."
        );
        assert_eq!(
            refuse(&|a| a.snapshot_export_limitations = Some(vec!["idle timeout".into()])),
            "This agent cannot be exported faithfully because of: idle timeout. Remove the listed settings before exporting."
        );
        assert_eq!(
            refuse(&|a| a.snapshot_export_limitations = Some(vec!["secret".into()])),
            "This agent cannot be exported faithfully."
        );
        assert_eq!(
            refuse(&|a| a.system_prompt = "api_key = abcdefgh1234".into()),
            "Portable configuration appears to contain a credential. Remove it before export."
        );
        // The agent defaults supply a missing session policy.
        let mut inherited = agent();
        inherited.session_policy = None;
        let snapshot = build_agent_snapshot(
            &inherited,
            MemoryLevel::None,
            &[],
            Some(SessionPolicy::Channel),
        )
        .unwrap();
        assert_eq!(
            snapshot.definition.session_policy.as_deref(),
            Some("channel")
        );
    }

    #[test]
    fn memory_levels_filter_entries() {
        let memories = [
            MemoryEntry {
                slug: "core".into(),
                body: "I am Scout.".into(),
            },
            MemoryEntry {
                slug: "mem/notes".into(),
                body: "Likes tea.".into(),
            },
        ];
        let core = build_agent_snapshot(&agent(), MemoryLevel::Core, &memories, None).unwrap();
        assert_eq!(core.memory.entries.len(), 1);
        let all = build_agent_snapshot(&agent(), MemoryLevel::Everything, &memories, None).unwrap();
        assert_eq!(all.memory.entries.len(), 2);
        let parsed = parse_agent_snapshot(&all.file_json(), false).unwrap();
        assert_eq!(parsed.memory.entries, memories);
    }

    fn manifest(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let mut value = serde_json::json!({
            "format": "buzz-agent-snapshot",
            "version": 1,
            "definition": { "name": "Scout" },
            "profile": { "displayName": "Scout" },
            "memory": { "level": "none" }
        });
        edit(&mut value);
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn parsing_fails_closed() {
        let parse = |bytes: Vec<u8>| parse_agent_snapshot(&bytes, false);
        assert!(parse(manifest(|_| {})).is_ok());
        assert_eq!(
            parse(manifest(|v| v["format"] = "other".into())).unwrap_err(),
            "Unsupported snapshot format."
        );
        assert_eq!(
            parse(manifest(|v| v["version"] = 2.into())).unwrap_err(),
            "Unsupported snapshot version."
        );
        assert_eq!(parse(b"{".to_vec()).unwrap_err(), INVALID_JSON);
        for bad in [
            manifest(|v| v["extra"] = 1.into()),
            manifest(|v| v["definition"]["workspace"] = "/tmp".into()),
            manifest(|v| v["definition"]["name"] = " ".into()),
            manifest(|v| v["definition"]["name"] = "Sc\u{200b}out".into()),
            manifest(|v| v["definition"]["parallelism"] = 0.into()),
            manifest(|v| v["definition"]["effort"] = "high effort".into()),
            manifest(|v| v["definition"]["sessionPolicy"] = Value::Null),
            manifest(|v| v["profile"]["avatarUrl"] = "http://x/y.png".into()),
            manifest(|v| {
                v["memory"]["entries"] = serde_json::json!([{"slug": "core", "body": "x"}])
            }),
            manifest(|v| {
                v["memory"] = serde_json::json!({"level": "core", "entries": [{"slug": "mem/a", "body": "x"}]})
            }),
            manifest(|v| {
                v["memory"] = serde_json::json!({"level": "everything", "entries": [
                    {"slug": "mem/a", "body": "x"}, {"slug": "mem/a", "body": "y"}]})
            }),
        ] {
            assert_eq!(parse(bad).unwrap_err(), INVALID_MANIFEST);
        }
        // Explicit nulls mean absent for the nullable fields.
        let nulls = parse(manifest(|v| {
            v["definition"]["model"] = Value::Null;
            v["profile"]["about"] = Value::Null;
        }))
        .unwrap();
        assert_eq!(nulls.definition.model, None);
        // Prompts may carry line breaks, and emoji keep their joiners.
        assert!(visible_snapshot_text("Hi\n\tthere 👩‍💻 ❤️", true));
        assert!(!visible_snapshot_text("Hi\nthere", false));
        assert!(!visible_snapshot_text("a\u{202e}b", true));
    }

    #[test]
    fn legacy_files_and_names() {
        assert!(legacy_agent_file_error("Old.Persona.PNG").is_some());
        assert!(legacy_agent_file_error("pack.zip").is_some());
        assert!(legacy_agent_file_error("scout.agent.png").is_none());
        assert_eq!(file_name("My agent: v2!", "png"), "My-agent-v2-.agent.png");
    }

    fn harness(kind: HarnessKind, command: Option<&str>) -> Harness {
        Harness {
            kind,
            label: String::new(),
            command: command.map(Into::into),
            missing: Vec::new(),
            setup_hint: String::new(),
            setup_url: None,
            status: None,
            install_supported: false,
            update_supported: false,
            login_command: None,
        }
    }

    #[test]
    fn imports_map_to_installed_harnesses() {
        let snapshot = build_agent_snapshot(&agent(), MemoryLevel::None, &[], None).unwrap();
        let installed = [harness(HarnessKind::BuzzAgent, Some("buzz-agent"))];
        let edit = snapshot_import_edit(&snapshot, &installed, false).unwrap();
        assert_eq!(edit.harness.command, "buzz-agent");
        assert_eq!(edit.harness.model, "gpt-5");
        assert_eq!(edit.session_policy, Some(Some(SessionPolicy::Thread)));
        assert_eq!(
            edit.environment.get("BUZZ_ACP_AGENTS"),
            Some(&Some("4".into()))
        );
        assert_eq!(edit.effort, Some(Some("high".into())));
        assert_eq!(
            edit.picture.as_deref(),
            Some("https://relay.example/media/x.png")
        );
        let missing = [harness(HarnessKind::BuzzAgent, None)];
        assert_eq!(
            snapshot_import_edit(&snapshot, &missing, false).unwrap_err(),
            "buzz-agent is unavailable. Install it in Settings first."
        );
        let mut blocked = snapshot.clone();
        blocked.definition.respond_to = Some("anyone".into());
        blocked.definition.idle_timeout_seconds = Some(5);
        assert_eq!(
            snapshot_import_edit(&blocked, &installed, false).unwrap_err(),
            "Import is blocked: response policy; idle timeout."
        );
        let mut claude = snapshot;
        claude.definition.runtime = Some("claude".into());
        let installed = [harness(HarnessKind::Claude, Some("/bin/claude-agent-acp"))];
        assert_eq!(
            snapshot_import_edit(&claude, &installed, false).unwrap_err(),
            "Import is blocked: external harness provider selector."
        );
    }
}
