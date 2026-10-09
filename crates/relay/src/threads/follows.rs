//! Which threads the viewer follows (`thread-follows.ts`, `unread.ts`
//! `following`/`follow`, §5c.6).
//!
//! Explicit choices are device-local and per community and viewer. The
//! session persists them as one partition preference
//! ([`THREAD_FOLLOWS_PREF`]) holding the reference's JSON shape:
//! `[["<channel>:<root>", true|false], ...]`, oldest first. Automatic
//! follows from participation are derived from evidence and never stored.

use super::window::is_hex64;
use crate::event::Verified;
use crate::fold::thread_reference;
use nostr::prelude::{EventId, PublicKey};
use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

/// Newest choices win; the oldest are dropped past this many.
pub const THREAD_FOLLOW_LIMIT: usize = 1000;
/// Partition preference name for the explicit choices
/// (`localStorage` `buzz.thread-follows.v1:<scope>` in the reference).
pub const THREAD_FOLLOWS_PREF: &str = "thread-follows.v1";

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Thread unavailable")]
pub struct InvalidThread;

/// Explicit follow choices plus automatic participation.
///
/// `true` follows a thread without replying; `false` unfollows it even
/// after participating. Without a choice, the viewer follows every thread
/// they wrote or replied in. Serializes as the explicit choices only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadFollows {
    /// Oldest first, keys unique.
    choices: Vec<(String, bool)>,
    /// `channel:root` of threads the viewer wrote or replied in.
    participation: HashSet<String>,
}

/// `<channel>:<lowercase root id>`.
pub fn follow_key(channel: &str, root: &EventId) -> String {
    format!("{channel}:{}", root.to_hex())
}

impl ThreadFollows {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a saved preference. Malformed JSON or entries are ignored, as
    /// in the reference, and only the newest [`THREAD_FOLLOW_LIMIT`] stay.
    pub fn from_json(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("follows serialize")
    }

    /// The explicit choice for a thread, if any.
    pub fn choice(&self, channel: &str, root: &EventId) -> Option<bool> {
        let key = follow_key(channel, root);
        self.choices
            .iter()
            .find(|(saved, _)| *saved == key)
            .map(|(_, following)| *following)
    }

    /// An explicit choice, otherwise participation.
    pub fn following(&self, channel: &str, root: &EventId) -> bool {
        self.choice(channel, root)
            .unwrap_or_else(|| self.participation.contains(&follow_key(channel, root)))
    }

    /// Saves an explicit choice as the newest entry and drops the oldest
    /// past the cap. Returns the channels whose choices changed, including
    /// those of dropped entries, so their unread state can be recounted.
    pub fn follow(&mut self, channel: &str, root: &EventId, following: bool) -> Vec<String> {
        let key = follow_key(channel, root);
        self.choices.retain(|(saved, _)| *saved != key);
        self.choices.push((key, following));
        let mut changed = vec![channel.to_string()];
        let excess = self.choices.len().saturating_sub(THREAD_FOLLOW_LIMIT);
        for (oldest, _) in self.choices.drain(..excess) {
            let evicted = oldest.split(':').next().unwrap_or_default().to_string();
            if !changed.contains(&evicted) {
                changed.push(evicted);
            }
        }
        changed
    }

    /// [`follow`](Self::follow) for a root given as text, which must be 64
    /// hex digits (any case) as in the reference.
    pub fn follow_hex(
        &mut self,
        channel: &str,
        root: &str,
        following: bool,
    ) -> Result<Vec<String>, InvalidThread> {
        let lower = root.to_ascii_lowercase();
        if !is_hex64(&lower) {
            return Err(InvalidThread);
        }
        let root = EventId::from_hex(&lower).map_err(|_| InvalidThread)?;
        Ok(self.follow(channel, &root, following))
    }

    /// Records participation from the viewer's own content events: a
    /// message or reply in a channel follows its thread (the message itself
    /// when it is top-level). Returns whether anything new was recorded.
    pub fn observe(&mut self, events: &[Verified], viewer: &PublicKey) -> bool {
        let mut changed = false;
        for event in events {
            if event.pubkey != *viewer || !super::window::CONTENT_KINDS.contains(&event.kind_u16())
            {
                continue;
            }
            let Some(channel) = event.channel() else {
                continue;
            };
            let root = thread_reference(event).map_or(event.id, |reference| reference.root_id);
            changed |= self.participation.insert(follow_key(channel, &root));
        }
        changed
    }

    /// Replaces participation wholesale, for an owner that rebuilds it
    /// from retained evidence (for example after the viewer's own reply is
    /// deleted). Keys are [`follow_key`]s.
    pub fn set_participation(&mut self, keys: impl IntoIterator<Item = String>) {
        self.participation = keys.into_iter().collect();
    }

    /// Explicit choices, oldest first.
    pub fn choices(&self) -> impl Iterator<Item = (&str, bool)> {
        self.choices
            .iter()
            .map(|(key, following)| (key.as_str(), *following))
    }

    /// Drops everything, for leaving the community.
    pub fn clear(&mut self) {
        self.choices.clear();
        self.participation.clear();
    }
}

impl Serialize for ThreadFollows {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.choices.len()))?;
        for (key, following) in &self.choices {
            seq.serialize_element(&(key, following))?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for ThreadFollows {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> Visitor<'de> for Entries {
            type Value = ThreadFollows;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array of [key, boolean] pairs")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ThreadFollows, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = seq.next_element::<Value>()? {
                    if let Value::Array(pair) = entry
                        && let [Value::String(key), Value::Bool(following), ..] = pair.as_slice()
                    {
                        entries.push((key.clone(), *following));
                    }
                }
                // The newest entries, then a JS `Map`: a repeated key keeps
                // its first position and takes its last value.
                let excess = entries.len().saturating_sub(THREAD_FOLLOW_LIMIT);
                let mut follows = ThreadFollows::default();
                for (key, following) in entries.drain(excess..) {
                    match follows.choices.iter_mut().find(|(saved, _)| *saved == key) {
                        Some(saved) => saved.1 = following,
                        None => follows.choices.push((key, following)),
                    }
                }
                Ok(follows)
            }
        }
        deserializer.deserialize_seq(Entries)
    }
}
