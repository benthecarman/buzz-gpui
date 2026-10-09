//! Local persistence: one SQLite file per install (spec §7.5).
//!
//! Everything a community writes is keyed by its partition, the relay
//! origin plus the viewer's pubkey, so leaving a community is one
//! transaction. The outbox needs `synchronous=FULL`: an intent must be on
//! disk before its event is signed, and signed bytes before they are sent.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Partition {
    /// `https://host[:port]`; the byte format must never change, or saved
    /// data is orphaned (spec §9.2 #16).
    pub origin: String,
    pub viewer: String,
}

impl Partition {
    pub fn new(origin: &crate::RelayOrigin, viewer: &str) -> Self {
        Self {
            origin: origin.http(),
            viewer: viewer.to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("local storage failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("local data is corrupt: {0}")]
    Corrupt(String),
}

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE outbox_ops (
  origin        TEXT NOT NULL,
  viewer        TEXT NOT NULL,
  event_id      TEXT NOT NULL CHECK(length(event_id)=64),
  kind          INTEGER NOT NULL,
  created_at    INTEGER NOT NULL,
  channel_id    TEXT,
  template_json TEXT NOT NULL,
  signed_json   TEXT,
  delivery      TEXT NOT NULL CHECK(delivery IN ('sending','accepted','unknown','failed','seen')),
  error         TEXT,
  recovery_key  TEXT CHECK(recovery_key IS NULL OR length(recovery_key) BETWEEN 1 AND 200),
  recovery_value TEXT,
  acknowledged  INTEGER NOT NULL DEFAULT 0,
  guarded       INTEGER NOT NULL DEFAULT 0,
  confirmed     INTEGER NOT NULL DEFAULT 0,
  enqueued_seq  INTEGER NOT NULL,
  bytes         INTEGER NOT NULL,
  updated_ms    INTEGER NOT NULL,
  PRIMARY KEY (origin, viewer, event_id)
);
CREATE INDEX outbox_pending ON outbox_ops(origin, viewer, confirmed, enqueued_seq);
CREATE UNIQUE INDEX outbox_recovery ON outbox_ops(origin, viewer, recovery_key)
  WHERE recovery_key IS NOT NULL AND confirmed = 0;

CREATE TABLE head_events (
  origin TEXT NOT NULL, viewer TEXT NOT NULL, channel_id TEXT NOT NULL,
  event_id TEXT NOT NULL, created_at INTEGER NOT NULL, kind INTEGER NOT NULL,
  raw_json TEXT NOT NULL,
  PRIMARY KEY (origin, viewer, channel_id, event_id)
);
CREATE TABLE head_meta (
  origin TEXT NOT NULL, viewer TEXT NOT NULL, channel_id TEXT NOT NULL,
  saved_ms INTEGER NOT NULL, bytes INTEGER NOT NULL, meta_json TEXT NOT NULL,
  PRIMARY KEY (origin, viewer, channel_id)
);
CREATE TABLE startup_cache (
  origin TEXT NOT NULL, viewer TEXT NOT NULL,
  discovery_json TEXT, preferences_json TEXT, saved_ms INTEGER NOT NULL, bytes INTEGER NOT NULL,
  PRIMARY KEY (origin, viewer)
);
CREATE TABLE read_state (
  origin TEXT NOT NULL, viewer TEXT NOT NULL,
  journal_json TEXT NOT NULL,
  PRIMARY KEY (origin, viewer)
);
CREATE TABLE view_state (
  origin TEXT NOT NULL, viewer TEXT NOT NULL, key TEXT NOT NULL,
  value_json TEXT NOT NULL,
  updated_ms INTEGER NOT NULL,
  PRIMARY KEY (origin, viewer, key)
);
CREATE TABLE partition_prefs (
  origin TEXT NOT NULL, viewer TEXT NOT NULL, name TEXT NOT NULL, value_json TEXT NOT NULL,
  PRIMARY KEY (origin, viewer, name)
);
CREATE TABLE viewer_prefs (
  viewer TEXT NOT NULL, name TEXT NOT NULL, value_json TEXT NOT NULL,
  PRIMARY KEY (viewer, name)
);
CREATE TABLE device_prefs (name TEXT PRIMARY KEY, value_json TEXT NOT NULL);
"#;

/// Tables holding one community's data, cleared together on leave.
const PARTITION_TABLES: &[&str] = &[
    "outbox_ops",
    "head_events",
    "head_meta",
    "startup_cache",
    "read_state",
    "view_state",
    "partition_prefs",
];

/// Shared handle to the database. Operations are short; callers on async
/// tasks that do bulk work should move it to a blocking thread.
#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        }
        Self::setup(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, StoreError> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(connection: Connection) -> Result<Self, StoreError> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                connection.execute_batch(&format!(
                    "BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
                ))?;
            }
            SCHEMA_VERSION => {}
            newer => {
                return Err(StoreError::Corrupt(format!(
                    "database schema {newer} is newer than this app understands"
                )));
            }
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Runs `f` with exclusive access to the connection.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut connection = self.connection.lock().expect("store lock");
        f(&mut connection)
    }

    pub fn view_state(
        &self,
        partition: &Partition,
        key: &str,
    ) -> Result<Option<String>, StoreError> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT value_json FROM view_state WHERE origin = ?1 AND viewer = ?2 AND key = ?3",
                params![partition.origin, partition.viewer, key],
                |row| row.get(0),
            )
            .optional()?)
        })
    }

    /// `None` removes the key.
    pub fn set_view_state(
        &self,
        partition: &Partition,
        key: &str,
        value: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with(|c| {
            match value {
                Some(value) => c.execute(
                    "INSERT INTO view_state (origin, viewer, key, value_json, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (origin, viewer, key) DO UPDATE SET
                       value_json = excluded.value_json, updated_ms = excluded.updated_ms",
                    params![partition.origin, partition.viewer, key, value, now_ms()],
                )?,
                None => c.execute(
                    "DELETE FROM view_state WHERE origin = ?1 AND viewer = ?2 AND key = ?3",
                    params![partition.origin, partition.viewer, key],
                )?,
            };
            Ok(())
        })
    }

    pub fn partition_pref(
        &self,
        partition: &Partition,
        name: &str,
    ) -> Result<Option<String>, StoreError> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT value_json FROM partition_prefs WHERE origin = ?1 AND viewer = ?2 AND name = ?3",
                params![partition.origin, partition.viewer, name],
                |row| row.get(0),
            )
            .optional()?)
        })
    }

    pub fn set_partition_pref(
        &self,
        partition: &Partition,
        name: &str,
        value: &str,
    ) -> Result<(), StoreError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO partition_prefs (origin, viewer, name, value_json) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (origin, viewer, name) DO UPDATE SET value_json = excluded.value_json",
                params![partition.origin, partition.viewer, name, value],
            )?;
            Ok(())
        })
    }

    pub fn viewer_pref(&self, viewer: &str, name: &str) -> Result<Option<String>, StoreError> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT value_json FROM viewer_prefs WHERE viewer = ?1 AND name = ?2",
                params![viewer, name],
                |row| row.get(0),
            )
            .optional()?)
        })
    }

    pub fn set_viewer_pref(&self, viewer: &str, name: &str, value: &str) -> Result<(), StoreError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO viewer_prefs (viewer, name, value_json) VALUES (?1, ?2, ?3)
                 ON CONFLICT (viewer, name) DO UPDATE SET value_json = excluded.value_json",
                params![viewer, name, value],
            )?;
            Ok(())
        })
    }

    pub fn device_pref(&self, name: &str) -> Result<Option<String>, StoreError> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT value_json FROM device_prefs WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()?)
        })
    }

    pub fn set_device_pref(&self, name: &str, value: &str) -> Result<(), StoreError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO device_prefs (name, value_json) VALUES (?1, ?2)
                 ON CONFLICT (name) DO UPDATE SET value_json = excluded.value_json",
                params![name, value],
            )?;
            Ok(())
        })
    }

    /// Replaces the cached head of one channel. Events are stored as their
    /// signed wire JSON and re-verified when loaded.
    pub fn save_head(
        &self,
        partition: &Partition,
        channel: &str,
        events: &[crate::Verified],
        meta_json: &str,
    ) -> Result<(), StoreError> {
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM head_events WHERE origin = ?1 AND viewer = ?2 AND channel_id = ?3",
                params![partition.origin, partition.viewer, channel],
            )?;
            let mut bytes = meta_json.len();
            {
                let mut insert = tx.prepare(
                    "INSERT OR REPLACE INTO head_events
                       (origin, viewer, channel_id, event_id, created_at, kind, raw_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                for event in events {
                    let raw = event.as_json();
                    bytes += raw.len();
                    insert.execute(params![
                        partition.origin,
                        partition.viewer,
                        channel,
                        event.id_hex(),
                        event.created_at_secs() as i64,
                        i64::from(event.kind_u16()),
                        raw,
                    ])?;
                }
            }
            tx.execute(
                "INSERT INTO head_meta (origin, viewer, channel_id, saved_ms, bytes, meta_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (origin, viewer, channel_id) DO UPDATE SET
                   saved_ms = excluded.saved_ms, bytes = excluded.bytes, meta_json = excluded.meta_json",
                params![partition.origin, partition.viewer, channel, now_ms(), bytes as i64, meta_json],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    /// The cached head: raw event JSON (to re-verify) and its metadata, if
    /// saved within `max_age_ms`.
    pub fn load_head(
        &self,
        partition: &Partition,
        channel: &str,
        max_age_ms: i64,
    ) -> Result<Option<(Vec<String>, String)>, StoreError> {
        self.with(|c| {
            let meta: Option<(i64, String)> = c
                .query_row(
                    "SELECT saved_ms, meta_json FROM head_meta
                     WHERE origin = ?1 AND viewer = ?2 AND channel_id = ?3",
                    params![partition.origin, partition.viewer, channel],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((saved_ms, meta)) = meta else {
                return Ok(None);
            };
            if now_ms() - saved_ms > max_age_ms {
                return Ok(None);
            }
            let mut query = c.prepare(
                "SELECT raw_json FROM head_events WHERE origin = ?1 AND viewer = ?2 AND channel_id = ?3
                 ORDER BY created_at DESC, event_id ASC",
            )?;
            let events = query
                .query_map(params![partition.origin, partition.viewer, channel], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(Some((events, meta)))
        })
    }

    /// Keeps cached heads within a global budget, oldest first out.
    pub fn trim_heads(&self, max_heads: usize, max_bytes: usize) -> Result<(), StoreError> {
        self.with(|c| {
            let tx = c.transaction()?;
            let heads: Vec<(String, String, String, i64)> = {
                let mut query = tx.prepare(
                    "SELECT origin, viewer, channel_id, bytes FROM head_meta ORDER BY saved_ms DESC",
                )?;
                query
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
                    .collect::<Result<_, _>>()?
            };
            let mut kept_bytes = 0usize;
            for (index, (origin, viewer, channel, bytes)) in heads.into_iter().enumerate() {
                kept_bytes += bytes as usize;
                if index >= max_heads || kept_bytes > max_bytes {
                    for table in ["head_events", "head_meta"] {
                        tx.execute(
                            &format!(
                                "DELETE FROM {table} WHERE origin = ?1 AND viewer = ?2 AND channel_id = ?3"
                            ),
                            params![origin, viewer, channel],
                        )?;
                    }
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    /// Removes everything stored for one community, in one transaction.
    /// The session must be disposed first so nothing writes it again.
    pub fn purge(&self, partition: &Partition) -> Result<(), StoreError> {
        self.with(|c| {
            let tx = c.transaction()?;
            for table in PARTITION_TABLES {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE origin = ?1 AND viewer = ?2"),
                    params![partition.origin, partition.viewer],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Verifier;
    use nostr::prelude::{EventBuilder, FinalizeEvent, Keys, Kind, Tag};

    fn partition(origin: &str) -> Partition {
        Partition {
            origin: origin.into(),
            viewer: "ab".repeat(32),
        }
    }

    #[test]
    fn opens_files_and_refuses_newer_schemas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/buzz.sqlite3");
        Store::open(&path).unwrap();
        // Reopening an existing database keeps its data.
        let store = Store::open(&path).unwrap();
        store.set_device_pref("theme", "\"dark\"").unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.device_pref("theme").unwrap().as_deref(),
            Some("\"dark\"")
        );
        store
            .with(|c| Ok(c.pragma_update(None, "user_version", 99)?))
            .unwrap();
        drop(store);
        assert!(matches!(Store::open(&path), Err(StoreError::Corrupt(_))));
    }

    #[test]
    fn view_state_is_partitioned_and_removable() {
        let store = Store::in_memory().unwrap();
        let a = partition("https://a.example");
        let b = partition("https://b.example");
        store
            .set_view_state(&a, "draft:general", Some("\"hi\""))
            .unwrap();
        assert_eq!(
            store.view_state(&a, "draft:general").unwrap().as_deref(),
            Some("\"hi\"")
        );
        assert_eq!(store.view_state(&b, "draft:general").unwrap(), None);
        store.set_view_state(&a, "draft:general", None).unwrap();
        assert_eq!(store.view_state(&a, "draft:general").unwrap(), None);
    }

    #[test]
    fn heads_round_trip_and_purge_clears_only_one_partition() {
        let store = Store::in_memory().unwrap();
        let verifier = Verifier::default();
        let keys = Keys::generate();
        let events: Vec<_> = (0..3)
            .map(|i| {
                let event = EventBuilder::new(Kind::from_u16(9), format!("m{i}"))
                    .tags([Tag::custom("h", ["general"])])
                    .finalize(&keys)
                    .unwrap();
                verifier.verify(event).unwrap()
            })
            .collect();
        let a = partition("https://a.example");
        let b = partition("https://b.example");
        store.save_head(&a, "general", &events, "{}").unwrap();
        store.save_head(&b, "general", &events[..1], "{}").unwrap();
        let (raw, meta) = store.load_head(&a, "general", 60_000).unwrap().unwrap();
        assert_eq!(raw.len(), 3);
        assert_eq!(meta, "{}");
        let reverified = verifier.verify_json(&serde_json::from_str(&raw[0]).unwrap());
        assert!(reverified.is_ok());

        store.purge(&a).unwrap();
        assert!(store.load_head(&a, "general", 60_000).unwrap().is_none());
        assert!(store.load_head(&b, "general", 60_000).unwrap().is_some());
    }

    #[test]
    fn head_budget_drops_the_oldest() {
        let store = Store::in_memory().unwrap();
        let a = partition("https://a.example");
        for channel in ["one", "two", "three"] {
            store.save_head(&a, channel, &[], "{}").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        store.trim_heads(2, usize::MAX).unwrap();
        assert!(store.load_head(&a, "one", 60_000).unwrap().is_none());
        assert!(store.load_head(&a, "three", 60_000).unwrap().is_some());
    }
}

/// The outbox journal for one partition (spec §6.5, §7.5). Every commit
/// replaces the partition's rows with the whole journal in one
/// transaction, so a crash leaves either the old journal or the new one.
#[derive(Clone)]
pub struct SqliteJournal {
    store: Store,
    partition: Partition,
}

impl SqliteJournal {
    pub fn new(store: Store, partition: Partition) -> Self {
        Self { store, partition }
    }
}

impl crate::outbox::Journal for SqliteJournal {
    fn load(
        &self,
    ) -> futures::future::BoxFuture<'static, Result<Vec<crate::outbox::Operation>, String>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.load_blocking())
                .await
                .map_err(|_| "Outbox load was interrupted".to_string())?
                .map_err(|error| error.to_string())
        })
    }

    fn commit(
        &self,
        operations: Vec<crate::outbox::Operation>,
    ) -> futures::future::BoxFuture<'static, Result<(), String>> {
        let this = self.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.commit_blocking(&operations))
                .await
                .map_err(|_| "Outbox commit was interrupted".to_string())?
                .map_err(|error| error.to_string())
        })
    }
}

impl SqliteJournal {
    fn load_blocking(&self) -> Result<Vec<crate::outbox::Operation>, StoreError> {
        use crate::outbox::{Delivery, Operation, Recovery};
        self.store.with(|c| {
            let mut query = c.prepare(
                "SELECT template_json, signed_json, delivery, error, recovery_key, recovery_value,
                        acknowledged, guarded
                 FROM outbox_ops WHERE origin = ?1 AND viewer = ?2 ORDER BY enqueued_seq",
            )?;
            let rows = query
                .query_map(
                    params![self.partition.origin, self.partition.viewer],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, bool>(6)?,
                            row.get::<_, bool>(7)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(
                    |(template, signed, delivery, error, key, value, acknowledged, guarded)| {
                        let corrupt = |what: &str| StoreError::Corrupt(format!("outbox {what}"));
                        let delivery: Delivery =
                            serde_json::from_value(serde_json::Value::String(delivery))
                                .map_err(|_| corrupt("delivery"))?;
                        Ok(Operation {
                            event: serde_json::from_str(&template)
                                .map_err(|_| corrupt("template"))?,
                            signed: signed
                                .map(|json| serde_json::from_str(&json))
                                .transpose()
                                .map_err(|_| corrupt("signed event"))?,
                            recovery: match (key, value) {
                                (Some(key), Some(value)) => Some(Recovery { key, value }),
                                (None, None) => None,
                                _ => return Err(corrupt("recovery")),
                            },
                            acknowledged,
                            guarded,
                            delivery,
                            error,
                        })
                    },
                )
                .collect()
        })
    }

    fn commit_blocking(&self, operations: &[crate::outbox::Operation]) -> Result<(), StoreError> {
        self.store.with(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM outbox_ops WHERE origin = ?1 AND viewer = ?2",
                params![self.partition.origin, self.partition.viewer],
            )?;
            {
                let mut insert = tx.prepare(
                    "INSERT INTO outbox_ops (origin, viewer, event_id, kind, created_at, channel_id,
                       template_json, signed_json, delivery, error, recovery_key, recovery_value,
                       acknowledged, guarded, confirmed, enqueued_seq, bytes, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
                )?;
                let now = now_ms();
                for (seq, op) in operations.iter().enumerate() {
                    let template = serde_json::to_string(&op.event)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                    let signed = op
                        .signed
                        .as_ref()
                        .map(|event| event.as_json());
                    let delivery = serde_json::to_value(op.delivery)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_default();
                    let bytes = template.len() + signed.as_ref().map_or(0, String::len);
                    let confirmed = matches!(op.delivery, crate::outbox::Delivery::Seen)
                        && op.recovery.is_none();
                    insert.execute(params![
                        self.partition.origin,
                        self.partition.viewer,
                        op.id().to_hex(),
                        i64::from(op.kind()),
                        op.created_at() as i64,
                        op.channel(),
                        template,
                        signed,
                        delivery,
                        op.error,
                        op.recovery.as_ref().map(|r| r.key.as_str()),
                        op.recovery.as_ref().map(|r| r.value.as_str()),
                        op.acknowledged,
                        op.guarded,
                        confirmed,
                        seq as i64,
                        bytes as i64,
                        now,
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod journal_tests {
    use super::*;
    use crate::outbox::{Delivery, Journal, Operation, Recovery};
    use buzz_identity::{Identity, Purpose};

    fn operation(identity: &Identity, content: &str, delivery: Delivery, sign: bool) -> Operation {
        let unsigned = identity
            .template(
                Purpose::Content,
                9,
                1_700_000_000,
                vec![vec!["h".into(), "general".into()]],
                content.into(),
            )
            .unwrap();
        Operation {
            signed: sign.then(|| identity.sign(Purpose::Content, unsigned.clone()).unwrap()),
            event: unsigned,
            recovery: None,
            acknowledged: false,
            guarded: false,
            delivery,
            error: None,
        }
    }

    #[tokio::test]
    async fn commits_replace_the_whole_journal_in_order() {
        let store = Store::in_memory().unwrap();
        let partition = Partition {
            origin: "https://a.example".into(),
            viewer: "ab".repeat(32),
        };
        let journal = SqliteJournal::new(store.clone(), partition.clone());
        let identity = Identity::ephemeral();
        let mut first = operation(&identity, "one", Delivery::Unknown, true);
        first.recovery = Some(Recovery {
            key: "draft:general".into(),
            value: "{}".into(),
        });
        let second = operation(&identity, "two", Delivery::Sending, false);
        journal
            .commit(vec![first.clone(), second.clone()])
            .await
            .unwrap();

        let loaded = journal.load().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].id(), first.id());
        assert_eq!(
            loaded[0].signed.as_ref().unwrap().sig,
            first.signed.as_ref().unwrap().sig
        );
        assert_eq!(loaded[0].recovery, first.recovery);
        assert_eq!(loaded[1].delivery, Delivery::Sending);
        assert!(loaded[1].signed.is_none());

        journal.commit(vec![second]).await.unwrap();
        assert_eq!(journal.load().await.unwrap().len(), 1);

        // Another partition is untouched by this one's commits.
        let other = SqliteJournal::new(
            store,
            Partition {
                origin: "https://b.example".into(),
                ..partition
            },
        );
        assert!(other.load().await.unwrap().is_empty());
    }
}

/// The NIP-RS read-state journal for one partition (spec §5i.4): one JSON
/// record in `read_state`. The record is returned exactly as saved;
/// [`crate::read_state::ReadStateSync`] validates it, so a corrupt record
/// is an error and is never replaced by an empty journal (§9.2 #25).
#[derive(Clone)]
pub struct SqliteReadJournal {
    store: Store,
    partition: Partition,
}

impl SqliteReadJournal {
    pub fn new(store: Store, partition: Partition) -> Self {
        Self { store, partition }
    }
}

fn read_state_error(error: StoreError) -> crate::read_state::ReadStateError {
    crate::read_state::ReadStateError::Storage(error.to_string())
}

impl crate::read_state::ReadStateJournal for SqliteReadJournal {
    fn load(&mut self) -> Result<Option<String>, crate::read_state::ReadStateError> {
        self.store
            .with(|c| {
                Ok(c.query_row(
                    "SELECT journal_json FROM read_state WHERE origin = ?1 AND viewer = ?2",
                    params![self.partition.origin, self.partition.viewer],
                    |row| row.get(0),
                )
                .optional()?)
            })
            .map_err(read_state_error)
    }

    fn store(&mut self, record: &str) -> Result<(), crate::read_state::ReadStateError> {
        self.store
            .with(|c| {
                c.execute(
                    "INSERT INTO read_state (origin, viewer, journal_json) VALUES (?1, ?2, ?3)
                     ON CONFLICT (origin, viewer) DO UPDATE SET journal_json = excluded.journal_json",
                    params![self.partition.origin, self.partition.viewer, record],
                )?;
                Ok(())
            })
            .map_err(read_state_error)
    }

    fn purge(&mut self) -> Result<(), crate::read_state::ReadStateError> {
        self.store
            .with(|c| {
                c.execute(
                    "DELETE FROM read_state WHERE origin = ?1 AND viewer = ?2",
                    params![self.partition.origin, self.partition.viewer],
                )?;
                Ok(())
            })
            .map_err(read_state_error)
    }
}

/// Device-local thread follows for one partition (spec §5i.9), saved as
/// the reference's JSON array in `partition_prefs`. As with the
/// reference's local storage, unreadable data reads as no choices.
#[derive(Clone)]
pub struct SqliteThreadFollows {
    store: Store,
    partition: Partition,
}

impl SqliteThreadFollows {
    pub const NAME: &'static str = "thread-follows";

    pub fn new(store: Store, partition: Partition) -> Self {
        Self { store, partition }
    }
}

impl crate::unread::ThreadFollowStore for SqliteThreadFollows {
    fn read(&mut self) -> Vec<(String, bool)> {
        self.store
            .partition_pref(&self.partition, Self::NAME)
            .ok()
            .flatten()
            .map(|text| crate::unread::parse_thread_follows(&text))
            .unwrap_or_default()
    }

    fn write(&mut self, choices: &[(String, bool)]) -> Result<(), String> {
        self.store
            .set_partition_pref(
                &self.partition,
                Self::NAME,
                &crate::unread::encode_thread_follows(choices),
            )
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod read_state_tests {
    use super::*;
    use crate::read_state::{
        ReadCapability, ReadStateJournal, ReadStateSync, ReadSyncStatus, ReadUpdate,
    };
    use crate::unread::ThreadFollowStore;

    fn partition(origin: &str) -> Partition {
        Partition {
            origin: origin.into(),
            viewer: "ab".repeat(32),
        }
    }

    #[test]
    fn read_journals_persist_per_partition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buzz.sqlite3");
        let a = partition("https://a.example");
        let viewer = a.viewer.clone();
        let store = Store::open(&path).unwrap();
        let mut sync = ReadStateSync::open(
            &viewer,
            ReadCapability::FrontierSync,
            SqliteReadJournal::new(store.clone(), a.clone()),
        );
        sync.apply(ReadUpdate::observe("general", 42), None, 1_000)
            .unwrap();
        let slot = sync.journal().unwrap().slot.clone();
        drop((sync, store));

        let store = Store::open(&path).unwrap();
        let sync = ReadStateSync::open(
            &viewer,
            ReadCapability::FrontierSync,
            SqliteReadJournal::new(store.clone(), a.clone()),
        );
        let journal = sync.journal().unwrap();
        assert_eq!(journal.slot, slot);
        assert_eq!(journal.state.frontier("general"), Some(42));

        // Another community starts its own journal.
        let mut other = SqliteReadJournal::new(store.clone(), partition("https://b.example"));
        assert_eq!(other.load().unwrap(), None);

        store.purge(&a).unwrap();
        assert_eq!(
            SqliteReadJournal::new(store, a).load().unwrap(),
            None,
            "leaving the community deletes its journal"
        );
    }

    #[test]
    fn a_corrupt_read_journal_is_an_error_and_kept() {
        let store = Store::in_memory().unwrap();
        let a = partition("https://a.example");
        let mut journal = SqliteReadJournal::new(store.clone(), a.clone());
        journal.store("{\"version\":1}").unwrap();
        let mut sync = ReadStateSync::open(&a.viewer, ReadCapability::FrontierSync, journal);
        assert_eq!(sync.snapshot().status, ReadSyncStatus::Error);
        assert!(sync.journal().is_none());
        assert!(
            sync.apply(ReadUpdate::observe("general", 1), None, 1_000)
                .is_err()
        );
        assert_eq!(
            sync.into_store().load().unwrap().as_deref(),
            Some("{\"version\":1}")
        );
        let mut purge = SqliteReadJournal::new(store, a);
        purge.purge().unwrap();
        assert_eq!(purge.load().unwrap(), None);
    }

    #[test]
    fn thread_follows_round_trip_per_partition() {
        let store = Store::in_memory().unwrap();
        let a = partition("https://a.example");
        let mut follows = SqliteThreadFollows::new(store.clone(), a.clone());
        assert!(follows.read().is_empty());
        let choices = vec![
            (format!("general:{}", "1".repeat(64)), true),
            (format!("general:{}", "2".repeat(64)), false),
        ];
        follows.write(&choices).unwrap();
        assert_eq!(
            SqliteThreadFollows::new(store.clone(), a.clone()).read(),
            choices
        );
        assert!(
            SqliteThreadFollows::new(store.clone(), partition("https://b.example"))
                .read()
                .is_empty()
        );

        // Unreadable data is no choices, like the reference's storage.
        store
            .set_partition_pref(&a, SqliteThreadFollows::NAME, "not json")
            .unwrap();
        assert!(follows.read().is_empty());

        follows.write(&choices[..1]).unwrap();
        store.purge(&a).unwrap();
        assert!(follows.read().is_empty());
    }
}
