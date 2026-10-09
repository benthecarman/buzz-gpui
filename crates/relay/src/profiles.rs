//! Profiles (kind 0) for everyone the UI shows (spec §5f).
//!
//! Only the newest kind 0 per author counts. Unchanged profiles keep their
//! `Arc`, so views can tell at a glance whether anything they show moved.

use crate::event::{Verified, is_newer, kinds};
use crate::socket::Filter;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

/// Authors per read.
pub const CHUNK: usize = 500;
/// Profiles retained per session.
pub const CAPACITY: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Profile {
    pub name: String,
    /// `name` is the pubkey prefix because the profile has no usable name.
    pub name_is_fallback: bool,
    /// An `https:` URL or an inline raster `data:` URL; anything else is
    /// dropped so the UI shows initials.
    pub picture: Option<String>,
    pub about: Option<String>,
    /// Self-declared and not DNS-verified.
    pub nip05: Option<String>,
    pub is_agent: bool,
    /// From a NIP-OA `auth` tag; its signature is checked elsewhere.
    pub owner_pubkey: Option<String>,
}

impl Profile {
    /// What to show for an author with no profile yet.
    pub fn fallback(pubkey: &str) -> Self {
        Self {
            name: pubkey.chars().take(10).collect(),
            name_is_fallback: true,
            ..Default::default()
        }
    }

    /// Parses a kind 0 (`profiles.ts:7-73`).
    pub fn from_event(event: &Verified) -> Self {
        let pubkey = event.pubkey_hex();
        let auth: Option<&[String]> = event
            .tags_named("auth")
            .find(|tag| tag.len() == 4 && is_hex(&tag[1], 64) && is_hex(&tag[3], 128));
        let owner_pubkey = auth.map(|tag| tag[1].clone());
        let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&event.content) else {
            return Self {
                is_agent: owner_pubkey.is_some(),
                owner_pubkey,
                ..Self::fallback(&pubkey)
            };
        };
        let text = |key: &str| {
            fields
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let name = text("display_name").or_else(|| text("name"));
        let flagged = |key: &str| fields.get(key) == Some(&Value::Bool(true));
        Self {
            name_is_fallback: name.is_none(),
            name: name.unwrap_or_else(|| pubkey.chars().take(10).collect()),
            picture: text("picture").filter(|url| avatar_source(url)),
            about: text("about"),
            nip05: text("nip05"),
            is_agent: flagged("is_agent") || flagged("isAgent") || owner_pubkey.is_some(),
            owner_pubkey,
        }
    }
}

/// `avatar-source.ts`: credential-free HTTPS up to 2048 characters, or an
/// inline PNG/JPEG/WebP/GIF up to 512 KiB.
fn avatar_source(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        return url.len() <= 2048 && !authority.is_empty() && !authority.contains('@');
    }
    ["png", "jpeg", "webp", "gif"].iter().any(|format| {
        url.strip_prefix(&format!("data:image/{format};base64,"))
            .is_some_and(|_| url.len() <= 512 * 1024)
    })
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub type Profiles = Arc<HashMap<String, Arc<Profile>>>;

#[derive(Default)]
pub struct ProfileDirectory {
    events: HashMap<String, Verified>,
    /// Least recently updated first, for eviction.
    order: VecDeque<String>,
    snapshot: Profiles,
    in_flight: HashSet<String>,
}

impl ProfileDirectory {
    pub fn snapshot(&self) -> Profiles {
        self.snapshot.clone()
    }

    /// Filters for the ids not already known or being fetched, in chunks of
    /// 500 authors. The ids are marked in flight until
    /// [`Self::finished`].
    pub fn ensure<'a>(
        &mut self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Vec<(Vec<String>, Filter)> {
        let mut missing: Vec<String> = ids
            .into_iter()
            .filter(|id| {
                is_hex(id, 64) && !self.events.contains_key(*id) && !self.in_flight.contains(*id)
            })
            .map(str::to_string)
            .collect();
        missing.sort();
        missing.dedup();
        self.in_flight.extend(missing.iter().cloned());
        missing
            .chunks(CHUNK)
            .map(|chunk| (chunk.to_vec(), authors_filter(chunk)))
            .collect()
    }

    /// Re-reads every known id, e.g. after a reconnect. Current profiles
    /// stay until something newer arrives.
    pub fn refresh(&mut self) -> Vec<(Vec<String>, Filter)> {
        let mut known: Vec<String> = self.events.keys().cloned().collect();
        known.sort();
        self.in_flight.extend(known.iter().cloned());
        known
            .chunks(CHUNK)
            .map(|chunk| (chunk.to_vec(), authors_filter(chunk)))
            .collect()
    }

    pub fn finished(&mut self, ids: &[String]) {
        for id in ids {
            self.in_flight.remove(id);
        }
    }

    /// Takes any kind 0 events; returns whether the snapshot changed.
    pub fn accept(&mut self, events: &[Verified]) -> bool {
        let mut changed: Vec<String> = Vec::new();
        for event in events.iter().filter(|e| e.kind_u16() == kinds::PROFILE) {
            let pubkey = event.pubkey_hex();
            let newer = self
                .events
                .get(&pubkey)
                .is_none_or(|current| is_newer(event, current));
            if newer {
                self.events.insert(pubkey.clone(), event.clone());
                self.order.retain(|id| id != &pubkey);
                self.order.push_back(pubkey.clone());
                changed.push(pubkey);
            }
        }
        if changed.is_empty() {
            return false;
        }
        let mut evicted = Vec::new();
        while self.order.len() > CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.events.remove(&oldest);
                evicted.push(oldest);
            }
        }
        let mut next = (*self.snapshot).clone();
        let mut different = false;
        for id in evicted {
            different |= next.remove(&id).is_some();
        }
        for id in changed {
            let Some(event) = self.events.get(&id) else {
                continue;
            };
            let profile = Profile::from_event(event);
            if next.get(&id).is_none_or(|current| **current != profile) {
                next.insert(id, Arc::new(profile));
                different = true;
            }
        }
        if different {
            self.snapshot = Arc::new(next);
        }
        different
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.order.clear();
        self.in_flight.clear();
        self.snapshot = Profiles::default();
    }
}

fn authors_filter(authors: &[String]) -> Filter {
    match json!({"kinds": [kinds::PROFILE], "authors": authors, "limit": authors.len()}) {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Verifier;
    use nostr::prelude::{EventBuilder, FinalizeEvent, Keys, Kind, Tag, Timestamp};

    fn profile(keys: &Keys, content: &str, at: u64, tags: Vec<Tag>) -> Verified {
        let event = EventBuilder::new(Kind::from_u16(0), content)
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(at))
            .finalize(keys)
            .unwrap();
        Verifier::default().verify(event).unwrap()
    }

    #[test]
    fn parses_names_pictures_and_agents() {
        let keys = Keys::generate();
        let parsed = Profile::from_event(&profile(
            &keys,
            r#"{"name":"alice","display_name":"  Alice ","picture":"https://x.example/a.png","about":" "}"#,
            1,
            vec![],
        ));
        assert_eq!(parsed.name, "Alice");
        assert!(!parsed.name_is_fallback);
        assert_eq!(parsed.picture.as_deref(), Some("https://x.example/a.png"));
        assert_eq!(parsed.about, None);

        let unsafe_picture = Profile::from_event(&profile(
            &keys,
            r#"{"name":"a","picture":"https://u:p@x.example/a"}"#,
            1,
            vec![],
        ));
        assert_eq!(unsafe_picture.picture, None);

        let agent = Profile::from_event(&profile(
            &keys,
            "not json",
            1,
            vec![Tag::custom(
                "auth",
                ["a".repeat(64), String::new(), "b".repeat(128)],
            )],
        ));
        assert!(agent.is_agent && agent.name_is_fallback);
        assert_eq!(agent.owner_pubkey, Some("a".repeat(64)));
    }

    #[test]
    fn keeps_the_newest_and_shares_unchanged_profiles() {
        let keys = Keys::generate();
        let id = keys.public_key().to_hex();
        let mut directory = ProfileDirectory::default();
        assert!(directory.accept(&[profile(&keys, r#"{"name":"new"}"#, 10, vec![])]));
        let before = directory.snapshot()[&id].clone();
        assert!(!directory.accept(&[profile(&keys, r#"{"name":"old"}"#, 5, vec![])]));
        assert_eq!(directory.snapshot()[&id].name, "new");
        // A newer event with identical fields leaves the snapshot alone.
        assert!(!directory.accept(&[profile(&keys, r#"{"name":"new"}"#, 11, vec![])]));
        assert!(Arc::ptr_eq(&before, &directory.snapshot()[&id]));
    }

    #[test]
    fn ensure_skips_known_and_in_flight_ids() {
        let keys = Keys::generate();
        let known = keys.public_key().to_hex();
        let mut directory = ProfileDirectory::default();
        directory.accept(&[profile(&keys, r#"{"name":"k"}"#, 1, vec![])]);
        let other = "c".repeat(64);
        let reads = directory.ensure([known.as_str(), other.as_str(), "short"]);
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].0, vec![other.clone()]);
        assert!(directory.ensure([other.as_str()]).is_empty());
        directory.finished(std::slice::from_ref(&other));
        assert_eq!(directory.ensure([other.as_str()]).len(), 1);
    }
}
