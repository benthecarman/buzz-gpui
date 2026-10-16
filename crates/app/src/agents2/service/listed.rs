//! Keeps the owner's agent list (kind 30177) in each bound community in
//! line with the agents here, and shows the agents it lists that run on
//! another of the owner's devices (buzz-app's `readListings`, `list`,
//! `listedAgents`).

use super::super::listings::{self, Elsewhere, Listing};
use super::{Agents2, http_origin, selected_origin};
use crate::runtime::Runtime;
use buzz_relay_client::RelayHttp;
use gpui_kit::{App, Context};
use std::collections::{HashMap, HashSet};

/// The listing state of the bound communities, by origin. A binding
/// replaced or let go drops its community's state, so nothing written
/// for an old session lands in a new one.
#[derive(Default)]
pub(super) struct Listed {
    /// The viewer's records in each community, as last read and written
    /// since.
    rows: HashMap<String, Vec<Listing>>,
    /// By origin until its records are first read: the listings written
    /// or removed meanwhile, which the read must not undo.
    edits: HashMap<String, HashMap<String, Option<Listing>>>,
    /// By origin, the binding generation whose read is under way.
    reading: HashMap<String, u64>,
    /// Agents whose record is being written, so a sync writes each once.
    writing: HashSet<String>,
}

impl Listed {
    pub(super) fn forget(&mut self, origin: &str) {
        self.rows.remove(origin);
        self.edits.remove(origin);
        self.reading.remove(origin);
    }

    /// The community's listings: as read, or until the first read
    /// finishes, those written since.
    fn rows_of(&self, origin: &str) -> Vec<Listing> {
        self.rows.get(origin).cloned().unwrap_or_else(|| {
            self.edits
                .get(origin)
                .map(|edits| edits.values().flatten().cloned().collect())
                .unwrap_or_default()
        })
    }

    /// Replaces or removes the agent's listing, and keeps the change until
    /// the community's records are first read.
    fn set_row(&mut self, origin: &str, pubkey: &str, row: Option<Listing>) {
        let Some(rows) = self.rows.get_mut(origin) else {
            self.edits
                .entry(origin.to_owned())
                .or_default()
                .insert(pubkey.to_owned(), row);
            return;
        };
        rows.retain(|entry| entry.pubkey != pubkey);
        rows.extend(row);
    }
}

impl Agents2 {
    /// Reads each bound community's records once, then lists each agent
    /// here that they lack. A failed read is asked for again on the next
    /// sync.
    pub(super) fn sync_listings(&mut self, cx: &mut Context<Self>) {
        let origins: Vec<String> = self
            .bound
            .iter()
            .filter(|(_, bound)| bound.binding.is_some())
            .map(|(origin, _)| origin.clone())
            .collect();
        for origin in origins {
            if self.listed.rows.contains_key(&origin) {
                self.list(&origin, cx);
            } else {
                self.read_listings(&origin, cx);
            }
        }
    }

    fn read_listings(&mut self, origin: &str, cx: &mut Context<Self>) {
        let Some(binding) = self.bound.get(origin).and_then(|b| b.binding.as_ref()) else {
            return;
        };
        let generation = binding.generation;
        if self.listed.reading.get(origin) == Some(&generation) {
            return;
        }
        self.listed.reading.insert(origin.to_owned(), generation);
        let http = RelayHttp::new(binding.session.origin().clone(), binding.identity.clone());
        let viewer = binding.viewer.clone();
        let read = Runtime::handle(cx).spawn(async move { listings::read(&http, &viewer).await });
        let origin = origin.to_owned();
        cx.spawn(async move |this, cx| {
            let read = read
                .await
                .unwrap_or_else(|_| Err("The agent list read was cancelled".into()));
            let _ = this.update(cx, |this, cx| {
                if this.listed.reading.get(&origin) != Some(&generation) {
                    return;
                }
                this.listed.reading.remove(&origin);
                let read = match read {
                    Ok(read) => read,
                    // Its edits stay for the next read, which a sync asks
                    // for.
                    Err(error) => {
                        return tracing::warn!(%error, "The agent list could not be read");
                    }
                };
                let edits = this.listed.edits.remove(&origin).unwrap_or_default();
                let mut rows: Vec<Listing> = read
                    .into_iter()
                    .filter(|row| !edits.contains_key(&row.pubkey))
                    .collect();
                rows.extend(edits.into_values().flatten());
                this.listed.rows.insert(origin.clone(), rows);
                cx.notify();
                this.list(&origin, cx);
            });
        })
        .detach();
    }

    /// Lists each agent here in the community whose record is missing or
    /// shows another name, picture or type. Waits for the first read, so
    /// it never replaces a record it has not seen.
    fn list(&mut self, origin: &str, cx: &mut Context<Self>) {
        let Some(rows) = self.listed.rows.get(origin) else {
            return;
        };
        let Some(binding) = self.bound.get(origin).and_then(|b| b.binding.as_ref()) else {
            return;
        };
        let mut writes = Vec::new();
        for saved in self.saved() {
            let identity = saved.summary;
            if identity.deleted
                || identity.owner != binding.viewer
                || http_origin(&identity.relay).as_deref() != Some(origin)
                || self.listed.writing.contains(&identity.pubkey)
            {
                continue;
            }
            let picture = listings::record_picture(identity.picture.as_deref());
            let row = rows.iter().find(|row| row.pubkey == identity.pubkey);
            if row.is_some_and(|row| {
                row.name == identity.name
                    && row.picture == picture
                    && row.runtime.as_deref() == Some(identity.agent_type.as_str())
            }) {
                continue;
            }
            writes.push((
                Listing {
                    pubkey: identity.pubkey,
                    name: identity.name,
                    picture,
                    runtime: Some(identity.agent_type.clone()),
                    prior: row.and_then(|row| row.prior.clone()),
                },
                identity.agent_type,
            ));
        }
        let generation = binding.generation;
        let (session, owner) = (binding.session.clone(), binding.identity.clone());
        for (entry, runtime) in writes {
            self.listed.writing.insert(entry.pubkey.clone());
            let http = RelayHttp::new(session.origin().clone(), owner.clone());
            let owner = owner.clone();
            let write = {
                let entry = entry.clone();
                Runtime::handle(cx)
                    .spawn(async move { listings::publish(&http, &owner, &entry, &runtime).await })
            };
            let origin = origin.to_owned();
            cx.spawn(async move |this, cx| {
                let written = write
                    .await
                    .unwrap_or_else(|_| Err("The agent list was not updated. Retry.".into()));
                let _ = this.update(cx, |this, cx| {
                    this.listed.writing.remove(&entry.pubkey);
                    let current = this
                        .bound
                        .get(&origin)
                        .and_then(|b| b.binding.as_ref())
                        .is_some_and(|binding| binding.generation == generation);
                    match written {
                        Ok(prior) if current => {
                            let pubkey = entry.pubkey.clone();
                            let row = Listing {
                                prior: Some(prior),
                                ..entry
                            };
                            this.listed.set_row(&origin, &pubkey, Some(row));
                            cx.notify();
                            // Lists a rename or new picture made while this
                            // write was under way.
                            this.list(&origin, cx);
                        }
                        Ok(_) => {}
                        Err(error) if current => {
                            tracing::warn!(agent = %entry.name, %error, "Agent was not listed");
                        }
                        Err(_) => {}
                    }
                });
            })
            .detach();
        }
    }

    /// Takes a deleted agent out of every community's listings.
    pub(super) fn unlist(&mut self, pubkey: &str) {
        let origins: Vec<String> = self.listed.rows.keys().cloned().collect();
        for origin in origins {
            self.listed.set_row(&origin, pubkey, None);
        }
    }

    /// The selected community's agents that are not here: ones on the
    /// viewer's other devices. Never run here, and not deleted from here.
    pub fn elsewhere(&self, cx: &App) -> Vec<Elsewhere> {
        let Some(origin) = selected_origin(cx) else {
            return Vec::new();
        };
        let Some(binding) = self.bound.get(&origin).and_then(|b| b.binding.as_ref()) else {
            return Vec::new();
        };
        let here: HashSet<String> = self.saved().map(|saved| saved.summary.pubkey).collect();
        self.listed
            .rows_of(&origin)
            .into_iter()
            .filter(|row| !row.name.is_empty() && !here.contains(&row.pubkey))
            .filter_map(|row| {
                Some(Elsewhere {
                    agent_type: row.runtime?,
                    pubkey: row.pubkey,
                    name: row.name,
                    picture: row.picture,
                    relay: binding.destination.clone(),
                    owner: binding.viewer.clone(),
                })
            })
            .collect()
    }
}
