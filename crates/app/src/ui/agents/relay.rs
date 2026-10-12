//! What the community relays say about the viewer's agents: buzz-app's
//! agent library (`src/features/agents/{library,relay-library,inventory}.ts`),
//! identity archives (`src/features/relay/identity-archives.ts`), the
//! per-community inventory (`use-community-inventory.ts`) and relay-only
//! removal (`src/features/agents/relay-removal.ts`).
//!
//! Discovery only: none of this proves key custody, membership or running
//! state. Reads use the community's signed HTTP lane, so they need no live
//! session.

use buzz_identity::{Identity, Purpose};
use buzz_relay_client::members::is_key;
use buzz_relay_client::outbox::accept_publish;
use buzz_relay_client::profiles::Profile;
use buzz_relay_client::{RelayHttp, RelayOrigin, Verifier};
use nostr::prelude::Event;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

/// Owner-authored agent definitions (30175) and identities (30177).
const INVENTORY_KINDS: [u16; 2] = [30175, 30177];
const PAGE: usize = 200;
const MAX_EVENTS: usize = 10_000;
const MAX_ARCHIVE_BYTES: usize = 2 * 1024 * 1024;

fn filter(value: Value) -> buzz_relay_client::Filter {
    match value {
        Value::Object(map) => map,
        _ => unreachable!("filters are objects"),
    }
}

fn tag<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .find(|tag| tag.first().map(String::as_str) == Some(name))
        .and_then(|tag| tag.get(1))
        .map(String::as_str)
}

/// The newer of two replaceable events (NIP-01 order).
fn newer(a: Option<Event>, b: Event) -> Event {
    match a {
        Some(a)
            if a.created_at > b.created_at
                || (a.created_at == b.created_at && a.id.to_hex() < b.id.to_hex()) =>
        {
            a
        }
        _ => b,
    }
}

/// An agent definition (a profile an identity can come from).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
}

/// An agent identity the viewer's library knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryIdentity {
    pub pubkey: String,
    pub name: String,
    pub avatar: Option<String>,
    pub definition: Option<String>,
}

/// `AgentLibrary`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Library {
    pub definitions: Vec<Definition>,
    pub identities: Vec<LibraryIdentity>,
}

/// buzz-app's `definitionSlug`: legacy `persona_id` uses the source ID;
/// kind 30175 publishes its slug.
pub fn definition_slug(id: &str) -> String {
    let slug: String = id
        .chars()
        .map(|c| match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    let slug = if slug.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        slug
    } else {
        format!("a{slug}")
    };
    slug.chars().take(64).collect()
}

/// An `https:` URL or an inline raster image, else nothing.
fn avatar_source(value: Option<&Value>) -> Option<String> {
    let source = value?.as_str()?.trim();
    let image = ["png", "jpeg", "webp", "gif"]
        .iter()
        .any(|format| source.starts_with(&format!("data:image/{format};base64,")));
    (source.starts_with("https://") || image).then(|| source.to_string())
}

/// One artwork rule for every agent surface: an identity without its own
/// art shows its definition's.
fn inherit_avatars(mut library: Library) -> Library {
    let art: HashMap<String, String> = library
        .definitions
        .iter()
        .filter_map(|row| Some((row.id.clone(), row.avatar.clone()?)))
        .collect();
    for identity in &mut library.identities {
        if identity.avatar.is_none() {
            identity.avatar = identity
                .definition
                .as_ref()
                .and_then(|id| art.get(id).cloned());
        }
    }
    library
}

/// `readRelayLibrary`: the owner's 30175/30177 heads, paged.
pub async fn read_library(http: &RelayHttp, owner: &str) -> Result<Library, String> {
    let mut heads: BTreeMap<String, Event> = BTreeMap::new();
    let mut cursor: Option<(u64, String)> = None;
    let mut count = 0;
    loop {
        let mut query = json!({ "kinds": INVENTORY_KINDS, "authors": [owner], "limit": PAGE });
        if let Some((until, before)) = &cursor {
            query["until"] = json!(until);
            query["before_id"] = json!(before);
        }
        let page = http
            .query(&[filter(query)])
            .await
            .map_err(|error| error.to_string())?;
        if page.iter().any(|event| {
            event.pubkey.to_hex() != owner || !INVENTORY_KINDS.contains(&event.kind.as_u16())
        }) {
            return Err("Agent inventory response is outside its owner scope".into());
        }
        count += page.len();
        if count > MAX_EVENTS {
            return Err("Agent inventory exceeds read budget".into());
        }
        for event in &page {
            let Some(d) = tag(event, "d") else {
                continue;
            };
            let coordinate = format!("{}:{d}", event.kind.as_u16());
            let previous = heads.remove(&coordinate);
            heads.insert(coordinate, newer(previous, event.clone()));
        }
        if page.len() < PAGE {
            break;
        }
        let last = page
            .iter()
            .min_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then(b.id.to_hex().cmp(&a.id.to_hex()))
            })
            .ok_or("Missing inventory cursor")?;
        let next = (last.created_at.as_secs(), last.id.to_hex());
        if let Some((until, before)) = &cursor
            && (next.0 > *until || (next.0 == *until && next.1 <= *before))
        {
            return Err("Agent inventory pagination did not advance".into());
        }
        cursor = Some(next);
    }
    let mut library = Library::default();
    for event in heads.values() {
        let id = tag(event, "d").unwrap_or_default();
        let Ok(Value::Object(body)) = serde_json::from_str::<Value>(&event.content) else {
            continue;
        };
        let kind = event.kind.as_u16();
        let raw_name = body.get(if kind == 30175 {
            "display_name"
        } else {
            "name"
        });
        let name = raw_name
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        if kind == 30175 {
            let valid = id.len() <= 64
                && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && id.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                });
            // Catalog shares are addressed by agent pubkey; the identity row
            // is the agent.
            if !valid || is_key(id) {
                continue;
            }
            library.definitions.push(Definition {
                id: id.into(),
                name: name.into(),
                avatar: avatar_source(body.get("avatar_url")),
            });
        } else {
            if !is_key(id) {
                continue;
            }
            library.identities.push(LibraryIdentity {
                pubkey: id.into(),
                name: name.into(),
                avatar: None,
                definition: body
                    .get("persona_id")
                    .and_then(Value::as_str)
                    .filter(|reference| !reference.is_empty())
                    .map(definition_slug),
            });
        }
    }
    Ok(inherit_avatars(library))
}

/// `combineInventory`: local names and artwork win; relay-only keys are
/// added. Names never join identities.
pub fn combine(local: &Library, relay: &Library) -> Library {
    let mut slugs: HashMap<String, usize> = HashMap::new();
    for row in &local.definitions {
        *slugs.entry(definition_slug(&row.id)).or_default() += 1;
    }
    let local_id = |id: &str| {
        let slug = definition_slug(id);
        if slugs.get(&slug) == Some(&1) {
            format!("profile:{slug}")
        } else {
            format!("local:{id}")
        }
    };
    let exact: HashMap<&str, String> = local
        .definitions
        .iter()
        .map(|row| (row.id.as_str(), local_id(&row.id)))
        .collect();
    let mut definitions: BTreeMap<String, Definition> = relay
        .definitions
        .iter()
        .map(|row| {
            let id = format!("profile:{}", row.id);
            (id.clone(), Definition { id, ..row.clone() })
        })
        .collect();
    for row in &local.definitions {
        let id = local_id(&row.id);
        definitions.insert(id.clone(), Definition { id, ..row.clone() });
    }
    let mut identities: BTreeMap<String, LibraryIdentity> = relay
        .identities
        .iter()
        .map(|row| {
            let pubkey = row.pubkey.to_lowercase();
            (
                pubkey.clone(),
                LibraryIdentity {
                    pubkey,
                    definition: row.definition.as_ref().map(|id| format!("profile:{id}")),
                    ..row.clone()
                },
            )
        })
        .collect();
    for row in &local.identities {
        let pubkey = row.pubkey.to_lowercase();
        let relay = identities.remove(&pubkey);
        identities.insert(
            pubkey.clone(),
            LibraryIdentity {
                pubkey,
                name: row.name.clone(),
                avatar: row
                    .avatar
                    .clone()
                    .or_else(|| relay.as_ref().and_then(|r| r.avatar.clone())),
                definition: match &row.definition {
                    Some(id) => Some(
                        exact
                            .get(id.as_str())
                            .cloned()
                            .unwrap_or_else(|| format!("local:{id}")),
                    ),
                    None => relay.and_then(|r| r.definition),
                },
            },
        );
    }
    inherit_avatars(Library {
        definitions: definitions.into_values().collect(),
        identities: identities.into_values().collect(),
    })
}

/// `identityTiles`: one row per key, and the definitions no identity
/// links to.
pub fn tiles(library: &Library) -> (Vec<LibraryIdentity>, Vec<Definition>) {
    let linked: BTreeSet<&str> = library
        .identities
        .iter()
        .filter_map(|row| row.definition.as_deref())
        .collect();
    let mut seen = BTreeSet::new();
    let identities = library
        .identities
        .iter()
        .filter(|row| seen.insert(row.pubkey.to_lowercase()))
        .cloned()
        .collect();
    let profiles = library
        .definitions
        .iter()
        .filter(|row| !linked.contains(row.id.as_str()))
        .cloned()
        .collect();
    (identities, profiles)
}

/// The host libraries converted to the shared shape.
pub fn from_host(library: &buzz_host::api::Library) -> Library {
    Library {
        definitions: library
            .definitions
            .iter()
            .map(|row| Definition {
                id: row.id.clone(),
                name: row.name.clone(),
                avatar: row.avatar.clone(),
            })
            .collect(),
        identities: library
            .identities
            .iter()
            .map(|row| LibraryIdentity {
                pubkey: row.pubkey.clone(),
                name: row.name.clone(),
                avatar: row.avatar.clone(),
                definition: row.definition_id.clone(),
            })
            .collect(),
    }
}

/// The relay's archive snapshot: who is archived in this community.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ArchiveSnapshot {
    pub archived: BTreeSet<String>,
    pub head: Option<(u64, String)>,
}

/// Reads the relay-signed 13535 snapshot. `last` is the head of the
/// previous read: an older snapshot is refused rather than shown.
pub async fn read_archives(
    http: &RelayHttp,
    authority: &str,
    last: Option<&(u64, String)>,
) -> Result<ArchiveSnapshot, String> {
    let invalid = || {
        "Archive visibility is unknown. Retry to obtain a valid current relay snapshot (2 MiB limit).".to_string()
    };
    let events = http
        .query(&[filter(json!({
            "kinds": [13535], "authors": [authority], "limit": 1, "consistency": "strong",
        }))])
        .await
        .map_err(|_| invalid())?;
    let [event] = events.as_slice() else {
        if events.is_empty() {
            return Ok(ArchiveSnapshot::default());
        }
        return Err(invalid());
    };
    let markers: Vec<_> = event
        .tags
        .iter()
        .filter(|tag| tag.as_slice().first().map(String::as_str) == Some("-"))
        .collect();
    let size = serde_json::to_string(event).map_or(usize::MAX, |json| json.len());
    if event.kind.as_u16() != 13535
        || event.pubkey.to_hex() != authority
        || !event.content.is_empty()
        || markers.len() != 1
        || markers[0].as_slice().len() != 1
        || size > MAX_ARCHIVE_BYTES
    {
        return Err(invalid());
    }
    let head = (event.created_at.as_secs(), event.id.to_hex());
    if let Some(last) = last
        && (head.0 < last.0 || (head.0 == last.0 && head.1 > last.1))
    {
        return Err(invalid());
    }
    let archived = event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .filter(|tag| tag.first().map(String::as_str) == Some("p"))
        .filter_map(|tag| tag.get(1))
        .filter(|key| is_key(key))
        .cloned()
        .collect();
    Ok(ArchiveSnapshot {
        archived,
        head: Some(head),
    })
}

/// How the viewer may archive an identity: as its verified owner (with
/// the owner attestation), as a community owner or admin, or as itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consent {
    pub auth: Option<Vec<String>>,
    pub admin: bool,
}

/// `consent`: a render guard; the request checks again before signing.
pub async fn consent(
    http: &RelayHttp,
    authority: &str,
    viewer: &str,
    target: &str,
) -> Result<Option<Consent>, String> {
    if !is_key(target) {
        return Ok(None);
    }
    if target == viewer {
        return Ok(Some(Consent {
            auth: None,
            admin: false,
        }));
    }
    let events = http
        .query(&[
            filter(json!({ "kinds": [0], "authors": [target], "limit": 1, "consistency": "strong" })),
            filter(json!({ "kinds": [13534], "authors": [authority], "limit": 1, "consistency": "strong" })),
        ])
        .await
        .map_err(|error| error.to_string())?;
    let verifier = Verifier::default();
    let profile = events
        .iter()
        .filter(|event| event.kind.as_u16() == 0 && event.pubkey.to_hex() == target)
        .cloned()
        .reduce(|a, b| newer(Some(a), b));
    let auth = profile.and_then(|event| {
        let tags: Vec<Vec<String>> = event
            .tags
            .iter()
            .map(|tag| tag.as_slice().to_vec())
            .filter(|tag| tag.first().map(String::as_str) == Some("auth"))
            .collect();
        let [tag] = tags.as_slice() else {
            return None;
        };
        let verified = verifier.verify(event.clone()).ok()?;
        (Profile::from_event(&verified).attested_owner.as_deref() == Some(viewer))
            .then(|| tag.clone())
    });
    let roster = events
        .iter()
        .filter(|event| event.kind.as_u16() == 13534 && event.pubkey.to_hex() == authority)
        .cloned()
        .reduce(|a, b| newer(Some(a), b));
    let admin = roster.is_some_and(|roster| {
        roster.tags.iter().any(|tag| {
            let tag = tag.as_slice();
            tag.first().map(String::as_str) == Some("member")
                && tag.get(1).map(String::as_str) == Some(viewer)
                && matches!(tag.get(2).map(String::as_str), Some("owner" | "admin"))
        })
    });
    if auth.is_none() && !admin {
        return Ok(None);
    }
    Ok(Some(Consent { auth, admin }))
}

/// NIP-OA `created_at<`/`created_at>` bounds of an `auth` tag hold now.
fn within_time_bounds(conditions: &str, now: u64) -> bool {
    conditions
        .split('&')
        .filter(|c| !c.is_empty())
        .all(|clause| {
            if let Some(limit) = clause.strip_prefix("created_at<") {
                limit.parse::<u64>().is_ok_and(|limit| now < limit)
            } else if let Some(limit) = clause.strip_prefix("created_at>") {
                limit.parse::<u64>().is_ok_and(|limit| now > limit)
            } else {
                true
            }
        })
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Signs and posts one event; `Ok(true)` when the relay accepted it,
/// `Ok(false)` when the outcome is unknown.
async fn publish(
    http: &RelayHttp,
    identity: &Identity,
    kind: u16,
    tags: Vec<Vec<String>>,
) -> Result<bool, String> {
    let unsigned = identity
        .template(Purpose::Content, kind, now(), tags, String::new())
        .map_err(|error| error.to_string())?;
    let event = identity
        .sign(Purpose::Content, unsigned)
        .map_err(|error| error.to_string())?;
    let body = serde_json::to_string(&event).map_err(|error| error.to_string())?;
    match accept_publish(http.post("/events", body).await, &event.id) {
        Ok(_) => Ok(true),
        Err(buzz_relay_client::outbox::PublishError::Rejected(reason)) => Err(reason),
        Err(buzz_relay_client::outbox::PublishError::Uncertain(_)) => Ok(false),
    }
}

/// Archive or unarchive an identity (NIP-IA 9035/9036), then confirm it
/// with a fresh snapshot.
pub async fn request_archive(
    http: &RelayHttp,
    identity: &Identity,
    authority: &str,
    target: &str,
    archive: bool,
    last: Option<(u64, String)>,
) -> Result<ArchiveSnapshot, String> {
    let viewer = identity.pubkey_hex();
    let path = consent(http, authority, &viewer, target)
        .await?
        .ok_or("You can no longer archive this identity")?;
    // Role authority stands alone, so never attach a credential whose time
    // bounds could fail it.
    let auth = if path.admin { None } else { path.auth };
    if let Some(auth) = &auth
        && !within_time_bounds(auth.get(2).map(String::as_str).unwrap_or(""), now())
    {
        return Err("You can no longer archive this identity".into());
    }
    let mut tags = vec![vec!["-".to_string()], vec!["p".into(), target.into()]];
    tags.extend(auth);
    let kind = if archive { 9035 } else { 9036 };
    let delivered = publish(http, identity, kind, tags).await?;
    let snapshot = read_archives(http, authority, last.as_ref()).await?;
    if snapshot.archived.contains(target) != archive {
        return Err(if delivered {
            "The relay did not confirm the change. Retry.".into()
        } else {
            "Relay delivery could not be confirmed".into()
        });
    }
    Ok(snapshot)
}

/// The channels whose relay roster lists `pubkey`.
async fn member_channels(
    http: &RelayHttp,
    authority: &str,
    pubkey: &str,
) -> Result<BTreeSet<String>, String> {
    let limit = 500;
    let events = http
        .query(&[filter(json!({
            "kinds": [39002], "authors": [authority], "#p": [pubkey],
            "limit": limit, "consistency": "strong",
        }))])
        .await
        .map_err(|error| error.to_string())?;
    if events.len() >= limit {
        return Err("Too many channels to confirm. Retry later.".into());
    }
    Ok(events
        .iter()
        .filter(|event| event.pubkey.to_hex() == authority)
        .filter_map(|event| tag(event, "d").map(str::to_string))
        .collect())
}

/// `removeRelayAgent`: archive the identity when a consent path exists,
/// delete the viewer's 30177 record, then remove it from its channels as
/// far as the relay allows.
pub async fn remove_relay_agent(
    http: &RelayHttp,
    identity: &Identity,
    authority: &str,
    pubkey: &str,
    archived: bool,
    last: Option<(u64, String)>,
) -> Result<(), String> {
    let viewer = identity.pubkey_hex();
    if !archived && consent(http, authority, &viewer, pubkey).await?.is_some() {
        request_archive(http, identity, authority, pubkey, true, last).await?;
    }
    let coordinate = format!("30177:{viewer}:{pubkey}");
    if !publish(http, identity, 5, vec![vec!["a".into(), coordinate]]).await? {
        return Err("Record deletion is not confirmed. Retry.".into());
    }
    // Best effort: a failed channel removal does not fail the removal.
    match member_channels(http, authority, pubkey).await {
        Ok(channels) => {
            for channel in channels {
                if let Err(error) = publish(
                    http,
                    identity,
                    9001,
                    vec![vec!["h".into(), channel], vec!["p".into(), pubkey.into()]],
                )
                .await
                {
                    tracing::warn!(%error, "agent channel removal was not confirmed");
                }
            }
        }
        Err(error) => tracing::warn!(%error, "agent channels could not be listed"),
    }
    Ok(())
}

/// One community's answer to the inventory read: the identities its
/// relay holds records for, then their profiles there.
#[derive(Clone, Debug, Default)]
pub struct CommunityRead {
    pub identities: Option<Vec<String>>,
    pub error: bool,
    pub profiles: Option<HashMap<String, Profile>>,
    pub profile_error: bool,
}

/// `agent-inventory` and `communityProfiles` for one community.
pub async fn read_community(http: &RelayHttp, owner: &str) -> CommunityRead {
    let identities = match read_library(http, owner).await {
        Ok(library) => library
            .identities
            .into_iter()
            .map(|row| row.pubkey)
            .collect::<Vec<_>>(),
        Err(_) => {
            return CommunityRead {
                error: true,
                ..Default::default()
            };
        }
    };
    let verifier = Verifier::default();
    let mut profiles = HashMap::new();
    for authors in identities.chunks(500) {
        let events = match http
            .query(&[filter(
                json!({ "kinds": [0], "authors": authors, "limit": 500 }),
            )])
            .await
        {
            Ok(events) => events,
            Err(_) => {
                return CommunityRead {
                    identities: Some(identities),
                    profile_error: true,
                    ..Default::default()
                };
            }
        };
        let mut newest: BTreeMap<String, Event> = BTreeMap::new();
        for event in events {
            let author = event.pubkey.to_hex();
            if event.kind.as_u16() != 0 || !authors.contains(&author) {
                return CommunityRead {
                    identities: Some(identities),
                    profile_error: true,
                    ..Default::default()
                };
            }
            let previous = newest.remove(&author);
            newest.insert(author, newer(previous, event));
        }
        for (author, event) in newest {
            if let Ok(verified) = verifier.verify(event) {
                profiles.insert(author, Profile::from_event(&verified));
            }
        }
    }
    CommunityRead {
        identities: Some(identities),
        profiles: Some(profiles),
        ..Default::default()
    }
}

/// A community's signed HTTP lane for the viewer.
pub fn lane(origin: &RelayOrigin, identity: Arc<Identity>) -> RelayHttp {
    RelayHttp::new(origin.clone(), identity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_the_reference() {
        assert_eq!(definition_slug("Code Reviewer"), "code-reviewer");
        assert_eq!(definition_slug("_x"), "a_x");
        assert_eq!(definition_slug(&"a".repeat(80)).len(), 64);
    }

    fn identity(pubkey: &str, name: &str, definition: Option<&str>) -> LibraryIdentity {
        LibraryIdentity {
            pubkey: pubkey.into(),
            name: name.into(),
            avatar: None,
            definition: definition.map(Into::into),
        }
    }

    #[test]
    fn local_names_win_and_relay_keys_are_added() {
        let relay = Library {
            definitions: vec![Definition {
                id: "reviewer".into(),
                name: "Reviewer".into(),
                avatar: Some("https://example.com/r.png".into()),
            }],
            identities: vec![
                identity("AA", "Relay name", Some("reviewer")),
                identity("bb", "Only relay", None),
            ],
        };
        let local = Library {
            definitions: vec![Definition {
                id: "Reviewer".into(),
                name: "Local reviewer".into(),
                avatar: None,
            }],
            identities: vec![identity("aa", "Local name", Some("Reviewer"))],
        };
        let combined = combine(&local, &relay);
        let names: Vec<_> = combined
            .identities
            .iter()
            .map(|row| (row.pubkey.as_str(), row.name.as_str()))
            .collect();
        assert_eq!(names, [("aa", "Local name"), ("bb", "Only relay")]);
        // The local definition replaces the relay one with the same slug.
        assert_eq!(combined.definitions.len(), 1);
        assert_eq!(combined.definitions[0].id, "profile:reviewer");
        assert_eq!(
            combined.identities[0].definition.as_deref(),
            Some("profile:reviewer")
        );
        let (identities, profiles) = tiles(&combined);
        assert_eq!(identities.len(), 2);
        assert!(profiles.is_empty());
    }

    #[test]
    fn unlinked_definitions_are_profiles_without_identities() {
        let library = Library {
            definitions: vec![Definition {
                id: "profile:lonely".into(),
                name: "Lonely".into(),
                avatar: None,
            }],
            identities: Vec::new(),
        };
        assert_eq!(tiles(&library).1.len(), 1);
    }

    #[test]
    fn auth_time_bounds_hold_now() {
        assert!(within_time_bounds("", 10));
        assert!(within_time_bounds("kind=0&created_at<20", 10));
        assert!(!within_time_bounds("created_at>20", 10));
    }
}
