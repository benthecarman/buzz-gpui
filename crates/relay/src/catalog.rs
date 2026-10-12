//! The NIP-AP community catalog (`catalog-protocol.ts`, `catalog.ts`):
//! shared agent definitions (kind 30175) and team projections (kind
//! 30178), what this viewer publishes there, and the state each of its
//! share switches shows.
//!
//! Everything here is untrusted relay data: a publication with any invalid
//! field is dropped whole, and reading never adopts anything. Adoption is
//! the caller's explicit act. [`crate::session::CommunityCatalog`] drives
//! this model against a session's reader and outbox.

use crate::event::{Verified, is_hex64};
use crate::messages::Draft;
use crate::outbox::{Delivery, Operation};
use crate::reader::{Priority, ReadErrorKind, ReadOptions, Reader};
use crate::socket::filter;
use nostr::prelude::{Event, UnsignedEvent};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

pub const AGENT_CATALOG_KIND: u16 = 30175;
pub const TEAM_CATALOG_KIND: u16 = 30178;
pub const MAX_CONTENT_BYTES: usize = 65_535;
/// Serialized catalog event bound: JSON escaping at most doubles the
/// content, plus the bounded tags and fields (`catalog-envelope.ts`).
pub const MAX_EVENT_BYTES: usize = 2 * MAX_CONTENT_BYTES + 2048;
/// Team-catalog projection version.
pub const TEAM_CATALOG_VERSION: u64 = 1;

const MAX_DISPLAY_NAME_CHARS: usize = 128;
const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 280;
const MAX_TEAM_MEMBERS: usize = 64;
const MAX_TEAM_NAME_BYTES: usize = 256;
const MAX_TEAM_TEXT_BYTES: usize = 4 * 1024;
const MAX_MEMBER_PROMPT_BYTES: usize = 16 * 1024;
const MAX_MEMBER_KEY_BYTES: usize = 128;
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_HTTP_AVATAR_BYTES: usize = 2048;
const STOCK_ACP: &str = "buzz-acp";
/// Small pages avoid the reader's size refusal; one still halves the page.
const PAGE: u64 = ((8 * 1024 * 1024 - 2) / (MAX_EVENT_BYTES + 1)) as u64;
const MAX_EVENTS: usize = 5_000;

pub const READ_FAILED: &str =
    "Could not read the community catalog. Check the community connection, then retry.";
pub const UNCONFIRMED: &str =
    "The relay accepted the update, but the catalog could not confirm it.";

pub fn is_catalog_kind(kind: u16) -> bool {
    kind == AGENT_CATALOG_KIND || kind == TEAM_CATALOG_KIND
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CatalogKind {
    Agent,
    Team,
}

impl CatalogKind {
    pub fn kind(self) -> u16 {
        match self {
            Self::Agent => AGENT_CATALOG_KIND,
            Self::Team => TEAM_CATALOG_KIND,
        }
    }

    pub fn from_kind(kind: u16) -> Option<Self> {
        match kind {
            AGENT_CATALOG_KIND => Some(Self::Agent),
            TEAM_CATALOG_KIND => Some(Self::Team),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionPolicy {
    #[default]
    Channel,
    Thread,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum RespondTo {
    #[serde(rename = "anyone")]
    Anyone,
    #[serde(rename = "owner-only")]
    OwnerOnly,
}

/// The adoptable fields of one shared agent; other content is ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogAgent {
    pub display_name: String,
    pub system_prompt: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub runtime: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Portable ACP transport alias; never a local path.
    pub acp_command: Option<String>,
    pub respond_to: Option<RespondTo>,
    pub session_policy: SessionPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogTeamMember {
    pub member_key: String,
    pub agent: CatalogAgent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPublication {
    pub event_id: String,
    pub owner: String,
    pub d: String,
    pub created_at: u64,
    pub agent: CatalogAgent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamPublication {
    pub event_id: String,
    pub owner: String,
    pub d: String,
    pub created_at: u64,
    pub name: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub members: Vec<CatalogTeamMember>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Publication {
    Agent(AgentPublication),
    Team(TeamPublication),
}

impl Publication {
    pub fn kind(&self) -> CatalogKind {
        match self {
            Self::Agent(_) => CatalogKind::Agent,
            Self::Team(_) => CatalogKind::Team,
        }
    }

    pub fn owner(&self) -> &str {
        match self {
            Self::Agent(p) => &p.owner,
            Self::Team(p) => &p.owner,
        }
    }

    pub fn d(&self) -> &str {
        match self {
            Self::Agent(p) => &p.d,
            Self::Team(p) => &p.d,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Agent(p) => &p.agent.display_name,
            Self::Team(p) => &p.name,
        }
    }

    /// `kind:owner:d`, the key an added copy is remembered under.
    pub fn coordinate(&self) -> String {
        format!("{}:{}:{}", self.kind().kind(), self.owner(), self.d())
    }

    /// The first non-stock ACP transport alias it names. This app runs
    /// only the bundled transport, so such an entry can't be adopted
    /// faithfully; callers refuse it rather than substituting silently.
    pub fn unsupported_transport(&self) -> Option<&str> {
        fn alias(agent: &CatalogAgent) -> Option<&str> {
            agent
                .acp_command
                .as_deref()
                .filter(|command| *command != STOCK_ACP)
        }
        match self {
            Self::Agent(p) => alias(&p.agent),
            Self::Team(p) => p.members.iter().find_map(|m| alias(&m.agent)),
        }
    }
}

pub fn unsupported_transport_message(name: &str, alias: &str) -> String {
    format!(
        "{name} uses the {alias} transport, which this app can't run yet, so it can't be added."
    )
}

// ---------------------------------------------------------------------------
// Text policy

static DEFAULT_IGNORABLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Default_Ignorable_Code_Point}$").expect("valid"));
static PICTOGRAPHIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Extended_Pictographic}$").expect("valid"));
static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Emoji}$").expect("valid"));
static AGENT_SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{0,63}$").expect("valid"));
static PORTABLE_ACP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^buzz-[A-Za-z0-9_-]+-acp$").expect("valid"));

fn is(regex: &Regex, c: Option<char>) -> bool {
    c.is_some_and(|c| regex.is_match(c.encode_utf8(&mut [0; 4])))
}

/// NIP-AP visible-text policy: rejects concealed, bidi and control
/// characters. Emoji ZWJ sequences and VS16 after an emoji stay legal.
pub fn visible_text(value: &str, multiline: bool) -> bool {
    let chars: Vec<char> = value.chars().collect();
    chars.iter().enumerate().all(|(index, &c)| {
        if multiline && (c == '\n' || c == '\t') {
            return true;
        }
        if c.is_control() {
            return false;
        }
        if !is(&DEFAULT_IGNORABLE, Some(c)) {
            return true;
        }
        let previous = index.checked_sub(1).map(|i| chars[i]);
        match c {
            '\u{FE0F}' => is(&EMOJI, previous),
            '\u{200D}' => {
                (is(&PICTOGRAPHIC, previous) || previous == Some('\u{FE0F}'))
                    && is(&PICTOGRAPHIC, chars.get(index + 1).copied())
            }
            _ => false,
        }
    })
}

pub fn portable_acp_command(command: &str) -> bool {
    command == STOCK_ACP || (command.len() <= 255 && PORTABLE_ACP.is_match(command))
}

/// Only HTTPS artwork travels; inline art would exceed the outbox budget.
pub fn catalog_avatar(value: Option<&Value>) -> Option<String> {
    let source = value?.as_str()?.trim();
    if source.len() > MAX_HTTP_AVATAR_BYTES {
        return None;
    }
    let url = url::Url::parse(source).ok()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.as_str().len() <= MAX_HTTP_AVATAR_BYTES)
        .then(|| url.to_string())
}

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
}

fn object(content: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str(content) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn check_definition(name: &str, prompt: &str) -> Result<(), String> {
    let chars = name.chars().count();
    if name.trim().is_empty() {
        return Err("Display name is required".into());
    }
    if chars > MAX_DISPLAY_NAME_CHARS {
        return Err(format!(
            "Display name is too long ({chars} characters, max {MAX_DISPLAY_NAME_CHARS})"
        ));
    }
    if prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        return Err(format!(
            "Agent instructions are too long ({} bytes, max {MAX_SYSTEM_PROMPT_BYTES})",
            prompt.len()
        ));
    }
    if !visible_text(name, false) || !visible_text(prompt, true) {
        return Err("Agent contains prohibited invisible or formatting characters".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Events

fn single<'a>(event: &'a Event, name: &str) -> Option<&'a [String]> {
    let mut found = event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .filter(|tag| tag.first().map(String::as_str) == Some(name));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The event's one nonempty `d` value.
pub fn catalog_d(event: &Event) -> Option<&str> {
    single(event, "d")
        .and_then(|tag| tag.get(1))
        .map(String::as_str)
        .filter(|d| !d.is_empty())
}

/// Exactly one `["shared","true"]` tag.
pub fn is_shared(event: &Event) -> bool {
    shared_tags(event.tags.iter().map(|tag| tag.as_slice()))
}

fn shared_tags<'a>(tags: impl Iterator<Item = &'a [String]>) -> bool {
    let shared: Vec<_> = tags
        .filter(|tag| tag.first().map(String::as_str) == Some("shared"))
        .collect();
    matches!(shared.as_slice(), [[_, value]] if value == "true")
}

/// One head as the catalog compares them: a relay event or a local
/// outbox operation of this viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    pub id: String,
    pub kind: u16,
    pub pubkey: String,
    pub d: String,
    pub created_at: u64,
    pub shared: bool,
    pub content: String,
}

impl Head {
    pub fn from_event(event: &Event) -> Option<Self> {
        let kind = event.kind.as_u16();
        if !is_catalog_kind(kind) {
            return None;
        }
        Some(Self {
            id: event.id.to_hex(),
            kind,
            pubkey: event.pubkey.to_hex(),
            d: catalog_d(event)?.to_owned(),
            created_at: event.created_at.as_secs(),
            shared: is_shared(event),
            content: event.content.clone(),
        })
    }

    pub fn from_unsigned(event: &UnsignedEvent) -> Option<Self> {
        let kind = event.kind.as_u16();
        if !is_catalog_kind(kind) {
            return None;
        }
        let tags: Vec<&[String]> = event.tags.iter().map(|tag| tag.as_slice()).collect();
        let ds: Vec<_> = tags
            .iter()
            .filter(|tag| tag.first().map(String::as_str) == Some("d"))
            .collect();
        let d = match ds.as_slice() {
            [[_, d, ..]] if !d.is_empty() => d.clone(),
            _ => return None,
        };
        Some(Self {
            id: event.id?.to_hex(),
            kind,
            pubkey: event.pubkey.to_hex(),
            d,
            created_at: event.created_at.as_secs(),
            shared: shared_tags(tags.into_iter()),
            content: event.content.clone(),
        })
    }

    pub fn coordinate(&self) -> String {
        format!("{}:{}:{}", self.kind, self.pubkey, self.d)
    }

    /// NIP-33 order: newer `created_at`, then the lower id.
    pub fn outranks(&self, other: &Head) -> bool {
        self.created_at > other.created_at
            || (self.created_at == other.created_at && self.id < other.id)
    }
}

/// NIP-33 heads per `(kind, owner, d)`. The head is claimed before
/// visibility, so an unshared head hides older shared ones.
pub fn heads<'a>(events: impl IntoIterator<Item = &'a Head>) -> HashMap<String, &'a Head> {
    let mut heads: HashMap<String, &Head> = HashMap::new();
    for head in events {
        let key = head.coordinate();
        if heads.get(&key).is_none_or(|known| head.outranks(known)) {
            heads.insert(key, head);
        }
    }
    heads
}

// ---------------------------------------------------------------------------
// Parsing publications

fn parse_agent(body: &Map<String, Value>, member: bool) -> Option<CatalogAgent> {
    let name = body.get("display_name")?.as_str()?;
    let prompt = match body.get("system_prompt") {
        None | Some(Value::Null) => "",
        Some(value) => value.as_str()?,
    };
    check_definition(name, prompt).ok()?;
    if member && prompt.len() > MAX_MEMBER_PROMPT_BYTES {
        return None;
    }
    let description = match body.get("description") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let text = value.as_str()?;
            if text.chars().count() > MAX_DESCRIPTION_CHARS || !visible_text(text, true) {
                return None;
            }
            Some(text)
        }
    };
    let acp = match body.get("acp_command") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_str().filter(|c| portable_acp_command(c))?),
    };
    let mut identifiers: [Option<String>; 3] = Default::default();
    for (slot, key) in identifiers.iter_mut().zip(["runtime", "model", "provider"]) {
        if let Some(value) = text(body.get(key)) {
            if value.len() > MAX_IDENTIFIER_BYTES || !visible_text(value, false) {
                return None;
            }
            *slot = Some(value.to_owned());
        }
    }
    let [runtime, model, provider] = identifiers;
    let respond_to = match body.get("respond_to").and_then(Value::as_str) {
        Some("allowlist" | "owner-only") => Some(RespondTo::OwnerOnly),
        Some("anyone") => Some(RespondTo::Anyone),
        _ => None,
    };
    Some(CatalogAgent {
        display_name: name.to_owned(),
        system_prompt: prompt.to_owned(),
        description: description
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned),
        avatar_url: catalog_avatar(body.get("avatar_url")),
        runtime,
        model,
        provider,
        acp_command: acp.map(str::to_owned),
        respond_to,
        session_policy: if body.get("session_policy").and_then(Value::as_str) == Some("thread") {
            SessionPolicy::Thread
        } else {
            SessionPolicy::Channel
        },
    })
}

fn team_id(d: &str) -> bool {
    d.chars().count() <= 64
        && !d
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\u{FEFF}')
}

/// The untrusted boundary: any invalid field rejects the whole
/// publication, as does a missing `shared` opt-in.
pub fn parse_publication(event: &Event) -> Option<Publication> {
    parse_head(&Head::from_event(event)?)
}

/// [`parse_publication`] of a head, relay-read or local.
pub fn parse_head(head: &Head) -> Option<Publication> {
    let d = head.d.as_str();
    let body = object(&head.content)?;
    if !head.shared {
        return None;
    }
    let (event_id, owner, created_at) = (head.id.clone(), head.pubkey.clone(), head.created_at);
    match head.kind {
        AGENT_CATALOG_KIND => {
            if !AGENT_SLUG.is_match(d) {
                return None;
            }
            Some(Publication::Agent(AgentPublication {
                event_id,
                owner,
                d: d.to_owned(),
                created_at,
                agent: parse_agent(&body, false)?,
            }))
        }
        TEAM_CATALOG_KIND if team_id(d) => {
            if body.get("v").and_then(Value::as_f64) != Some(TEAM_CATALOG_VERSION as f64) {
                return None;
            }
            let name = body.get("name")?.as_str()?;
            let members = body.get("members")?.as_array()?;
            if name.trim().is_empty()
                || name.len() > MAX_TEAM_NAME_BYTES
                || !visible_text(name, false)
                || members.len() > MAX_TEAM_MEMBERS
            {
                return None;
            }
            let mut texts: [Option<String>; 2] = Default::default();
            for (slot, (key, max)) in texts.iter_mut().zip([
                ("description", MAX_TEAM_TEXT_BYTES),
                ("instructions", MAX_MEMBER_PROMPT_BYTES),
            ]) {
                match body.get(key) {
                    None | Some(Value::Null) => {}
                    Some(value) => {
                        let text = value.as_str()?;
                        if text.len() > max || !visible_text(text, true) {
                            return None;
                        }
                        if !text.trim().is_empty() {
                            *slot = Some(text.to_owned());
                        }
                    }
                }
            }
            let mut keys = HashSet::new();
            let mut parsed = Vec::with_capacity(members.len());
            for raw in members {
                let member = raw.as_object()?;
                let key = member.get("member_key")?.as_str()?;
                if key.trim().is_empty() || key.len() > MAX_MEMBER_KEY_BYTES || !keys.insert(key) {
                    return None;
                }
                parsed.push(CatalogTeamMember {
                    member_key: key.to_owned(),
                    agent: parse_agent(member, true)?,
                });
            }
            let [description, instructions] = texts;
            Some(Publication::Team(TeamPublication {
                event_id,
                owner,
                d: d.to_owned(),
                created_at,
                name: name.to_owned(),
                description,
                instructions,
                members: parsed,
            }))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Projection: what this viewer publishes

/// One local agent definition, as the catalog projects it. Only these
/// fields can ever leave the machine.
#[derive(Clone, Debug, Default)]
pub struct PersonaSource {
    pub pubkey: String,
    pub name: String,
    pub system_prompt: String,
    pub picture: Option<String>,
    pub harness_command: String,
    pub model: String,
    pub provider: String,
    pub acp_command: Option<String>,
    /// `anyone`, `owner-only` or `allowlist`.
    pub respond_to: Option<String>,
    /// The agent's own boundary; `None` inherits the agent defaults.
    pub session_policy: Option<SessionPolicy>,
}

/// The portable runtime name of a harness command (`harnessKind`).
pub fn runtime_of(command: &str) -> Option<&'static str> {
    let name = command.replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or_default();
    if name.eq_ignore_ascii_case("buzz-agent.exe") {
        return Some("buzz-agent");
    }
    let bare = ["exe", "cmd", "bat"]
        .iter()
        .find_map(|ext| name.strip_suffix(&format!(".{ext}")))
        .unwrap_or(name);
    match (name, bare) {
        ("buzz-agent", _) => Some("buzz-agent"),
        (_, "goose" | "goose-acp") => Some("goose"),
        ("buzz-pi-acp", _) => Some("pi"),
        (_, "hermes-acp") => Some("hermes"),
        (_, "claude-agent-acp") => Some("claude"),
        _ => None,
    }
}

#[derive(Serialize)]
struct Persona<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    member_key: Option<String>,
    display_name: &'a str,
    system_prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    acp_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
    runtime: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    respond_to: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_policy: Option<SessionPolicy>,
}

fn persona(agent: &PersonaSource, default: Option<SessionPolicy>) -> Result<Persona<'_>, String> {
    let policy = agent.session_policy.or(default).ok_or_else(|| {
        format!(
            "Agent defaults for '{}' are unavailable. Refresh agents and try again.",
            agent.name
        )
    })?;
    // The bundled transport is a local path; only its portable alias travels.
    let acp = agent
        .acp_command
        .as_deref()
        .map(|command| {
            let name = command.rsplit(['/', '\\']).next().unwrap_or_default();
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".exe") {
                name[..name.len() - 4].to_owned()
            } else {
                name.to_owned()
            }
        })
        .unwrap_or_else(|| STOCK_ACP.to_owned());
    let runtime = runtime_of(&agent.harness_command).ok_or_else(|| {
        format!(
            "Agent '{}' uses a runtime that cannot be shared with the community catalog.",
            agent.name
        )
    })?;
    let model = agent.model.trim();
    let provider = agent.provider.trim();
    Ok(Persona {
        member_key: None,
        display_name: &agent.name,
        system_prompt: &agent.system_prompt,
        acp_command: portable_acp_command(&acp).then_some(acp),
        avatar_url: catalog_avatar(
            agent
                .picture
                .as_ref()
                .map(|p| Value::String(p.clone()))
                .as_ref(),
        ),
        runtime,
        model: (!model.is_empty()).then_some(model),
        provider: (!provider.is_empty()).then_some(provider),
        // An allowlist would disclose the owner's trusted keys; downgrade it.
        respond_to: agent.respond_to.as_deref().map(|value| match value {
            "anyone" => "anyone",
            _ => "owner-only",
        }),
        session_policy: Some(policy),
    })
}

fn check_agent(agent: &PersonaSource) -> Result<(), String> {
    check_definition(&agent.name, &agent.system_prompt)
}

/// The kind-30175 content for one local agent.
pub fn agent_catalog_content(
    agent: &PersonaSource,
    default: Option<SessionPolicy>,
) -> Result<String, String> {
    check_agent(agent)?;
    let content = serde_json::to_string(&persona(agent, default)?).expect("serializes");
    if content.len() > MAX_CONTENT_BYTES {
        return Err("Agent definition is too large to share".into());
    }
    Ok(content)
}

/// A saved team as the catalog projects it.
#[derive(Clone, Debug, Default)]
pub struct TeamSource {
    pub id: String,
    pub name: String,
    pub agents: Vec<String>,
    /// From the team's saved portable definition, when it has one.
    pub description: Option<String>,
    pub instructions: Option<String>,
}

/// An opaque per-team member identity; it discloses no local identifier.
pub fn member_key(id: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("buzz:team-catalog:member-key:v1\0{id}"))
    )
}

#[derive(Serialize)]
struct TeamProjection<'a> {
    v: u64,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<&'a str>,
    members: Vec<Persona<'a>>,
}

/// The kind-30178 content for one saved team, projected from this
/// community's local agent definitions.
pub fn team_catalog_content(
    team: &TeamSource,
    members: &[PersonaSource],
    default: Option<SessionPolicy>,
) -> Result<String, String> {
    if team.name.trim().is_empty() {
        return Err("invalid team projection: the team name is empty".into());
    }
    if team.name.len() > MAX_TEAM_NAME_BYTES {
        return Err(format!(
            "team too large to share: the team name is {} bytes (limit {MAX_TEAM_NAME_BYTES})",
            team.name.len()
        ));
    }
    if !visible_text(&team.name, false) {
        return Err("the team name contains prohibited invisible or formatting characters".into());
    }
    if team.agents.len() > MAX_TEAM_MEMBERS {
        return Err(format!(
            "team too large to share: {} members (limit {MAX_TEAM_MEMBERS})",
            team.agents.len()
        ));
    }
    let mut texts: [Option<&str>; 2] = [None, None];
    for (slot, (field, value, max)) in texts.iter_mut().zip([
        ("description", &team.description, MAX_TEAM_TEXT_BYTES),
        ("instructions", &team.instructions, MAX_MEMBER_PROMPT_BYTES),
    ]) {
        let Some(shown) = value.as_deref().filter(|text| !text.trim().is_empty()) else {
            continue;
        };
        if shown.len() > max {
            return Err(format!(
                "team too large to share: the team {field} is {} bytes (limit {max})",
                shown.len()
            ));
        }
        if !visible_text(shown, true) {
            return Err(format!(
                "the team {field} contains prohibited invisible or formatting characters"
            ));
        }
        *slot = Some(shown);
    }
    let mut projected = Vec::with_capacity(team.agents.len());
    for pubkey in &team.agents {
        let agent = members
            .iter()
            .find(|member| member.pubkey == *pubkey)
            .ok_or_else(|| format!("team member {pubkey} not found"))?;
        check_agent(agent)?;
        if agent.system_prompt.len() > MAX_MEMBER_PROMPT_BYTES {
            return Err(format!(
                "team too large to share: the system prompt for '{}' is {} bytes (limit {MAX_MEMBER_PROMPT_BYTES})",
                agent.name,
                agent.system_prompt.len()
            ));
        }
        let mut fields = persona(agent, default)?;
        fields.member_key = Some(member_key(pubkey));
        // The team schema omits the default boundary.
        fields.session_policy = fields
            .session_policy
            .filter(|policy| *policy == SessionPolicy::Thread);
        projected.push(fields);
    }
    let [description, instructions] = texts;
    let content = serde_json::to_string(&TeamProjection {
        v: TEAM_CATALOG_VERSION,
        name: &team.name,
        description,
        instructions,
        members: projected,
    })
    .expect("serializes");
    if content.len() > MAX_CONTENT_BYTES {
        return Err("team too large to share".into());
    }
    Ok(content)
}

/// The agent catalog coordinate of an agent key.
pub fn catalog_slug(pubkey: &str) -> Result<String, String> {
    is_hex64(pubkey)
        .then(|| pubkey.to_owned())
        .ok_or_else(|| "Invalid agent identity".into())
}

/// The owner-to-self envelope the signer admits. Unsharing is a newer
/// head without the `shared` tag, never a deletion.
pub fn catalog_draft(kind: CatalogKind, d: &str, content: String, shared: bool) -> Draft {
    let mut tags = vec![vec!["d".to_owned(), d.to_owned()]];
    if shared {
        tags.push(vec!["shared".to_owned(), "true".to_owned()]);
    }
    Draft {
        kind: kind.kind(),
        content,
        tags,
    }
}

/// A listed team as the v1 team snapshot the shared importer takes.
/// Catalog entries never carry memories, allowlists or secrets.
pub fn catalog_team_snapshot(team: &TeamPublication) -> Value {
    let members: Vec<Value> = team
        .members
        .iter()
        .map(|member| {
            let agent = &member.agent;
            let mut definition = json!({
                "name": agent.display_name,
                "systemPrompt": agent.system_prompt,
                "sessionPolicy": agent.session_policy,
            });
            for (key, value) in [
                ("runtime", &agent.runtime),
                ("model", &agent.model),
                ("provider", &agent.provider),
            ] {
                if let Some(value) = value {
                    definition[key] = json!(value);
                }
            }
            if let Some(respond_to) = agent.respond_to {
                definition["respondTo"] = json!(respond_to);
            }
            let mut profile = json!({ "displayName": agent.display_name });
            if let Some(about) = &agent.description {
                profile["about"] = json!(about);
            }
            if let Some(url) = &agent.avatar_url {
                profile["avatarUrl"] = json!(url);
            }
            json!({
                "format": "buzz-agent-snapshot",
                "version": 1,
                "definition": definition,
                "profile": profile,
                "memory": { "level": "none", "entries": [] },
            })
        })
        .collect();
    let mut meta = json!({ "name": team.name });
    if let Some(text) = &team.description {
        meta["description"] = json!(text);
    }
    if let Some(text) = &team.instructions {
        meta["instructions"] = json!(text);
    }
    json!({
        "format": "buzz-team-snapshot",
        "version": 1,
        "team": meta,
        "members": members,
    })
}

// ---------------------------------------------------------------------------
// Reads

/// Every catalog event readable on the relay, newest first, page by page.
/// More than 5,000 refuses the read rather than showing a partial catalog.
pub async fn read_all(reader: &Reader) -> Result<Vec<Verified>, String> {
    let mut events = Vec::new();
    let mut cursor: Option<(u64, String)> = None;
    let mut limit = PAGE;
    loop {
        let mut query = json!({
            "kinds": [AGENT_CATALOG_KIND, TEAM_CATALOG_KIND],
            "limit": limit,
        });
        if let Some((until, before)) = &cursor {
            query["until"] = json!(until);
            query["before_id"] = json!(before);
        }
        let page = match reader
            .read(
                vec![filter(query)],
                ReadOptions {
                    priority: Priority::Background,
                    fresh: false,
                },
            )
            .await
        {
            Ok(page) => page,
            // A size refusal halves the page; the same cursor is retried.
            Err(error) if error.kind == ReadErrorKind::InvalidResponse && limit > 1 => {
                limit /= 2;
                continue;
            }
            Err(error) => return Err(error.message),
        };
        events.extend(
            page.iter()
                .filter(|event| is_catalog_kind(event.kind_u16()) && catalog_d(event).is_some())
                .cloned(),
        );
        if events.len() > MAX_EVENTS {
            return Err("Community catalog exceeds read budget".into());
        }
        let Some(last) = page.last() else {
            return Ok(events);
        };
        if (page.len() as u64) < limit {
            return Ok(events);
        }
        cursor = Some((last.created_at_secs(), last.id.to_hex()));
    }
}

/// The authoritative head of one coordinate, read fresh and strong.
pub async fn read_head(
    reader: &Reader,
    kind: CatalogKind,
    owner: &str,
    d: &str,
) -> Result<Option<Head>, String> {
    let events = reader
        .read(
            vec![filter(json!({
                "kinds": [kind.kind()],
                "authors": [owner],
                "#d": [d],
                "limit": 20,
                "consistency": "strong",
            }))],
            ReadOptions {
                priority: Priority::Foreground,
                fresh: true,
            },
        )
        .await
        .map_err(|error| error.message)?;
    let candidates: Vec<Head> = events
        .iter()
        .filter_map(|event| Head::from_event(event))
        .filter(|head| head.pubkey == owner && head.d == d && head.kind == kind.kind())
        .collect();
    Ok(heads(&candidates).into_values().next().cloned())
}

// ---------------------------------------------------------------------------
// The model behind the catalog and its share switches

/// `queued`: the relay has not confirmed it yet; `rejected`: it refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogDelivery {
    Queued,
    Accepted,
    Rejected,
}

fn delivery(op: &Operation) -> CatalogDelivery {
    match op.delivery {
        Delivery::Accepted | Delivery::Seen => CatalogDelivery::Accepted,
        Delivery::Failed => CatalogDelivery::Rejected,
        Delivery::Sending | Delivery::Unknown => CatalogDelivery::Queued,
    }
}

/// The newest local change of one coordinate, until it is dismissed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareChange {
    pub operation: nostr::prelude::EventId,
    pub shared: bool,
    pub delivery: CatalogDelivery,
    /// Queued with nothing in flight; `retry` resumes it.
    pub stalled: bool,
    pub error: Option<String>,
}

/// What one share switch shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShareState {
    /// Whether the newest relay-accepted head is shared.
    pub shared: bool,
    pub change: Option<ShareChange>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CatalogStatus {
    Unavailable,
    #[default]
    Idle,
    Loading,
    Ready,
    Error,
}

/// Everything the catalog shows, rebuilt on every change.
#[derive(Clone, Debug, Default)]
pub struct CatalogView {
    pub status: CatalogStatus,
    pub error: Option<String>,
    /// Newest first.
    pub agents: Vec<AgentPublication>,
    pub teams: Vec<TeamPublication>,
    /// This viewer's coordinates, `(kind, d)`.
    pub own: HashMap<(CatalogKind, String), ShareState>,
    /// The session's outbox is connected, so sharing can change.
    pub writable: bool,
}

impl CatalogView {
    pub fn state(&self, kind: CatalogKind, d: &str) -> ShareState {
        self.own
            .get(&(kind, d.to_owned()))
            .cloned()
            .unwrap_or_default()
    }

    pub fn publications(&self) -> impl Iterator<Item = Publication> + '_ {
        self.agents
            .iter()
            .cloned()
            .map(Publication::Agent)
            .chain(self.teams.iter().cloned().map(Publication::Team))
    }
}

/// The newest local change per own coordinate.
type Changes<'a> = HashMap<String, (&'a Arc<Operation>, Head)>;

/// The catalog's own state: what the relay returned, what it confirmed,
/// and which own changes a strong read proved to be the head.
#[derive(Default)]
pub struct CatalogModel {
    pub viewer: String,
    pub status: CatalogStatus,
    pub error: Option<String>,
    relay: Vec<Head>,
    /// Relay-confirmed own heads, kept apart from the dismissible outbox
    /// and the transient read so neither can revive a replaced head.
    confirmed: HashMap<String, Head>,
    /// Own changes a strong read returned as the head. An accepted receipt
    /// alone is not proof: a relay holding a newer head keeps it.
    pub verified: HashSet<String>,
    /// Confirmation failed or found another head; re-read on retry only.
    pub unconfirmed: HashSet<String>,
    /// The last build's heads parsed, by event id: an id fixes its content.
    parsed: HashMap<String, Option<Publication>>,
}

impl CatalogModel {
    pub fn new(viewer: String) -> Self {
        Self {
            viewer,
            ..Self::default()
        }
    }

    pub fn retain(&mut self, head: Head) {
        if head.pubkey != self.viewer {
            return;
        }
        let key = head.coordinate();
        if self
            .confirmed
            .get(&key)
            .is_none_or(|known| head.outranks(known))
        {
            self.confirmed.insert(key, head);
        }
    }

    /// A finished full read.
    pub fn loaded(&mut self, events: Vec<Verified>) {
        for event in &events {
            if let Some(head) = Head::from_event(event) {
                self.retain(head);
            }
        }
        self.relay = events.iter().filter_map(|e| Head::from_event(e)).collect();
        self.status = CatalogStatus::Ready;
        self.error = None;
        self.unconfirmed.clear();
    }

    pub fn failed(&mut self) {
        self.status = CatalogStatus::Error;
        self.error = Some(READ_FAILED.into());
    }

    /// The viewer's own catalog operations in `ops`, as heads.
    pub fn local<'a>(&self, ops: &'a [Arc<Operation>]) -> Vec<(&'a Arc<Operation>, Head)> {
        ops.iter()
            .filter_map(|op| {
                let head = Head::from_unsigned(&op.event)?;
                (head.pubkey == self.viewer).then_some((op, head))
            })
            .collect()
    }

    /// Accepted own changes that still need a strong head read.
    pub fn to_verify(&self, ops: &[Arc<Operation>]) -> Vec<Head> {
        self.local(ops)
            .into_iter()
            .filter(|(op, head)| {
                delivery(op) == CatalogDelivery::Accepted
                    && !self.verified.contains(&head.id)
                    && !self.unconfirmed.contains(&head.id)
            })
            .map(|(_, head)| head)
            .collect()
    }

    /// Records the head a confirmation read found for `change`.
    pub fn confirmed_read(&mut self, change: &Head, head: Option<Head>) {
        let matches = head.as_ref().is_some_and(|head| head.id == change.id);
        if let Some(head) = head {
            self.retain(head);
        }
        if matches {
            self.verified.insert(change.id.clone());
            self.unconfirmed.remove(&change.id);
        } else {
            self.unconfirmed.insert(change.id.clone());
        }
    }

    /// The newest local change per own coordinate, and the own heads.
    fn changes<'a>(&mut self, ops: &'a [Arc<Operation>]) -> (HashMap<String, Head>, Changes<'a>) {
        let local = self.local(ops);
        // Verified local heads count at once; the relay read catches up.
        for (_, head) in &local {
            if self.verified.contains(&head.id) {
                self.retain(head.clone());
            }
        }
        let all: Vec<&Head> = self.relay.iter().chain(self.confirmed.values()).collect();
        let heads: HashMap<String, Head> = heads(all.iter().copied())
            .into_iter()
            .map(|(key, head)| (key, head.clone()))
            .collect();
        let mut changes: Changes = HashMap::new();
        for (op, head) in local {
            let key = head.coordinate();
            if changes
                .get(&key)
                .is_none_or(|(_, known)| head.outranks(known))
            {
                changes.insert(key, (op, head));
            }
        }
        // A change the observed head outranks is history, not a notice.
        changes.retain(|key, (_, change)| {
            heads
                .get(key)
                .is_none_or(|head| head.id == change.id || !head.outranks(change))
        });
        (heads, changes)
    }

    /// The newest content of an own coordinate: the newest local change,
    /// else the relay head.
    pub fn content(
        &mut self,
        ops: &[Arc<Operation>],
        kind: CatalogKind,
        d: &str,
    ) -> Option<String> {
        let key = format!("{}:{}:{d}", kind.kind(), self.viewer);
        let (heads, changes) = self.changes(ops);
        changes
            .get(&key)
            .map(|(_, head)| head.content.clone())
            .or_else(|| heads.get(&key).map(|head| head.content.clone()))
    }

    /// The own head's `created_at`, which a replacement must outrank, and
    /// the local change being replaced when the relay refused it.
    pub fn replacing(
        &mut self,
        ops: &[Arc<Operation>],
        kind: CatalogKind,
        d: &str,
    ) -> (Option<u64>, Option<nostr::prelude::EventId>) {
        let key = format!("{}:{}:{d}", kind.kind(), self.viewer);
        let (heads, changes) = self.changes(ops);
        let refused = changes
            .get(&key)
            .filter(|(op, _)| delivery(op) == CatalogDelivery::Rejected)
            .map(|(op, _)| op.id());
        (heads.get(&key).map(|head| head.created_at), refused)
    }

    pub fn build(&mut self, ops: &[Arc<Operation>], writable: bool) -> CatalogView {
        let (heads, changes) = self.changes(ops);
        let mut agents = Vec::new();
        let mut teams = Vec::new();
        let mut parsed = HashMap::with_capacity(heads.len());
        for head in heads.values() {
            let publication = self
                .parsed
                .remove(&head.id)
                .unwrap_or_else(|| parse_head(head));
            match &publication {
                Some(Publication::Agent(p)) => agents.push(p.clone()),
                Some(Publication::Team(p)) => teams.push(p.clone()),
                None => {}
            }
            parsed.insert(head.id.clone(), publication);
        }
        self.parsed = parsed;
        agents.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(a.event_id.cmp(&b.event_id))
        });
        teams.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then(a.event_id.cmp(&b.event_id))
        });
        let mut own: HashMap<(CatalogKind, String), ShareState> = HashMap::new();
        for head in heads.values().filter(|head| head.pubkey == self.viewer) {
            if let Some(kind) = CatalogKind::from_kind(head.kind) {
                own.entry((kind, head.d.clone())).or_default().shared = head.shared;
            }
        }
        for (op, head) in changes.values() {
            let Some(kind) = CatalogKind::from_kind(head.kind) else {
                continue;
            };
            let unconfirmed = self.unconfirmed.contains(&head.id);
            let mut outcome = delivery(op);
            if outcome == CatalogDelivery::Accepted && !self.verified.contains(&head.id) {
                outcome = CatalogDelivery::Queued;
            }
            own.entry((kind, head.d.clone())).or_default().change = Some(ShareChange {
                operation: op.id(),
                shared: head.shared,
                delivery: outcome,
                stalled: op.delivery == Delivery::Unknown || unconfirmed,
                error: op
                    .error
                    .clone()
                    .or_else(|| unconfirmed.then(|| UNCONFIRMED.to_owned())),
            });
        }
        CatalogView {
            status: self.status.clone(),
            error: self.error.clone(),
            agents,
            teams,
            own,
            writable,
        }
    }
}

/// buzz-app's share notice for a queued or accepted change.
pub fn share_notice(name: &str, shared: bool, delivery: CatalogDelivery) -> String {
    match (delivery, shared) {
        (CatalogDelivery::Queued, true) => {
            format!("Sharing {name} is queued. It will appear after the relay accepts the update.")
        }
        (CatalogDelivery::Queued, false) => format!(
            "Removing {name} is queued. It may remain discoverable until the relay accepts the update."
        ),
        (_, true) => format!("Published {name} to the community catalog."),
        (_, false) => format!("{name} is no longer discoverable in the community catalog."),
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
