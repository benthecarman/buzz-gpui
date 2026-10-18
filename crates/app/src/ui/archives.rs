//! Identity archives (NIP-IA, kind 13535) for forward-looking choices
//! (`identity-archives.ts` `archiveHides`): mentions, adding members and
//! search authors leave out identities the community archived.
//!
//! The snapshot is read with the Agents page's reader (relay-signed by the
//! NIP-11 `self` key, one event, one `-` marker, 2 MiB, never older than
//! the last one shown), once per community and again on demand. Unknown
//! state fails open, and the viewer is never hidden from themself.

use super::agents::relay::{self, ArchiveSnapshot};
use crate::community::{AppState, Community, IdentityState};
use crate::runtime::Runtime;
use gpui_kit::{App, Entity, Global};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    Error,
}

struct Entry {
    status: Status,
    snapshot: ArchiveSnapshot,
}

#[derive(Default)]
struct Archives(HashMap<String, Entry>);

impl Global for Archives {}

fn key(community: &Entity<Community>, cx: &App) -> String {
    let community = community.read(cx);
    format!("{}:{}", community.origin.http(), community.session.viewer())
}

/// Reads the community's archive snapshot unless one was read or is being
/// read; `again` reads it anyway.
pub fn ensure(community: &Entity<Community>, again: bool, cx: &mut App) {
    let key = key(community, cx);
    let current = cx
        .try_global::<Archives>()
        .and_then(|archives| archives.0.get(&key))
        .map(|entry| entry.status);
    if current == Some(Status::Loading) || (current.is_some() && !again) {
        return;
    }
    let (origin, authority) = {
        let community = community.read(cx);
        (
            community.origin.clone(),
            community
                .snapshot
                .relay
                .as_ref()
                .map(|relay| relay.authority.clone())
                .filter(|authority| !authority.is_empty()),
        )
    };
    let identity = match &cx.global::<AppState>().identity {
        IdentityState::Ready(identity) => identity.clone(),
        _ => return,
    };
    let Some(authority) = authority else {
        return;
    };
    let last = cx
        .try_global::<Archives>()
        .and_then(|archives| archives.0.get(&key))
        .and_then(|entry| entry.snapshot.head.clone());
    let archives = cx.default_global::<Archives>();
    let snapshot = archives
        .0
        .get(&key)
        .map(|entry| entry.snapshot.clone())
        .unwrap_or_default();
    archives.0.insert(
        key.clone(),
        Entry {
            status: Status::Loading,
            snapshot,
        },
    );
    let http = relay::lane(&origin, identity);
    let read = Runtime::handle(cx)
        .spawn(async move { relay::read_archives(&http, &authority, last.as_ref()).await });
    cx.spawn(async move |cx| {
        let result = read.await.unwrap_or_else(|_| Err("cancelled".into()));
        cx.update(|cx| {
            let entry = cx
                .default_global::<Archives>()
                .0
                .entry(key)
                .or_insert(Entry {
                    status: Status::Loading,
                    snapshot: ArchiveSnapshot::default(),
                });
            match result {
                Ok(snapshot) => {
                    entry.snapshot = snapshot;
                    entry.status = Status::Ready;
                }
                // A failed read shows nothing as archived (fail open).
                Err(_) => {
                    entry.snapshot = ArchiveSnapshot::default();
                    entry.status = Status::Error;
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Calls `changed` whenever any community's archive read changes.
pub fn observe(
    cx: &mut App,
    mut changed: impl FnMut(&mut App) + 'static,
) -> gpui_kit::Subscription {
    cx.observe_global::<Archives>(move |cx| changed(cx))
}

/// The read state, if a read was asked for.
pub fn status(community: &Entity<Community>, cx: &App) -> Option<Status> {
    let key = key(community, cx);
    cx.try_global::<Archives>()?
        .0
        .get(&key)
        .map(|entry| entry.status)
}

/// `archiveHides`: a known-archived identity that is not the viewer.
/// The identities the community archived, as last read.
pub fn archived(community: &Entity<Community>, cx: &App) -> Vec<String> {
    let key = key(community, cx);
    cx.try_global::<Archives>()
        .and_then(|archives| archives.0.get(&key))
        .map(|entry| entry.snapshot.archived.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn hides(community: &Entity<Community>, pubkey: &str, cx: &App) -> bool {
    let viewer = community.read(cx).session.viewer().to_string();
    let key = key(community, cx);
    pubkey != viewer
        && cx
            .try_global::<Archives>()
            .and_then(|archives| archives.0.get(&key))
            .is_some_and(|entry| entry.snapshot.archived.contains(pubkey))
}
