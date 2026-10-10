//! `@` completion (`MentionCompletion.tsx`, `use-mention-choices.ts`,
//! `useMentionDirectory.ts`): the channel's members, the viewer's agents
//! and people a community directory search finds, labelled by the
//! contextual name policy and ordered by the mention rules.

use super::mention_rules::{self, Choice, Presence};
use crate::ui::identity::{self, AgentLibrary};
use buzz_relay_client::{ChannelSummary, ChannelType, PresenceState, Profiles};
use std::collections::{HashMap, HashSet, VecDeque};

/// Rows shown at most ("Narrow your search to see more members.").
pub const LIMIT: usize = 50;

/// A person row.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub pubkey: String,
    /// The identity's own name, which a pick inserts.
    pub name: String,
    /// The label shown, with any namesake qualifier.
    pub label: String,
    pub picture: Option<String>,
    pub agent: bool,
    pub member: bool,
}

/// The directory search for the current query.
pub enum Directory<'a> {
    /// No search applies (an empty query, or a session).
    Idle,
    /// Waiting or running; the last answer's people still show.
    Loading(&'a [String]),
    Done {
        people: &'a [String],
        more: bool,
    },
    Failed,
}

/// What the popup shows.
pub struct Completion {
    pub people: Vec<Person>,
    pub status: Option<&'static str>,
    pub retry: bool,
    /// The one person Space selects, when the choice set is complete.
    pub space: Option<String>,
    /// A multi-word query that continues no known name is prose.
    pub closed: bool,
}

pub struct Inputs<'a> {
    pub channel: Option<&'a ChannelSummary>,
    pub roster_ready: bool,
    pub profiles: &'a Profiles,
    pub library: &'a AgentLibrary,
    pub viewer: &'a str,
    pub origin: Option<&'a buzz_relay_client::RelayOrigin>,
    pub presence: &'a HashMap<String, PresenceState>,
    pub history: &'a HashMap<String, u64>,
}

/// Whether people outside the channel can be found: not in sessions.
pub fn searches_directory(channel: Option<&ChannelSummary>) -> bool {
    channel.is_none_or(|c| c.channel_type != Some(ChannelType::Session))
}

/// The `@query` popup (`MentionCompletion`).
pub fn complete(inputs: &Inputs, query: &str, directory: Directory) -> Completion {
    let channel = inputs.channel;
    // Archived or read-only channels have no choices.
    if channel.is_some_and(|c| c.archived || (c.read_only && !c.cached)) {
        return Completion {
            people: Vec::new(),
            status: None,
            retry: false,
            space: None,
            closed: true,
        };
    }
    let members: Vec<String> = channel.map(|c| c.members.clone()).unwrap_or_default();
    let member_set: HashSet<&str> = members.iter().map(String::as_str).collect();
    let mut keys: Vec<String> = members.clone();
    let mut seen: HashSet<String> = keys.iter().cloned().collect();
    for (key, _, _) in &inputs.library.agents {
        if seen.insert(key.clone()) {
            keys.push(key.clone());
        }
    }
    let (found, searching, failed, more) = match directory {
        Directory::Idle => (&[][..], false, false, false),
        Directory::Loading(people) => (people, true, false, false),
        Directory::Done { people, more } => (people, false, false, more),
        Directory::Failed => (&[][..], false, true, false),
    };
    for key in found {
        if seen.insert(key.clone()) {
            keys.push(key.clone());
        }
    }
    // Labels are resolved against the whole choice set.
    let names = identity::names(
        inputs.profiles,
        inputs.library,
        inputs.viewer,
        inputs.origin,
        Some(&keys),
    );
    let library: HashMap<&str, &str> = inputs
        .library
        .agents
        .iter()
        .map(|(key, name, _)| (key.as_str(), name.as_str()))
        .collect();
    let choices: Vec<Choice> = keys
        .iter()
        .filter(|key| key.len() == 64)
        .map(|key| {
            let profile = inputs.profiles.get(key.as_str());
            let mut aliases = Vec::new();
            if let Some(profile) =
                profile.filter(|p| !p.name_is_fallback && !p.name.trim().is_empty())
            {
                aliases.push(profile.name.clone());
            }
            if let Some(name) = library.get(key.as_str())
                && !aliases.iter().any(|a| a == name)
            {
                aliases.push(name.to_string());
            }
            let label = names.display(key);
            let owned = names.is_library_agent(key)
                || profile.is_some_and(|p| p.attested_owner.as_deref() == Some(inputs.viewer));
            Choice {
                key: key.clone(),
                name: aliases.first().cloned().unwrap_or_else(|| label.clone()),
                label,
                aliases,
                member: member_set.contains(key.as_str()),
                agent: names.is_agent(key),
                owned,
                managed: names.is_library_agent(key),
            }
        })
        .collect();
    let all_names: Vec<&str> = choices
        .iter()
        .flat_map(|c| {
            c.aliases
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(c.label.as_str()))
        })
        .collect();
    let admitted = mention_rules::admitted(query, &all_names);
    if !admitted && !searching && !failed && inputs.roster_ready {
        return Completion {
            people: Vec::new(),
            status: None,
            retry: false,
            space: None,
            closed: true,
        };
    }
    let ranked = if admitted {
        mention_rules::rank(&choices, query, inputs.history, |key| {
            match inputs.presence.get(key) {
                Some(PresenceState::Online) => Presence::Online,
                Some(PresenceState::Away) => Presence::Away,
                _ => Presence::Unknown,
            }
        })
    } else {
        Vec::new()
    };
    let truncated = ranked.len() > LIMIT;
    let people: Vec<Person> = ranked
        .into_iter()
        .take(LIMIT)
        .map(|choice| Person {
            pubkey: choice.key.clone(),
            name: choice.name.clone(),
            label: choice.label.clone(),
            picture: inputs
                .profiles
                .get(choice.key.as_str())
                .and_then(|p| p.picture.clone()),
            agent: choice.agent,
            member: choice.member,
        })
        .collect();
    let complete = !searching && !failed && !more;
    let space = complete
        .then(|| mention_rules::exact_mention(&choices, query))
        .flatten()
        .filter(|choice| people.iter().any(|p| p.pubkey == choice.key))
        .map(|choice| choice.key.clone());
    let missing_members = channel.is_some_and(|c| c.members.is_empty());
    let status = if !inputs.roster_ready {
        Some("Loading recipients…")
    } else if failed {
        Some("Could not search community people. Retry to refresh.")
    } else if !admitted {
        None
    } else if missing_members {
        Some("Channel membership unavailable.")
    } else if searching {
        Some("Searching community…")
    } else if more || truncated {
        Some("Narrow your search to see more members.")
    } else {
        None
    };
    Completion {
        people,
        status,
        retry: failed || (admitted && missing_members),
        space,
        closed: false,
    }
}

/// Explicit picks per channel, newest highest, for 100 channels of 100
/// keys each (`mention-history.ts`).
#[derive(Default)]
pub struct History {
    channels: VecDeque<(String, HashMap<String, u64>)>,
    tick: u64,
}

impl History {
    pub fn get(&self, channel: &str) -> HashMap<String, u64> {
        self.channels
            .iter()
            .find(|(id, _)| id == channel)
            .map(|(_, keys)| keys.clone())
            .unwrap_or_default()
    }

    pub fn remember(&mut self, channel: &str, key: &str) {
        self.tick += 1;
        let index = self.channels.iter().position(|(id, _)| id == channel);
        let mut entry = match index {
            Some(index) => self.channels.remove(index).expect("index is in range"),
            None => (channel.to_string(), HashMap::new()),
        };
        entry.1.insert(key.to_string(), self.tick);
        if entry.1.len() > 100
            && let Some(oldest) = entry
                .1
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(k, _)| k.clone())
        {
            entry.1.remove(&oldest);
        }
        self.channels.push_back(entry);
        if self.channels.len() > 100 {
            self.channels.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_relay_client::Profile;
    use core::prelude::v1::test;
    use std::sync::Arc;

    fn key(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn profiles(names: &[(String, &str)]) -> Profiles {
        Arc::new(
            names
                .iter()
                .map(|(k, n)| {
                    (
                        k.clone(),
                        Arc::new(Profile {
                            name: n.to_string(),
                            ..Default::default()
                        }),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn members_first_then_found_people_and_space_needs_a_complete_set() {
        let (alice, alicia, bob) = (key(1), key(2), key(3));
        let profiles = profiles(&[
            (alice.clone(), "Alice"),
            (alicia.clone(), "Alicia"),
            (bob.clone(), "Bob"),
        ]);
        let channel = ChannelSummary {
            id: "c".into(),
            channel_type: Some(ChannelType::Stream),
            members: vec![bob.clone(), alicia.clone()],
            ..Default::default()
        };
        let library = AgentLibrary::default();
        let presence = HashMap::new();
        let history = HashMap::new();
        let inputs = Inputs {
            channel: Some(&channel),
            roster_ready: true,
            profiles: &profiles,
            library: &library,
            viewer: &bob,
            origin: None,
            presence: &presence,
            history: &history,
        };
        let found = vec![alice.clone()];
        let done = complete(
            &inputs,
            "ali",
            Directory::Done {
                people: &found,
                more: false,
            },
        );
        let order: Vec<&str> = done.people.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(order, ["Alicia", "Alice"]);
        assert!(done.people[0].member && !done.people[1].member);
        assert_eq!(done.status, None);
        // "alice" is exact and unique once the search is in.
        assert_eq!(
            complete(
                &inputs,
                "alice",
                Directory::Done {
                    people: &found,
                    more: false
                }
            )
            .space,
            Some(alice.clone())
        );
        let loading = complete(&inputs, "alice", Directory::Loading(&found));
        assert_eq!(
            (loading.space, loading.status),
            (None, Some("Searching community…"))
        );
        let failed = complete(&inputs, "al", Directory::Failed);
        assert!(failed.retry);
        assert_eq!(
            failed.status,
            Some("Could not search community people. Retry to refresh.")
        );
        // Prose after a complete name closes the popup.
        assert!(
            complete(
                &inputs,
                "Bob is ",
                Directory::Done {
                    people: &[],
                    more: false
                }
            )
            .closed
        );
    }

    #[test]
    fn history_keeps_recent_picks_per_channel() {
        let mut history = History::default();
        history.remember("c", "a");
        history.remember("c", "b");
        let picks = history.get("c");
        assert!(picks["b"] > picks["a"]);
        assert!(history.get("other").is_empty());
    }
}
