//! The relay side of the Agents page: the viewer's agent library, the
//! connected community's identity archive and every membership's agent
//! inventory, read again on Refresh agents. One per page; switching
//! community or identity starts over.

use super::relay::{self, ArchiveSnapshot, CommunityRead, Library};
use crate::community::{AppState, IdentityState};
use crate::hosts::HostConnection;
use crate::runtime::Runtime;
use buzz_host::api::{Request, Response};
use buzz_identity::Identity;
use buzz_relay_client::RelayOrigin;
use gpui_kit::{App, Context, Entity, Task};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadStatus {
    Unavailable,
    Loading,
    Ready,
    Error,
}

/// An archive or unarchive in flight, or its failure.
#[derive(Clone, Debug)]
pub struct ArchiveRun {
    pub archive: bool,
    pub error: Option<String>,
}

/// What a relay-only Remove is doing.
#[derive(Clone, Debug, Default)]
pub struct Removal {
    pub running: bool,
    pub error: Option<String>,
}

/// The viewer and community these reads belong to.
#[derive(Clone, PartialEq)]
struct Scope {
    viewer: String,
    destination: Option<RelayOrigin>,
    communities: Vec<RelayOrigin>,
    authority: Option<String>,
}

pub struct Discovery {
    scope: Option<Scope>,
    identity: Option<Arc<Identity>>,
    pub library: Library,
    pub library_status: ReadStatus,
    pub library_error: Option<String>,
    pub archives: ArchiveSnapshot,
    pub archive_status: ReadStatus,
    /// Archived keys of the last good snapshot, shown while a new read runs.
    last_archived: BTreeSet<String>,
    /// Per membership: its answer in the current read, if any yet.
    pub reads: BTreeMap<String, CommunityRead>,
    /// Identities each community answered with last, kept across reads.
    pub known: BTreeMap<String, Vec<String>>,
    known_profiles: BTreeMap<String, HashMap<String, buzz_relay_client::Profile>>,
    /// Bumped by Refresh agents, to ask for archive consent again.
    pub attempt: u64,
    pub runs: BTreeMap<String, ArchiveRun>,
    /// Who the viewer may archive, as last asked.
    pub consent: BTreeMap<String, bool>,
    pub removals: BTreeMap<String, Removal>,
    /// Removed in this view; hidden for as long as it lives.
    pub removed: BTreeSet<String>,
    generation: u64,
    _tasks: Vec<Task<()>>,
}

impl Discovery {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            scope: None,
            identity: None,
            library: Library::default(),
            library_status: ReadStatus::Unavailable,
            library_error: None,
            archives: ArchiveSnapshot::default(),
            archive_status: ReadStatus::Unavailable,
            last_archived: BTreeSet::new(),
            reads: BTreeMap::new(),
            known: BTreeMap::new(),
            known_profiles: BTreeMap::new(),
            attempt: 0,
            runs: BTreeMap::new(),
            consent: BTreeMap::new(),
            removals: BTreeMap::new(),
            removed: BTreeSet::new(),
            generation: 0,
            _tasks: Vec::new(),
        };
        this.sync(&[], cx);
        this
    }

    fn current_scope(cx: &App) -> (Option<Scope>, Option<Arc<Identity>>) {
        let state = cx.global::<AppState>();
        let IdentityState::Ready(identity) = &state.identity else {
            return (None, None);
        };
        let current = AppState::current(cx);
        let destination = current.as_ref().map(|c| c.read(cx).origin.clone());
        let authority = current.as_ref().and_then(|c| {
            c.read(cx)
                .snapshot
                .relay
                .as_ref()
                .map(|r| r.authority.clone())
        });
        let mut communities: Vec<RelayOrigin> = state
            .communities
            .iter()
            .map(|community| community.read(cx).origin.clone())
            .collect();
        communities.sort_by_key(|origin| origin.http());
        communities.dedup();
        (
            Some(Scope {
                viewer: identity.pubkey_hex(),
                destination,
                communities,
                authority,
            }),
            Some(identity.clone()),
        )
    }

    /// Starts over when the viewer, community or memberships changed.
    pub fn sync(&mut self, hosts: &[Entity<HostConnection>], cx: &mut Context<Self>) {
        let (scope, identity) = Self::current_scope(cx);
        if scope == self.scope {
            return;
        }
        let viewer_changed =
            scope.as_ref().map(|s| &s.viewer) != self.scope.as_ref().map(|s| &s.viewer);
        let community_changed =
            scope.as_ref().map(|s| &s.destination) != self.scope.as_ref().map(|s| &s.destination);
        self.scope = scope;
        self.identity = identity;
        if viewer_changed {
            self.known.clear();
            self.known_profiles.clear();
        }
        if community_changed || viewer_changed {
            self.runs.clear();
            self.consent.clear();
            self.removals.clear();
            self.last_archived.clear();
            self.archives = ArchiveSnapshot::default();
        }
        self.refresh(hosts, cx);
    }

    /// The connected community, as an HTTP origin; empty when none.
    pub fn destination(&self) -> String {
        self.scope
            .as_ref()
            .and_then(|scope| scope.destination.as_ref())
            .map(RelayOrigin::http)
            .unwrap_or_default()
    }

    pub fn viewer(&self) -> Option<&str> {
        self.scope.as_ref().map(|scope| scope.viewer.as_str())
    }

    /// Reads everything again.
    pub fn refresh(&mut self, hosts: &[Entity<HostConnection>], cx: &mut Context<Self>) {
        self.generation += 1;
        self.attempt += 1;
        self._tasks.clear();
        let (Some(scope), Some(identity)) = (self.scope.clone(), self.identity.clone()) else {
            self.library_status = ReadStatus::Unavailable;
            self.archive_status = ReadStatus::Unavailable;
            self.reads.clear();
            cx.notify();
            return;
        };
        let generation = self.generation;
        let runtime = Runtime::handle(cx);

        // The library: this community's relay records and every host's
        // old Buzz library.
        self.library_status = if scope.destination.is_some() {
            ReadStatus::Loading
        } else {
            ReadStatus::Unavailable
        };
        if let Some(destination) = scope.destination.clone() {
            let http = relay::lane(&destination, identity.clone());
            let owner = scope.viewer.clone();
            let remote = runtime.spawn(async move { relay::read_library(&http, &owner).await });
            let locals: Vec<_> = hosts
                .iter()
                .map(|host| host.update(cx, |host, cx| host.request(Request::Library, cx)))
                .collect();
            self._tasks.push(cx.spawn(async move |this, cx| {
                let remote = remote.await.unwrap_or_else(|_| Err("cancelled".into()));
                let mut local = Library::default();
                let mut local_failed = false;
                for read in locals {
                    match read.await {
                        Ok(Response::Library(library)) => {
                            let library = relay::from_host(&library);
                            local.definitions.extend(library.definitions);
                            local.identities.extend(library.identities);
                        }
                        Err(buzz_host_rpc::RpcError::Failed(_)) => local_failed = true,
                        // Offline hosts, and hosts from before libraries, add
                        // nothing.
                        _ => {}
                    }
                }
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    match remote {
                        Ok(remote) => {
                            this.library = relay::combine(&local, &remote);
                            this.library_status = ReadStatus::Ready;
                            this.library_error = local_failed.then(|| {
                                "Local library unavailable; showing relay inventory. Refresh agents to include local identities.".to_string()
                            });
                        }
                        Err(_) if !local.identities.is_empty() || !local.definitions.is_empty() => {
                            this.library = relay::combine(&local, &Library::default());
                            this.library_status = ReadStatus::Ready;
                            this.library_error = Some(
                                "Relay inventory unavailable; showing local library. Refresh agents to discover other identities.".into(),
                            );
                        }
                        Err(_) => {
                            this.library_status = ReadStatus::Error;
                            this.library_error = Some(
                                "Could not read agent inventory. Check the community connection and local library, then retry; saved data is unchanged.".into(),
                            );
                        }
                    }
                    cx.notify();
                });
            }));
        }

        // The connected community's archive.
        match (scope.destination.clone(), scope.authority.clone()) {
            (Some(destination), Some(authority)) => {
                self.archive_status = ReadStatus::Loading;
                let http = relay::lane(&destination, identity.clone());
                let last = self.archives.head.clone();
                let read = runtime.spawn(async move {
                    relay::read_archives(&http, &authority, last.as_ref()).await
                });
                self._tasks.push(cx.spawn(async move |this, cx| {
                    let result = read.await.unwrap_or_else(|_| Err("cancelled".into()));
                    let _ = this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        match result {
                            Ok(snapshot) => {
                                this.last_archived = snapshot.archived.clone();
                                this.archives = snapshot;
                                this.archive_status = ReadStatus::Ready;
                            }
                            Err(_) => {
                                this.last_archived.clear();
                                this.archive_status = ReadStatus::Error;
                            }
                        }
                        cx.notify();
                    });
                }));
            }
            _ => self.archive_status = ReadStatus::Unavailable,
        }

        // Every membership's inventory, then the profiles it supplies.
        self.reads.clear();
        for origin in scope.communities.clone() {
            let http = relay::lane(&origin, identity.clone());
            let owner = scope.viewer.clone();
            let key = origin.http();
            let read = runtime.spawn(async move { relay::read_community(&http, &owner).await });
            self._tasks.push(cx.spawn(async move |this, cx| {
                let result = read.await.unwrap_or_else(|_| CommunityRead {
                    error: true,
                    ..Default::default()
                });
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    if let Some(identities) = &result.identities {
                        this.known.insert(key.clone(), identities.clone());
                    }
                    if let Some(profiles) = &result.profiles {
                        this.known_profiles.insert(key.clone(), profiles.clone());
                    }
                    this.reads.insert(key, result);
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    /// Hides a key the snapshot archives. A re-read briefly knows nothing,
    /// so the last confirmed set keeps hiding until it answers.
    pub fn archived(&self, pubkey: &str) -> bool {
        if self.viewer() == Some(pubkey) {
            return false;
        }
        match self.archive_status {
            ReadStatus::Ready => self.archives.archived.contains(pubkey),
            ReadStatus::Loading => self.last_archived.contains(pubkey),
            _ => false,
        }
    }

    /// Whether every membership answered in this read.
    pub fn pending(&self) -> bool {
        self.scope.as_ref().is_some_and(|scope| {
            scope
                .communities
                .iter()
                .any(|c| !self.reads.contains_key(&c.http()))
        })
    }

    pub fn errors(&self) -> Vec<String> {
        self.reads
            .iter()
            .filter(|(_, read)| read.error)
            .map(|(community, _)| community.clone())
            .collect()
    }

    pub fn profile_errors(&self) -> Vec<String> {
        self.reads
            .iter()
            .filter(|(_, read)| read.profile_error)
            .map(|(community, _)| community.clone())
            .collect()
    }

    /// A profile a community supplied for an identity, and that community.
    pub fn source_profile(&self, pubkey: &str) -> Option<(String, buzz_relay_client::Profile)> {
        self.known_profiles
            .iter()
            .find_map(|(community, profiles)| {
                let retained = self
                    .known
                    .get(community)
                    .is_some_and(|keys| keys.iter().any(|key| key == pubkey));
                profiles
                    .get(pubkey)
                    .filter(|_| retained)
                    .map(|profile| (community.clone(), profile.clone()))
            })
    }

    /// Asks whether the viewer may archive `pubkey` here, once per attempt.
    pub fn ask_consent(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let key = format!("{}:{pubkey}", self.attempt);
        if self.consent.contains_key(&key) {
            return;
        }
        self.consent.insert(key.clone(), self.may_archive(pubkey));
        let (Some(scope), Some(identity)) = (self.scope.clone(), self.identity.clone()) else {
            return;
        };
        let (Some(destination), Some(authority)) = (scope.destination, scope.authority) else {
            return;
        };
        let http = relay::lane(&destination, identity);
        let target = pubkey.to_string();
        let read = Runtime::handle(cx)
            .spawn(async move { relay::consent(&http, &authority, &scope.viewer, &target).await });
        cx.spawn(async move |this, cx| {
            let allowed = matches!(read.await, Ok(Ok(Some(_))));
            let _ = this.update(cx, |this, cx| {
                this.consent.insert(key, allowed);
                cx.notify();
            });
        })
        .detach();
    }

    /// The last answer about archiving `pubkey`, kept until a new one.
    pub fn may_archive(&self, pubkey: &str) -> bool {
        self.consent
            .iter()
            .rev()
            .find(|(key, _)| key.ends_with(&format!(":{pubkey}")))
            .is_some_and(|(_, allowed)| *allowed)
    }

    pub fn community_name(&self, cx: &App) -> String {
        AppState::current(cx)
            .map(|community| community.read(cx).name())
            .unwrap_or_else(|| "this community".into())
    }

    /// Archive or unarchive in the connected community. `done` runs once
    /// the relay confirmed it.
    pub fn request_archive(
        &mut self,
        pubkey: &str,
        archive: bool,
        done: impl FnOnce(&mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.runs.get(pubkey).is_some_and(|run| run.error.is_none()) {
            return;
        }
        let (Some(scope), Some(identity)) = (self.scope.clone(), self.identity.clone()) else {
            return;
        };
        let (Some(destination), Some(authority)) = (scope.destination, scope.authority) else {
            return;
        };
        self.runs.insert(
            pubkey.to_string(),
            ArchiveRun {
                archive,
                error: None,
            },
        );
        cx.notify();
        let http = relay::lane(&destination, identity.clone());
        let target = pubkey.to_string();
        let last = self.archives.head.clone();
        let generation = self.generation;
        let request = Runtime::handle(cx).spawn(async move {
            relay::request_archive(&http, &identity, &authority, &target, archive, last).await
        });
        let target = pubkey.to_string();
        cx.spawn(async move |this, cx| {
            let result = request.await.unwrap_or_else(|_| Err("cancelled".into()));
            let confirmed = this.update(cx, |this, cx| {
                let confirmed = match result {
                    Ok(snapshot) => {
                        if this.generation == generation {
                            this.last_archived = snapshot.archived.clone();
                            this.archives = snapshot;
                            this.archive_status = ReadStatus::Ready;
                        }
                        this.runs.remove(&target);
                        true
                    }
                    Err(error) => {
                        this.runs.insert(
                            target,
                            ArchiveRun {
                                archive,
                                error: Some(error),
                            },
                        );
                        false
                    }
                };
                cx.notify();
                confirmed
            });
            if confirmed.unwrap_or(false) {
                cx.update(done);
            }
        })
        .detach();
    }

    /// Removes a relay-only agent from the connected community.
    pub fn remove(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        if self
            .removals
            .get(pubkey)
            .is_some_and(|removal| removal.running)
        {
            return;
        }
        let (Some(scope), Some(identity)) = (self.scope.clone(), self.identity.clone()) else {
            return;
        };
        let (Some(destination), Some(authority)) = (scope.destination, scope.authority) else {
            return;
        };
        self.removals.insert(
            pubkey.to_string(),
            Removal {
                running: true,
                error: None,
            },
        );
        cx.notify();
        let http = relay::lane(&destination, identity.clone());
        let target = pubkey.to_string();
        let archived = self.archives.archived.contains(pubkey);
        let last = self.archives.head.clone();
        let request = Runtime::handle(cx).spawn(async move {
            relay::remove_relay_agent(&http, &identity, &authority, &target, archived, last).await
        });
        let target = pubkey.to_string();
        cx.spawn(async move |this, cx| {
            let result = request
                .await
                .unwrap_or_else(|_| Err("Failed to remove agent.".into()));
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.removals.remove(&target);
                        this.removed.insert(target);
                    }
                    Err(error) => {
                        this.removals.insert(
                            target,
                            Removal {
                                running: false,
                                error: Some(error),
                            },
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
