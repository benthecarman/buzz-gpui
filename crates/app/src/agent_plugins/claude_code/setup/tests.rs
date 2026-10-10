//! buzz-app's `setup.test.ts`.

use super::*;
use crate::agent_plugins::process::{ProcessControl, Spawn, Spawned};
use futures::future::BoxFuture;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

/// What a scripted process prints and exits with, or the spawn error.
#[derive(Clone)]
enum Script {
    Run { stdout: &'static str, code: i32 },
    Fail(&'static str),
}

struct Killable(mpsc::UnboundedSender<ProcessOutput>, Arc<AtomicBool>);

impl ProcessControl for Killable {
    fn write(&self, _: String, _: bool) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn kill(&self) -> BoxFuture<'static, Result<(), String>> {
        self.1.store(true, Ordering::SeqCst);
        let _ = self.0.send(ProcessOutput::Exit(None));
        Box::pin(async { Ok(()) })
    }
}

/// A spawn whose processes print `script[id args]` and exit with its code.
/// `install` waits for `gate` before its process is returned.
#[derive(Clone, Default)]
struct Scripted {
    scripts: HashMap<String, Script>,
    calls: Arc<Mutex<Vec<String>>>,
    gate: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    killed: Arc<AtomicBool>,
}

impl Spawn for Scripted {
    fn spawn(
        &self,
        id: &str,
        options: SpawnOptions,
    ) -> BoxFuture<'static, Result<Spawned, String>> {
        let line = format!("{id} {}", options.args.join(" "))
            .trim()
            .to_string();
        self.calls.lock().unwrap().push(options.args.join(" "));
        let script = self.scripts.get(&line).cloned();
        let gate = (id == "install")
            .then(|| self.gate.lock().unwrap().take())
            .flatten();
        let killed = self.killed.clone();
        let install = id == "install";
        Box::pin(async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            let (output, receiver) = mpsc::unbounded_channel();
            match script {
                Some(Script::Fail(error)) => return Err(error.into()),
                Some(Script::Run { stdout, code }) => {
                    let _ = output.send(ProcessOutput::Stdout(stdout.into()));
                    let _ = output.send(ProcessOutput::Exit(Some(code)));
                }
                // Unscripted processes run until killed.
                None if install => {}
                None => {
                    let _ = output.send(ProcessOutput::Exit(Some(0)));
                }
            }
            Ok(Spawned {
                process: Process(Arc::new(Killable(output, killed))),
                output: receiver,
            })
        })
    }
}

fn scripted(scripts: &[(&str, Script)]) -> Scripted {
    Scripted {
        scripts: scripts
            .iter()
            .map(|(line, script)| (line.to_string(), script.clone()))
            .collect(),
        ..Default::default()
    }
}

fn run(stdout: &'static str, code: i32) -> Script {
    Script::Run { stdout, code }
}

#[tokio::test]
async fn finds_claude_code_missing_when_it_cannot_be_started() {
    let spawn: Spawner = Arc::new(scripted(&[(
        "claude --version",
        Script::Fail("Could not start claude: No such file"),
    )]));
    assert_eq!(check_claude(&spawn).await, ClaudeStatus::Missing);
    let old: Spawner = Arc::new(scripted(&[(
        "claude --version",
        Script::Fail(crate::agent_plugins::process::UPDATE_HOST),
    )]));
    assert_eq!(
        check_claude(&old).await,
        ClaudeStatus::Error {
            message: "Update this host to run Claude Code agents".into()
        }
    );
}

#[tokio::test]
async fn reports_the_version_and_whether_it_is_signed_in() {
    let signed_out: Spawner = Arc::new(scripted(&[
        ("claude --version", run("2.1.3 (Claude Code)\n", 0)),
        ("claude auth status --json", run("{\"loggedIn\":false}", 1)),
    ]));
    assert_eq!(
        check_claude(&signed_out).await,
        ClaudeStatus::SignedOut {
            version: "2.1.3".into()
        }
    );
    let ready: Spawner = Arc::new(scripted(&[
        ("claude --version", run("2.1.3 (Claude Code)\n", 0)),
        (
            "claude auth status --json",
            run(
                "{\"loggedIn\":true,\"email\":\"a@example.com\",\"orgName\":\"Example\"}",
                0,
            ),
        ),
    ]));
    assert_eq!(
        check_claude(&ready).await,
        ClaudeStatus::Ready {
            version: "2.1.3".into(),
            account: Some("a@example.com · Example".into())
        }
    );
}

#[tokio::test]
async fn runs_sign_in_shows_its_output_and_checks_again_after() {
    let script = scripted(&[
        ("claude auth login", run("Opening browser…\n", 0)),
        ("claude --version", run("2.1.3\n", 0)),
        ("claude auth status --json", run("{\"loggedIn\":true}", 0)),
    ]);
    let calls = script.calls.clone();
    let setup = ClaudeSetup::new(Some(Arc::new(script)));
    let running = tokio::spawn({
        let setup = setup.clone();
        async move { setup.run(Step::Login).await }
    });
    running.await.unwrap();
    let state = setup.snapshot();
    assert_eq!(state.running, None);
    assert_eq!(state.output, "Opening browser…\n");
    assert_eq!(
        state.status,
        Some(ClaudeStatus::Ready {
            version: "2.1.3".into(),
            account: None
        })
    );
    assert_eq!(
        *calls.lock().unwrap(),
        ["auth login", "--version", "auth status --json"]
    );
}

#[tokio::test]
async fn stops_a_step_cancelled_before_its_process_was_returned() {
    let script = scripted(&[]);
    let (open, gate) = oneshot::channel();
    *script.gate.lock().unwrap() = Some(gate);
    let killed = script.killed.clone();
    let setup = ClaudeSetup::new(Some(Arc::new(script)));
    let running = tokio::spawn({
        let setup = setup.clone();
        async move { setup.run(Step::Install).await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(setup.snapshot().running, Some(Step::Install));
    setup.cancel().await;
    open.send(()).unwrap();
    running.await.unwrap();
    assert!(killed.load(Ordering::SeqCst));
    let state = setup.snapshot();
    assert_eq!(state.running, None);
    assert_eq!(state.output, "\nInstall ended (stopped).");
}

#[tokio::test]
async fn explains_that_it_needs_a_host() {
    let setup = ClaudeSetup::new(None);
    setup.check().await;
    assert_eq!(
        setup.snapshot().status,
        Some(ClaudeStatus::Error {
            message: "Claude Code agents run only in the desktop app".into()
        })
    );
}
