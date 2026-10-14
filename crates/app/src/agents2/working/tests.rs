use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn counted() -> (Arc<AtomicUsize>, End) {
    let ended = Arc::new(AtomicUsize::new(0));
    let end = {
        let ended = ended.clone();
        Box::new(move || {
            ended.fetch_add(1, Ordering::SeqCst);
        }) as End
    };
    (ended, end)
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(20)).await;
}

#[tokio::test]
async fn ends_once_the_run_and_everything_it_handed_off_have_settled() {
    let (ended, end) = counted();
    let working = Working::new(vec![end], tokio::runtime::Handle::current());
    let (done, finished) = tokio::sync::oneshot::channel::<()>();
    working.until(async move {
        let _ = finished.await;
    });
    // `run` returned, but the turn it handed off is still going.
    working.release();
    settle().await;
    assert_eq!(ended.load(Ordering::SeqCst), 0);
    done.send(()).unwrap();
    settle().await;
    assert_eq!(ended.load(Ordering::SeqCst), 1);
    // Once over, nothing holds it again or ends it twice.
    working.until(async {});
    working.release();
    settle().await;
    assert_eq!(ended.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_run_that_hands_nothing_off_ends_when_it_returns() {
    let (ended, end) = counted();
    let working = Working::new(vec![end], tokio::runtime::Handle::current());
    working.release();
    assert_eq!(ended.load(Ordering::SeqCst), 1);
}

#[test]
fn types_in_the_thread_the_mention_belongs_to() {
    let event = |tags: Vec<Vec<&str>>| EventData {
        id: "m".repeat(64),
        pubkey: "a".repeat(64),
        created_at: 1,
        kind: 9,
        content: String::new(),
        tags: tags
            .into_iter()
            .map(|tag| tag.into_iter().map(str::to_owned).collect())
            .collect(),
    };
    let root = "r".repeat(64);
    let parent = "p".repeat(64);
    assert_eq!(thread_root(&event(vec![vec!["h", "c"]])), "m".repeat(64));
    assert_eq!(
        thread_root(&event(vec![vec!["e", &root, "", "reply"]])),
        root
    );
    assert_eq!(
        thread_root(&event(vec![
            vec!["e", &root, "", "root"],
            vec!["e", &parent, "", "reply"]
        ])),
        root
    );
    // A lone root marker is not a reply.
    assert_eq!(
        thread_root(&event(vec![vec!["e", &root, "", "root"]])),
        "m".repeat(64)
    );
}
