//! buzz-app's `sessions.test.ts`.

use super::*;
use crate::agent_plugins::claude_code::claude::testing::{FakeClaude, FakeSpawn, flush};
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use tokio::sync::watch;

#[derive(Default)]
struct MemoryStore(Mutex<HashMap<String, SavedSession>>);

impl SessionStore for MemoryStore {
    fn get(&self, key: &str) -> Option<SavedSession> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn set(&self, key: &str, session: SavedSession) {
        self.0.lock().unwrap().insert(key.into(), session);
    }
    fn delete(&self, key: &str) {
        self.0.lock().unwrap().remove(key);
    }
}

struct Pool {
    pool: AgentSessions,
    store: Arc<MemoryStore>,
    fake: FakeSpawn,
    model: Arc<Mutex<String>>,
    release: watch::Sender<bool>,
}

impl Pool {
    fn claudes(&self) -> Vec<FakeClaude> {
        self.fake.claudes()
    }
    fn live(&self) -> Vec<FakeClaude> {
        self.fake.live()
    }
    fn set_model(&self, model: &str) {
        *self.model.lock().unwrap() = model.into();
    }
    fn keys(&self) -> Vec<String> {
        self.pool
            .snapshot()
            .into_iter()
            .map(|view| view.key)
            .collect()
    }
    fn deliver(&self, key: &str, text: &str, seen: u64) -> BoxFuture<'static, Settled> {
        self.pool
            .deliver(key.into(), text.into(), seen, DeliverOptions::default())
    }
}

/// `hold`: every process keeps its turns open until finished. `held`:
/// each launch waits, after reading the settings, until released, as a
/// memory read does.
fn sessions(store: Option<Arc<MemoryStore>>, hold: bool, held: bool, idle: Duration) -> Pool {
    let fake = FakeSpawn {
        hold,
        ..Default::default()
    };
    let store = store.unwrap_or_default();
    let model = Arc::new(Mutex::new("opus".to_string()));
    let (release, released) = watch::channel(!held);
    let ids = Arc::new(AtomicUsize::new(0));
    let launch_model = model.clone();
    let fingerprint_model = model.clone();
    let mut options = SessionsOptions::new(
        fake.spawner(),
        store.clone(),
        Arc::new(move || {
            let current = launch_model.lock().unwrap().clone();
            let mut released = released.clone();
            Box::pin(async move {
                let _ = released.wait_for(|released| *released).await;
                Ok(Launch {
                    cwd: "/work".into(),
                    system_prompt: "prompt".into(),
                    model: Some(current),
                    ..Default::default()
                })
            })
        }),
        Arc::new(move || fingerprint_model.lock().unwrap().clone()),
    );
    options.new_id = Arc::new(move || format!("new-{}", ids.fetch_add(1, Ordering::SeqCst) + 1));
    options.idle = idle;
    Pool {
        pool: AgentSessions::new(options),
        store,
        fake,
        model,
        release,
    }
}

fn pool() -> Pool {
    sessions(None, false, false, IDLE)
}

#[tokio::test]
async fn starts_a_new_conversation_on_the_warm_spare_and_warms_another() {
    let pool = pool();
    pool.pool.warm();
    flush().await;
    assert_eq!(pool.claudes().len(), 1);
    assert_eq!(pool.deliver("c/root", "hello", 10).await, Settled::ok());
    flush().await;
    assert_eq!(pool.claudes().len(), 2);
    assert_eq!(pool.claudes()[0].prompts(), ["hello"]);
    let saved = pool.store.get("c/root").unwrap();
    assert_eq!((saved.id.as_str(), saved.seen), ("new-1", 10));
    assert_eq!(
        pool.pool.snapshot(),
        vec![SessionView {
            key: "c/root".into(),
            busy: false
        }]
    );
}

#[tokio::test]
async fn continues_a_conversation_in_its_own_process_apart_from_others() {
    let pool = pool();
    pool.deliver("c/one", "first", 1).await;
    pool.deliver("c/two", "other", 2).await;
    pool.deliver("c/one", "second", 3).await;
    let prompts: Vec<Vec<String>> = pool.claudes().iter().map(FakeClaude::prompts).collect();
    assert!(prompts.contains(&vec!["first".into(), "second".into()]));
    assert!(prompts.contains(&vec!["other".into()]));
}

#[tokio::test]
async fn steers_a_conversation_that_is_still_working_instead_of_queueing() {
    let pool = pool();
    pool.pool.warm();
    flush().await;
    let process = pool.claudes()[0].clone();
    process.set_hold(true);
    let first = tokio::spawn(pool.deliver("c/root", "first", 1));
    flush().await;
    let second = tokio::spawn(pool.pool.deliver(
        "c/root".into(),
        "plain".into(),
        2,
        DeliverOptions {
            steer: Some("steer".into()),
            fresh: None,
        },
    ));
    flush().await;
    assert_eq!(process.prompts(), ["first", "steer"]);
    assert_eq!(
        pool.pool.snapshot(),
        vec![SessionView {
            key: "c/root".into(),
            busy: true
        }]
    );
    process.finish(None);
    assert_eq!(first.await.unwrap(), Settled::ok());
    assert_eq!(second.await.unwrap(), Settled::ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn opens_one_process_for_messages_that_arrive_together() {
    let pool = pool();
    let (one, two) = futures::join!(
        pool.deliver("c/root", "one", 1),
        pool.deliver("c/root", "two", 2)
    );
    assert!(one.ok && two.ok);
    let used: Vec<FakeClaude> = pool
        .claudes()
        .into_iter()
        .filter(|process| !process.prompts().is_empty())
        .collect();
    assert_eq!(used.len(), 1);
    assert_eq!(used[0].prompts(), ["one", "two"]);
}

#[tokio::test]
async fn resumes_a_saved_conversation_after_its_process_stops() {
    let pool = sessions(None, false, false, Duration::from_millis(50));
    pool.deliver("c/root", "first", 1).await;
    let first = pool.claudes()[0].clone();
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(first.killed());
    assert_eq!(pool.pool.snapshot(), vec![]);
    pool.deliver("c/root", "later", 5).await;
    let resumed = pool.claudes().last().unwrap().clone();
    assert!(resumed.args().contains(&"--resume".into()));
    assert!(resumed.args().contains(&"new-1".into()));
    assert_eq!(resumed.prompts(), ["later"]);
    let saved = pool.store.get("c/root").unwrap();
    assert_eq!((saved.id.as_str(), saved.seen), ("new-1", 5));
}

#[tokio::test]
async fn starts_over_when_a_saved_session_no_longer_loads() {
    let store = Arc::new(MemoryStore::default());
    store.set(
        "c/root",
        SavedSession {
            id: "lost".into(),
            seen: 4,
            at: 1,
        },
    );
    let pool = sessions(Some(store), false, false, IDLE);
    let settled = pool
        .pool
        .deliver(
            "c/root".into(),
            "hello".into(),
            9,
            DeliverOptions {
                steer: None,
                fresh: Some(Box::pin(async { "hello, with the thread so far".into() })),
            },
        )
        .await;
    assert_eq!(settled, Settled::ok());
    let claudes = pool.claudes();
    assert!(claudes[0].args().contains(&"lost".into()));
    assert!(claudes[1].args().contains(&"--session-id".into()));
    assert_eq!(claudes[1].prompts(), ["hello, with the thread so far"]);
    let saved = pool.store.get("c/root").unwrap();
    assert_eq!((saved.id.as_str(), saved.seen), ("new-1", 9));
}

#[tokio::test]
async fn stops_the_longest_idle_conversation_to_stay_within_its_limit() {
    let pool = pool();
    for index in 0..MAX_LIVE + 2 {
        pool.deliver(&format!("c/{index}"), "hi", index as u64)
            .await;
    }
    flush().await;
    assert!(pool.live().len() <= MAX_LIVE);
    let keys = pool.keys();
    assert!(!keys.contains(&"c/0".to_string()));
    assert!(keys.contains(&format!("c/{}", MAX_LIVE + 1)));
}

#[tokio::test]
async fn says_it_is_busy_rather_than_start_a_process_past_its_limit() {
    let pool = sessions(None, true, false, IDLE);
    pool.pool.warm();
    for index in 0..MAX_LIVE {
        tokio::spawn(pool.deliver(&format!("c/{index}"), "hi", index as u64));
    }
    flush().await;
    assert_eq!(pool.live().len(), MAX_LIVE);
    assert_eq!(
        pool.deliver("c/extra", "hi", 9).await,
        Settled::failed(format!(
            "Claude Code is already working in {MAX_LIVE} conversations; try again when one finishes"
        ))
    );
    assert_eq!(pool.live().len(), MAX_LIVE);
    assert!(!pool.keys().contains(&"c/extra".to_string()));
}

#[tokio::test]
async fn counts_conversations_still_starting_toward_its_limit() {
    let pool = sessions(None, true, true, IDLE);
    let results: Vec<_> = (0..=MAX_LIVE)
        .map(|index| tokio::spawn(pool.deliver(&format!("c/{index}"), "hi", index as u64)))
        .collect();
    let last = results.into_iter().last().unwrap();
    flush().await;
    assert!(!last.await.unwrap().ok);
    pool.release.send_replace(true);
    flush().await;
    assert_eq!(pool.live().len(), MAX_LIVE);
}

#[tokio::test]
async fn starts_again_with_settings_saved_while_a_process_started() {
    let pool = sessions(None, false, true, IDLE);
    let done = tokio::spawn(pool.deliver("c/root", "hi", 1));
    flush().await;
    pool.set_model("sonnet");
    pool.pool.reconfigure();
    pool.release.send_replace(true);
    assert_eq!(done.await.unwrap(), Settled::ok());
    let claudes = pool.claudes();
    // The conversation's first start and the new spare race to spawn.
    let old: Vec<&FakeClaude> = claudes
        .iter()
        .filter(|process| process.args().contains(&"opus".into()))
        .collect();
    assert_eq!(old.len(), 1);
    assert!(old[0].killed());
    let used: Vec<FakeClaude> = claudes
        .into_iter()
        .filter(|process| !process.prompts().is_empty())
        .collect();
    assert_eq!(used.len(), 1);
    assert!(used[0].args().contains(&"sonnet".into()));
}

#[tokio::test]
async fn restarts_idle_processes_and_the_spare_when_settings_change() {
    let pool = pool();
    pool.deliver("c/root", "hi", 1).await;
    flush().await;
    let (conversation, spare) = (pool.claudes()[0].clone(), pool.claudes()[1].clone());
    pool.set_model("sonnet");
    pool.pool.reconfigure();
    flush().await;
    assert!(conversation.killed());
    assert!(spare.killed());
    assert!(
        pool.claudes()
            .last()
            .unwrap()
            .args()
            .contains(&"sonnet".into())
    );
    pool.deliver("c/root", "again", 2).await;
    let args = pool.claudes().last().unwrap().args();
    for wanted in ["--resume", "new-1", "--model", "sonnet"] {
        assert!(args.contains(&wanted.into()), "{args:?}");
    }
}

#[tokio::test]
async fn stops_every_process_when_disposed() {
    let pool = pool();
    pool.pool.warm();
    pool.deliver("c/root", "hi", 1).await;
    flush().await;
    pool.pool.dispose();
    flush().await;
    assert!(pool.claudes().iter().all(FakeClaude::killed));
    assert!(!pool.deliver("c/root", "late", 2).await.ok);
}

#[test]
fn keeps_saved_sessions_per_agent_in_a_file() {
    let dir = tempfile_dir();
    let path = dir.join("sessions.json");
    let one = FileSessions::new(path.clone(), "one");
    let two = FileSessions::new(path.clone(), "two");
    let saved = |id: &str, seen, at| SavedSession {
        id: id.into(),
        seen,
        at,
    };
    one.set("c/root", saved("s1", 3, 1));
    two.set("c/root", saved("s2", 4, 2));
    assert_eq!(
        FileSessions::new(path.clone(), "one")
            .get("c/root")
            .unwrap()
            .id,
        "s1"
    );
    assert_eq!(two.get("c/root").unwrap().id, "s2");
    one.delete("c/root");
    assert_eq!(one.get("c/root"), None);
    assert_eq!(two.get("c/root").unwrap().id, "s2");
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(one.get("c/root"), None);
    // The newest 200 are kept.
    for index in 0..205 {
        one.set(&format!("c/{index}"), saved("s", 0, index));
    }
    assert_eq!(one.get("c/0"), None);
    assert!(one.get("c/204").is_some());
    let _ = std::fs::remove_dir_all(dir);
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("buzz-claude-sessions-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
