//! Display names for people and agents (`identity-names/policy.ts`,
//! `identity-names/directory.ts`): public profile names, with the viewer's
//! own agents named from their library, and namesakes inside one channel
//! told apart by owner, "(agent)", or an npub suffix.
//!
//! Names are display only; they never select recipients or grant control.

use crate::hosts::Hosts;
use buzz_relay_client::{Profiles, RelayOrigin};
use gpui_kit::App;
use nostr::nips::nip19::ToBech32;
use nostr::prelude::PublicKey;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// One identity as the naming policy sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NamingIdentity {
    pub pubkey: String,
    pub name: String,
    pub is_agent: bool,
    pub owner: Option<String>,
}

/// A resolved label and the npub suffix that made it unique, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub qualifier: Option<String>,
}

/// `resolveIdentityNames`: every displayed alias compared across distinct
/// keys. A namesake keeps its name when it outranks the others (the viewer,
/// then people, then the viewer's agents, then other agents); the rest are
/// qualified by owner ("Ann’s Helper") or "(agent)", then by a growing npub
/// suffix.
pub fn resolve(
    identities: &[NamingIdentity],
    viewer: Option<&str>,
    candidates: Option<&[String]>,
) -> HashMap<String, Label> {
    let viewer = viewer.map(str::to_ascii_lowercase);
    let mut unique: HashMap<String, NamingIdentity> = HashMap::new();
    for identity in identities {
        let mut row = identity.clone();
        row.owner = row.owner.map(|o| o.to_ascii_lowercase());
        unique.insert(identity.pubkey.to_ascii_lowercase(), row);
    }
    let selected: Option<HashSet<String>> =
        candidates.map(|keys| keys.iter().map(|k| k.to_ascii_lowercase()).collect());
    // Repeated configurations of one key are not competing identities: the
    // last alias for a (key, name) pair wins, in first-seen order.
    let mut order: Vec<(String, String)> = Vec::new();
    let mut aliases: HashMap<(String, String), NamingIdentity> = HashMap::new();
    for identity in identities {
        let key = (
            identity.pubkey.to_ascii_lowercase(),
            identity.name.trim().to_string(),
        );
        let mut row = identity.clone();
        row.owner = row.owner.map(|o| o.to_ascii_lowercase());
        if aliases.insert(key.clone(), row).is_none() {
            order.push(key);
        }
    }
    struct Row {
        key: String,
        identity: NamingIdentity,
        base: String,
        label: String,
        priority: u8,
        mine: bool,
        length: usize,
        suffix: Option<String>,
    }
    let mut rows: Vec<Row> = order
        .into_iter()
        .filter_map(|alias| aliases.remove(&alias))
        .filter(|identity| {
            selected
                .as_ref()
                .is_none_or(|s| s.contains(&identity.pubkey.to_ascii_lowercase()))
        })
        .map(|identity| {
            let key = identity.pubkey.to_ascii_lowercase();
            let mine = viewer
                .as_ref()
                .is_some_and(|v| *v == key || identity.owner.as_ref() == Some(v));
            let priority = match (identity.is_agent, mine) {
                (true, true) => 2,
                (true, false) => 3,
                (false, _) if viewer.as_deref() == Some(key.as_str()) => 0,
                (false, _) => 1,
            };
            let name = identity.name.trim().to_string();
            Row {
                key,
                identity,
                base: name.clone(),
                label: name,
                priority,
                mine,
                length: 0,
                suffix: None,
            }
        })
        .collect();
    loop {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            match groups.iter_mut().find(|(label, _)| *label == row.label) {
                Some((_, members)) => members.push(index),
                None => groups.push((row.label.clone(), vec![index])),
            }
        }
        let collisions: Vec<Vec<usize>> = groups
            .into_iter()
            .map(|(_, members)| members)
            .filter(|members| {
                let keys: HashSet<&str> = members.iter().map(|&i| rows[i].key.as_str()).collect();
                keys.len() > 1
            })
            .collect();
        if collisions.is_empty() {
            break;
        }
        for group in collisions {
            let best = group.iter().map(|&i| rows[i].priority).min().unwrap_or(0);
            let winners: HashSet<String> = group
                .iter()
                .filter(|&&i| rows[i].priority == best)
                .map(|&i| rows[i].key.clone())
                .collect();
            let changing: Vec<usize> = group
                .iter()
                .copied()
                .filter(|&i| winners.len() != 1 || !winners.contains(&rows[i].key))
                .collect();
            let any_person = group.iter().any(|&i| !rows[i].identity.is_agent);
            let mut qualified = false;
            for &i in &changing {
                let row = &rows[i];
                let owner = row
                    .identity
                    .owner
                    .as_ref()
                    .and_then(|owner| unique.get(owner))
                    .map(|owner| owner.name.trim().to_string());
                let name = row.identity.name.trim();
                let readable = if row.identity.is_agent && row.length == 0 {
                    match owner {
                        Some(owner) if !row.mine => format!("{owner}’s {name}"),
                        _ if any_person => format!("{name} (agent)"),
                        _ => row.base.clone(),
                    }
                } else {
                    row.base.clone()
                };
                if readable != row.base {
                    let row = &mut rows[i];
                    row.base = readable.clone();
                    row.label = readable;
                    qualified = true;
                }
            }
            if qualified {
                continue;
            }
            for &i in &changing {
                let row = &mut rows[i];
                row.length = if row.length == 0 { 4 } else { row.length + 1 };
                let npub = npub(&row.key);
                let count = npub.chars().count();
                let suffix = if row.length <= count {
                    npub.chars().skip(count - row.length).collect()
                } else {
                    format!("{npub} · {}", row.length - count)
                };
                row.label = format!("{} · {suffix}", row.base);
                row.suffix = Some(suffix);
            }
        }
    }
    rows.into_iter()
        .filter(|row| {
            unique
                .get(&row.key)
                .is_some_and(|u| u.name.trim() == row.identity.name.trim())
        })
        .map(|row| {
            (
                row.key,
                Label {
                    name: row.label,
                    qualifier: row.suffix,
                },
            )
        })
        .collect()
}

fn npub(hex: &str) -> String {
    PublicKey::from_hex(hex)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .unwrap_or_else(|| hex.to_string())
}

/// The viewer's agents, as every connected host lists them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentLibrary {
    /// `(pubkey, name, relay url)` in host order.
    pub agents: Vec<(String, String, String)>,
}

impl AgentLibrary {
    pub fn from_hosts(cx: &App) -> Self {
        let Some(hosts) = cx.try_global::<Hosts>() else {
            return Self::default();
        };
        let agents = hosts
            .hosts
            .iter()
            .flat_map(|host| host.read(cx).agents.agents.iter())
            .map(|agent| {
                (
                    agent.pubkey.to_ascii_lowercase(),
                    agent.name.clone(),
                    agent.relay_url.clone(),
                )
            })
            .collect();
        Self { agents }
    }
}

/// Names for one view: one channel's members, or the whole community.
pub struct Names {
    labels: HashMap<String, Label>,
    /// Library names for keys outside the candidate scope.
    library: HashMap<String, String>,
    agents: HashSet<String>,
    profiles: Profiles,
    viewer: String,
}

impl Names {
    /// The display name for `pubkey`, or `fallback`.
    pub fn name(&self, pubkey: &str, fallback: &str) -> String {
        let key = pubkey.to_ascii_lowercase();
        if let Some(label) = self.labels.get(&key).filter(|l| !l.name.is_empty()) {
            return label.name.clone();
        }
        if let Some(name) = self.library.get(&key) {
            return name.clone();
        }
        self.profiles
            .get(&key)
            .map(|profile| profile.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback.to_string())
    }

    /// `profile?.name ?? pubkey.slice(0, 10)` through the policy.
    pub fn display(&self, pubkey: &str) -> String {
        let fallback: String = pubkey.chars().take(10).collect();
        self.name(pubkey, &fallback)
    }

    /// One of the viewer's own agents (`agentPubkeys`).
    pub fn is_library_agent(&self, pubkey: &str) -> bool {
        self.agents.contains(&pubkey.to_ascii_lowercase())
    }

    /// An agent by its envelope, library or signed profile.
    pub fn is_agent(&self, pubkey: &str) -> bool {
        self.is_library_agent(pubkey) || self.profiles.get(pubkey).is_some_and(|p| p.is_agent)
    }

    pub fn profiles(&self) -> &Profiles {
        &self.profiles
    }

    pub fn viewer(&self) -> &str {
        &self.viewer
    }

    /// Library names of the viewer's agents, which also bind mentions.
    pub fn agent_names(&self) -> impl Iterator<Item = (&str, &str)> {
        self.library.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// `createNameProvider`'s identities: every profile, with the viewer's
/// agents named from a signed agent profile, else their library alias,
/// and same-community host settings winning.
pub fn identities(
    profiles: &Profiles,
    library: &AgentLibrary,
    origin: Option<&RelayOrigin>,
) -> (Vec<NamingIdentity>, HashMap<String, String>) {
    let mut names: HashMap<String, String> = HashMap::new();
    for (key, name, _) in &library.agents {
        if names.contains_key(key) {
            continue;
        }
        let profile = profiles.get(key);
        let profile_name = profile.map(|p| p.name.trim().to_string());
        let signed = profile.is_some_and(|p| p.is_agent && !p.name_is_fallback);
        let chosen = match profile_name.filter(|n| !n.is_empty()) {
            Some(profile_name) if signed => profile_name,
            profile_name => Some(name.trim().to_string())
                .filter(|n| !n.is_empty())
                .or(profile_name)
                .unwrap_or_else(|| "Agent".into()),
        };
        names.insert(key.clone(), chosen);
    }
    if let Some(origin) = origin {
        let mut seen = HashSet::new();
        for (key, name, relay) in &library.agents {
            let same = RelayOrigin::parse(relay).is_ok_and(|other| other == *origin);
            if !seen.insert(key.clone()) || !same {
                continue;
            }
            if !name.trim().is_empty() {
                names.insert(key.clone(), name.trim().to_string());
            }
        }
    }
    let mut identities: Vec<NamingIdentity> = profiles
        .iter()
        .filter(|(key, _)| !names.contains_key(key.as_str()))
        .map(|(key, profile)| NamingIdentity {
            pubkey: key.clone(),
            name: profile.name.clone(),
            is_agent: profile.is_agent,
            owner: profile.owner_pubkey.clone(),
        })
        .collect();
    // Stable order: the policy's result does not depend on it, but ties in
    // label groups are compared in order.
    identities.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
    let mut agents: Vec<(&String, &String)> = names.iter().collect();
    agents.sort();
    for (key, name) in agents {
        identities.push(NamingIdentity {
            pubkey: key.clone(),
            name: name.clone(),
            is_agent: true,
            owner: profiles.get(key).and_then(|p| p.owner_pubkey.clone()),
        });
    }
    (identities, names)
}

thread_local! {
    static CACHE: RefCell<Vec<(CacheKey, Arc<Names>)>> = const { RefCell::new(Vec::new()) };
}

#[derive(PartialEq)]
struct CacheKey {
    profiles: usize,
    library: AgentLibrary,
    viewer: String,
    origin: Option<String>,
    members: Option<Vec<String>>,
}

/// Names scoped to `members` (a channel's roster), or community-wide.
/// Cached per inputs, so rows share one resolution per render.
pub fn names(
    profiles: &Profiles,
    library: &AgentLibrary,
    viewer: &str,
    origin: Option<&RelayOrigin>,
    members: Option<&[String]>,
) -> Arc<Names> {
    let key = CacheKey {
        profiles: Arc::as_ptr(profiles) as *const () as usize,
        library: library.clone(),
        viewer: viewer.to_string(),
        origin: origin.map(|o| o.to_string()),
        members: members.map(|m| {
            let mut m: Vec<String> = m.iter().map(|k| k.to_ascii_lowercase()).collect();
            m.sort();
            m
        }),
    };
    if let Some(found) = CACHE.with_borrow(|cache| {
        cache
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, names)| names.clone())
    }) {
        return found;
    }
    let (identities, library_names) = identities(profiles, library, origin);
    let labels = resolve(&identities, Some(viewer), key.members.as_deref());
    let names = Arc::new(Names {
        labels,
        library: library_names,
        agents: library.agents.iter().map(|(k, _, _)| k.clone()).collect(),
        profiles: profiles.clone(),
        viewer: viewer.to_ascii_lowercase(),
    });
    CACHE.with_borrow_mut(|cache| {
        if cache.len() >= 8 {
            cache.remove(0);
        }
        cache.push((key, names.clone()));
    });
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn id(pubkey: &str, name: &str, is_agent: bool, owner: Option<&str>) -> NamingIdentity {
        NamingIdentity {
            pubkey: pubkey.into(),
            name: name.into(),
            is_agent,
            owner: owner.map(Into::into),
        }
    }

    fn key(byte: u8) -> String {
        buzz_identity::Identity::ephemeral()
            .public_key()
            .to_hex()
            .chars()
            .take(62)
            .collect::<String>()
            + &format!("{byte:02x}")
    }

    #[test]
    fn people_keep_names_and_agents_are_qualified() {
        let (ann, bot, viewer) = (key(1), key(2), key(3));
        let identities = [
            id(&ann, "Ann", false, None),
            id(&bot, "Helper", true, Some(&ann)),
            id(&viewer, "Helper", false, None),
        ];
        let labels = resolve(&identities, Some(&viewer), None);
        assert_eq!(labels[&viewer].name, "Helper");
        assert_eq!(labels[&bot].name, "Ann’s Helper");
        assert_eq!(labels[&ann].name, "Ann");
    }

    #[test]
    fn agents_without_owner_say_agent_then_suffix() {
        let (person, agent) = (key(4), key(5));
        let labels = resolve(
            &[
                id(&person, "Sam", false, None),
                id(&agent, "Sam", true, None),
            ],
            None,
            None,
        );
        assert_eq!(labels[&person].name, "Sam");
        assert_eq!(labels[&agent].name, "Sam (agent)");
        // Two people with one name both take suffixes.
        let (a, b) = (key(6), key(7));
        let labels = resolve(
            &[id(&a, "Kim", false, None), id(&b, "Kim", false, None)],
            None,
            None,
        );
        let suffix = labels[&a].qualifier.clone().unwrap();
        assert_eq!(suffix.len(), 4);
        assert_eq!(labels[&a].name, format!("Kim · {suffix}"));
        assert!(npub(&a).ends_with(&suffix));
        assert_ne!(labels[&a].name, labels[&b].name);
    }

    #[test]
    fn candidates_limit_the_collision_scope() {
        let (a, b) = (key(8), key(9));
        let identities = [id(&a, "Lee", false, None), id(&b, "Lee", false, None)];
        let labels = resolve(&identities, None, Some(std::slice::from_ref(&a)));
        assert_eq!(labels[&a].name, "Lee");
        assert!(!labels.contains_key(&b));
    }
}
