//! The agent inventory: every identity the viewer's hosts, old Buzz
//! installs and community relays know, joined by public key, and where each
//! belongs on the page. buzz-app's `inventory-model.ts` and
//! `inventory-decisions.ts`.
//!
//! Hosts are an addition: a local setup remembers which host runs it.

use super::relay::LibraryIdentity;
use buzz_host::api::{Agent, AgentsSnapshot, ImportSource};
use std::collections::{BTreeMap, BTreeSet};

/// A community's HTTP origin (`https://host`), as buzz-app's `relayOrigin`.
pub fn origin(url: &str) -> String {
    buzz_relay_client::RelayOrigin::parse(url)
        .map(|origin| origin.http())
        .unwrap_or_default()
}

/// Whether `url` names the community at `community`, however either is
/// written.
pub fn in_community(url: &str, community: &buzz_relay_client::RelayOrigin) -> bool {
    buzz_relay_client::RelayOrigin::parse(url)
        .is_ok_and(|origin| origin.websocket() == community.websocket())
}

/// `npub1…` for a hex key, or the key itself if it is not one.
pub fn npub(pubkey: &str) -> String {
    use nostr::nips::nip19::ToBech32;
    nostr::prelude::PublicKey::from_hex(pubkey)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .unwrap_or_else(|| pubkey.to_string())
}

/// One agent setup on one host. `host` indexes the page's hosts.
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub host: usize,
    pub agent: Agent,
}

/// `AgentInventoryIdentity`.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub pubkey: String,
    pub display_name: String,
    pub avatar: Option<String>,
    pub relay: Option<LibraryIdentity>,
    /// Old Buzz installs that held this identity, by host.
    pub sources: Vec<(usize, ImportSource)>,
    /// The first host record of this identity.
    pub local: Option<(usize, String)>,
    pub known: BTreeSet<String>,
    /// Configured setups by community origin.
    pub setups: BTreeMap<String, Setup>,
    /// Older imports that still need Use here.
    pub unconfigured: Vec<Setup>,
}

impl Row {
    /// Configured setups, this community's first: the card represents it.
    pub fn local_setups(&self, destination: &str) -> Vec<Setup> {
        let here = self.setups.get(destination);
        here.into_iter()
            .chain(self.setups.values().filter(|setup| Some(*setup) != here))
            .cloned()
            .collect()
    }

    /// The old Buzz installations this identity can come from.
    pub fn import_sources(&self) -> Vec<ImportSource> {
        let mut sources: Vec<ImportSource> = Vec::new();
        for (_, source) in &self.sources {
            if !sources.contains(source) {
                sources.push(*source);
            }
        }
        sources
    }
}

/// Joins the sources by key without changing any of them.
pub fn identities(
    relay: &[LibraryIdentity],
    communities: &BTreeMap<String, Vec<String>>,
    hosts: &[&AgentsSnapshot],
    resolve: impl Fn(&str, &str) -> String,
) -> BTreeMap<String, Row> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let entry = |rows: &mut BTreeMap<String, Row>, pubkey: &str, fallback: &str| {
        let key = pubkey.to_lowercase();
        rows.entry(key.clone())
            .or_insert_with(|| Row {
                display_name: resolve(&key, fallback),
                pubkey: key,
                ..Default::default()
            })
            .pubkey
            .clone()
    };
    for metadata in relay {
        let key = entry(&mut rows, &metadata.pubkey, &metadata.name);
        let row = rows.get_mut(&key).expect("just added");
        row.relay = Some(metadata.clone());
        row.avatar = metadata.avatar.clone();
    }
    for (community, keys) in communities {
        for pubkey in keys {
            let fallback: String = pubkey.chars().take(12).collect();
            let key = entry(&mut rows, pubkey, &fallback);
            let row = rows.get_mut(&key).expect("just added");
            row.known.insert(origin(community));
        }
    }
    for (host, snapshot) in hosts.iter().enumerate() {
        for parked in &snapshot.parked {
            let key = entry(&mut rows, &parked.pubkey, &parked.name);
            let row = rows.get_mut(&key).expect("just added");
            row.display_name = resolve(&key, &parked.name);
            for source in &parked.sources {
                if !row.sources.contains(&(host, *source)) {
                    row.sources.push((host, *source));
                }
            }
        }
    }
    for (host, snapshot) in hosts.iter().enumerate() {
        for agent in &snapshot.agents {
            let key = entry(&mut rows, &agent.pubkey, &agent.name);
            let row = rows.get_mut(&key).expect("just added");
            row.local.get_or_insert((host, agent.id.clone()));
            let community = origin(&agent.relay_url);
            if !community.is_empty() {
                row.known.insert(community.clone());
            }
            let setup = Setup {
                host,
                agent: agent.clone(),
            };
            if !agent.configured {
                row.unconfigured.push(setup);
                continue;
            }
            // One setup per identity and community; a second host's copy
            // of the same pair is left out rather than guessed between.
            row.setups.entry(community).or_insert(setup);
        }
    }
    rows
}

/// Where an identity is listed, in page order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// "Individual agents".
    LocalHere,
    LocalOther,
    Import,
    Relay,
}

impl Group {
    pub fn heading(self) -> &'static str {
        match self {
            Group::LocalHere => "Individual agents",
            Group::LocalOther => "Local agents in other communities",
            Group::Import => "Available to import",
            Group::Relay => "Relay-only agents",
        }
    }
}

/// What the card offers for the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A setup here: process controls.
    Controls,
    /// Set up elsewhere: clone it here.
    Clone,
    Import,
    Unavailable,
    /// An older import that needs Use here.
    Use,
    /// Use here once a destination is connected.
    Wait,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub group: Group,
    pub action: Action,
    pub blocked: Option<&'static str>,
}

/// `inventoryDecision`: one identity, first matching section.
pub fn decide(row: &Row, destination: &str) -> Decision {
    let configured_here = row.setups.contains_key(destination);
    let local_here = configured_here
        || row
            .unconfigured
            .iter()
            .any(|setup| !destination.is_empty() && origin(&setup.agent.relay_url) == destination);
    let group = if row.local.is_some() {
        if !destination.is_empty() && local_here {
            Group::LocalHere
        } else {
            Group::LocalOther
        }
    } else if !row.sources.is_empty() {
        Group::Import
    } else {
        Group::Relay
    };
    let action = if !row.setups.is_empty() {
        if configured_here {
            Action::Controls
        } else {
            Action::Clone
        }
    } else if row.local.is_none() {
        if row.sources.is_empty() {
            Action::Unavailable
        } else {
            Action::Import
        }
    } else if !destination.is_empty() {
        Action::Use
    } else {
        Action::Wait
    };
    let blocked = match action {
        Action::Wait => Some("Connect to a destination community to set up this identity."),
        Action::Unavailable => {
            Some("No import source has been confirmed. Check any discovery warnings.")
        }
        _ => None,
    };
    Decision {
        group,
        action,
        blocked,
    }
}

/// Community subsections of a row-list group: one community per identity,
/// the first sorted; identities with none are "Community unknown".
pub fn community_sections(group: Group, rows: Vec<&Row>) -> Vec<(Option<String>, Vec<&Row>)> {
    if !matches!(group, Group::LocalOther | Group::Relay) {
        return vec![(None, rows)];
    }
    let mut sections: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        // A saved setup owns local placement; relay sightings never move it.
        let mut communities: Vec<String> = if group == Group::LocalOther {
            row.setups
                .keys()
                .cloned()
                .chain(
                    row.unconfigured
                        .iter()
                        .map(|setup| origin(&setup.agent.relay_url)),
                )
                .collect()
        } else {
            row.known.iter().cloned().collect()
        };
        communities.retain(|community| !community.is_empty());
        communities.sort();
        sections
            .entry(communities.into_iter().next().unwrap_or_default())
            .or_default()
            .push(row);
    }
    let mut sections: Vec<_> = sections.into_iter().collect();
    // Named communities first, sorted; the unknown one last.
    sections.sort_by(|(a, _), (b, _)| (a.is_empty(), a).cmp(&(b.is_empty(), b)));
    sections
        .into_iter()
        .map(|(community, rows)| {
            (
                Some(if community.is_empty() {
                    "Community unknown".to_string()
                } else {
                    community
                }),
                rows,
            )
        })
        .collect()
}

/// Case-insensitive by name, then by key.
pub fn sort(rows: &mut [&Row]) {
    rows.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then(a.pubkey.cmp(&b.pubkey))
    });
}

/// buzz-app's naming policy for the viewer's own agents: names that collide
/// across different keys get the end of their npub, longer until unique.
pub fn disambiguate(names: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut labels: BTreeMap<String, (String, usize)> = names
        .iter()
        .map(|(key, name)| (key.clone(), (name.trim().to_string(), 0)))
        .collect();
    loop {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (key, (label, _)) in &labels {
            groups.entry(label.clone()).or_default().push(key.clone());
        }
        let collisions: Vec<Vec<String>> =
            groups.into_values().filter(|keys| keys.len() > 1).collect();
        if collisions.is_empty() {
            break;
        }
        for key in collisions.into_iter().flatten() {
            let base = names[&key].trim().to_string();
            let entry = labels.get_mut(&key).expect("listed");
            entry.1 = if entry.1 == 0 { 4 } else { entry.1 + 1 };
            let npub = npub(&key);
            let suffix = if entry.1 <= npub.len() {
                npub[npub.len() - entry.1..].to_string()
            } else {
                format!("{npub} · {}", entry.1 - npub.len())
            };
            entry.0 = format!("{base} · {suffix}");
        }
    }
    labels
        .into_iter()
        .map(|(key, (label, _))| (key, label))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_host::api::ParkedIdentity;

    const A: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const B: &str = "bb00000000000000000000000000000000000000000000000000000000000002";
    const HERE: &str = "https://here.example";

    #[test]
    fn community_matches_however_written() {
        let here = buzz_relay_client::RelayOrigin::parse("wss://here.example").unwrap();
        assert!(in_community("wss://here.example/", &here));
        assert!(in_community(HERE, &here));
        assert!(!in_community("wss://there.example", &here));
        assert!(!in_community("not a url", &here));
    }

    fn agent(id: &str, pubkey: &str, relay: &str, configured: bool) -> Agent {
        Agent {
            id: id.into(),
            pubkey: pubkey.into(),
            relay_url: relay.into(),
            name: id.into(),
            configured,
            ..Default::default()
        }
    }

    fn rows(snapshot: &AgentsSnapshot, relay: &[LibraryIdentity]) -> BTreeMap<String, Row> {
        identities(relay, &BTreeMap::new(), &[snapshot], |_, name| name.into())
    }

    #[test]
    fn identities_land_in_buzz_apps_groups() {
        let snapshot = AgentsSnapshot {
            agents: vec![
                agent("here", A, "wss://here.example", true),
                agent("elsewhere", B, "wss://there.example", true),
            ],
            parked: vec![ParkedIdentity {
                pubkey: "cc".repeat(32),
                name: "Old".into(),
                sources: vec![ImportSource::Installed],
            }],
            ..Default::default()
        };
        let relay = vec![LibraryIdentity {
            pubkey: "dd".repeat(32),
            name: "Relay".into(),
            ..Default::default()
        }];
        let rows = rows(&snapshot, &relay);
        let group = |key: &str| decide(&rows[key], HERE);
        assert_eq!(group(A).group, Group::LocalHere);
        assert_eq!(group(A).action, Action::Controls);
        assert_eq!(group(B).group, Group::LocalOther);
        assert_eq!(group(B).action, Action::Clone);
        assert_eq!(group(&"cc".repeat(32)).group, Group::Import);
        assert_eq!(group(&"cc".repeat(32)).action, Action::Import);
        let relay_only = group(&"dd".repeat(32));
        assert_eq!(
            (relay_only.group, relay_only.action),
            (Group::Relay, Action::Unavailable)
        );
        assert!(relay_only.blocked.is_some());
    }

    #[test]
    fn older_imports_need_use_here() {
        let snapshot = AgentsSnapshot {
            agents: vec![agent("old", A, "wss://here.example", false)],
            ..Default::default()
        };
        let rows = rows(&snapshot, &[]);
        let decision = decide(&rows[A], HERE);
        assert_eq!(
            (decision.group, decision.action),
            (Group::LocalHere, Action::Use)
        );
        let away = decide(&rows[A], "");
        assert_eq!((away.group, away.action), (Group::LocalOther, Action::Wait));
    }

    #[test]
    fn other_communities_are_subsections_with_unknown_last() {
        let snapshot = AgentsSnapshot {
            agents: vec![
                agent("b", B, "wss://b.example", true),
                agent("a", A, "", true),
            ],
            ..Default::default()
        };
        let rows = rows(&snapshot, &[]);
        let list: Vec<&Row> = rows.values().collect();
        let sections = community_sections(Group::LocalOther, list);
        let names: Vec<_> = sections.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names,
            [
                Some("https://b.example".to_string()),
                Some("Community unknown".to_string())
            ]
        );
    }

    #[test]
    fn colliding_names_get_npub_suffixes() {
        let names = BTreeMap::from([
            (A.to_string(), "Scout".to_string()),
            (B.to_string(), "Scout".to_string()),
            ("ee".repeat(32), "Other".to_string()),
        ]);
        let labels = disambiguate(&names);
        assert_ne!(labels[A], labels[B]);
        assert!(labels[A].starts_with("Scout · "));
        assert_eq!(labels[&"ee".repeat(32)], "Other");
    }
}
