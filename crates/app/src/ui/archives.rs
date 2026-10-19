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
    /// When the last read finished (ms), and why it failed.
    read_at: Option<u64>,
    error: Option<String>,
    /// The session generation it was read in: a retry or cache clear
    /// reads it again, as buzz-app clears it then.
    generation: u64,
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
    let generation = community.read(cx).session.generation();
    let current = cx
        .try_global::<Archives>()
        .and_then(|archives| archives.0.get(&key))
        .map(|entry| (entry.status, entry.generation));
    let stale = current.is_some_and(|(_, read)| read != generation);
    if current.is_some_and(|(status, _)| status == Status::Loading)
        || (current.is_some() && !again && !stale)
    {
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
    let (read_at, error) = archives
        .0
        .get(&key)
        .map(|entry| (entry.read_at, entry.error.clone()))
        .unwrap_or_default();
    archives.0.insert(
        key.clone(),
        Entry {
            status: Status::Loading,
            snapshot,
            read_at,
            error,
            generation,
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
                    read_at: None,
                    error: None,
                    generation,
                });
            entry.read_at = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_millis() as u64),
            );
            match result {
                Ok(snapshot) => {
                    entry.snapshot = snapshot;
                    entry.status = Status::Ready;
                    entry.error = None;
                }
                // A failed read shows nothing as archived (fail open).
                Err(error) => {
                    entry.snapshot = ArchiveSnapshot::default();
                    entry.status = Status::Error;
                    entry.error = Some(error);
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

/// The archive as plugins see it (`IdentityArchiveSnapshot`): `status`
/// is `unavailable` for a community without an archive, `idle` before a
/// read, then `loading`, `ready` or `error`.
pub fn snapshot_json(community: &Entity<Community>, cx: &App) -> serde_json::Value {
    let available = community
        .read(cx)
        .snapshot
        .relay
        .as_ref()
        .is_some_and(|relay| !relay.authority.is_empty());
    let key = key(community, cx);
    let entry = cx
        .try_global::<Archives>()
        .and_then(|archives| archives.0.get(&key));
    let status = match entry.map(|entry| entry.status) {
        _ if !available => "unavailable",
        None => "idle",
        Some(Status::Loading) => "loading",
        Some(Status::Ready) => "ready",
        Some(Status::Error) => "error",
    };
    let head = entry.and_then(|entry| entry.snapshot.head.as_ref());
    serde_json::json!({
        "status": status,
        "archived": entry
            .map(|entry| entry.snapshot.archived.iter().collect::<Vec<_>>())
            .unwrap_or_default(),
        "eventId": head.map(|(_, id)| id),
        "createdAt": head.map(|(at, _)| at),
        "readAt": entry.and_then(|entry| entry.read_at),
        "error": entry.and_then(|entry| entry.error.clone()),
    })
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
/// Every identity the community's archive hides from the viewer: all
/// archived ones but the viewer's own.
pub fn hidden(community: &Entity<Community>, cx: &App) -> Vec<String> {
    let viewer = community.read(cx).session.viewer().to_string();
    let key = key(community, cx);
    cx.try_global::<Archives>()
        .and_then(|archives| archives.0.get(&key))
        .map(|entry| {
            entry
                .snapshot
                .archived
                .iter()
                .filter(|pubkey| **pubkey != viewer)
                .cloned()
                .collect()
        })
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
