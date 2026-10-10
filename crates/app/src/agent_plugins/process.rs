//! Processes an agent type starts on the host that holds its agent, the
//! port of buzz-app's `host.spawn` (`src/features/host/service.ts`).
//!
//! The host runs only processes the type declares, and streams their output
//! back to this connection as [`HostAsk::ProcessOutput`]. The GUI picks each
//! handle and registers where its output goes before asking, so nothing
//! that arrives early is lost.

use buzz_host::api::{HostAnswer, HostProtocol, ProcessOutput, Request};
use buzz_host_rpc::{ClientEvent, HostClient, RpcError};
use futures::future::BoxFuture;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::mpsc;

/// How to start a declared process.
#[derive(Clone, Debug, Default)]
pub struct SpawnOptions {
    /// After the arguments the type declares.
    pub args: Vec<String>,
    /// An absolute directory or one under home written `~/…`; created if
    /// missing.
    pub cwd: Option<String>,
    /// Added to the host's environment; `None` removes a variable.
    pub env: BTreeMap<String, Option<String>>,
}

/// A running process: its controls, and its output ending in an exit.
pub struct Spawned {
    pub process: Process,
    pub output: mpsc::UnboundedReceiver<ProcessOutput>,
}

pub trait ProcessControl: Send + Sync {
    /// Writes `data` to stdin; `close` then ends it.
    fn write(&self, data: String, close: bool) -> BoxFuture<'static, Result<(), String>>;
    /// Asks it to stop, then kills it and its descendants.
    fn kill(&self) -> BoxFuture<'static, Result<(), String>>;
}

/// A process's controls. Cheap to clone.
#[derive(Clone)]
pub struct Process(pub Arc<dyn ProcessControl>);

impl Process {
    pub async fn write(&self, data: String) -> Result<(), String> {
        self.0.write(data, false).await
    }
    pub async fn kill(&self) -> Result<(), String> {
        self.0.kill().await
    }
}

/// Starts an agent type's declared processes.
pub trait Spawn: Send + Sync {
    fn spawn(&self, id: &str, options: SpawnOptions)
    -> BoxFuture<'static, Result<Spawned, String>>;
}

pub type Spawner = Arc<dyn Spawn>;

/// Where each process's output goes, by handle, for one host connection.
/// Cheap to clone.
#[derive(Clone, Default)]
pub struct ProcessSinks {
    inner: Arc<Sinks>,
}

#[derive(Default)]
struct Sinks {
    next: AtomicU64,
    sinks: Mutex<HashMap<u64, mpsc::UnboundedSender<ProcessOutput>>>,
}

impl ProcessSinks {
    fn sinks(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<u64, mpsc::UnboundedSender<ProcessOutput>>> {
        self.inner
            .sinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn open(&self) -> (u64, mpsc::UnboundedReceiver<ProcessOutput>) {
        let handle = self.inner.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = mpsc::unbounded_channel();
        self.sinks().insert(handle, sender);
        (handle, receiver)
    }

    /// Hands output the host sent to its process's reader. Runs as the ask
    /// is read, so output keeps its order. An unknown handle, or a reader
    /// that is gone, tells the host to stop the process.
    pub fn deliver(&self, handle: u64, output: Vec<ProcessOutput>) -> Result<HostAnswer, RpcError> {
        let mut sinks = self.sinks();
        let Some(sink) = sinks.get(&handle) else {
            return Err(RpcError::Failed("No such process".into()));
        };
        let mut exited = false;
        for item in output {
            exited |= matches!(item, ProcessOutput::Exit(_));
            if sink.send(item).is_err() {
                sinks.remove(&handle);
                return Err(RpcError::Failed("Nobody is reading that process".into()));
            }
        }
        if exited {
            sinks.remove(&handle);
        }
        Ok(HostAnswer::ProcessReceived)
    }

    /// The connection dropped, and the host killed this connection's
    /// processes with it: each ends, its exit unknown.
    fn disconnected(&self) {
        for (_, sink) in self.sinks().drain() {
            let _ = sink.send(ProcessOutput::Exit(None));
        }
    }

    /// Ends this connection's processes whenever the connection drops.
    pub fn follow(&self, client: &HostClient<HostProtocol>, runtime: &tokio::runtime::Handle) {
        let sinks = self.clone();
        let mut events = client.events();
        runtime.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ClientEvent::Disconnected) => sinks.disconnected(),
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return,
                }
            }
        });
    }
}

/// Spawns `owner`'s processes on one host.
pub struct HostSpawner {
    pub client: Arc<HostClient<HostProtocol>>,
    pub sinks: ProcessSinks,
    /// The agent type whose declared processes these are.
    pub owner: &'static str,
}

/// What a host older than the process bridge answers.
pub const UPDATE_HOST: &str = "Update this host to run Claude Code agents";

fn failure(error: RpcError) -> String {
    match error {
        RpcError::Failed(message) if message.contains("does not support this request") => {
            UPDATE_HOST.into()
        }
        other => other.to_string(),
    }
}

impl Spawn for HostSpawner {
    fn spawn(
        &self,
        id: &str,
        options: SpawnOptions,
    ) -> BoxFuture<'static, Result<Spawned, String>> {
        let client = self.client.clone();
        let sinks = self.sinks.clone();
        let request = |handle| Request::ProcessSpawn {
            handle,
            owner: self.owner.into(),
            process: id.into(),
            args: options.args,
            cwd: options.cwd,
            env: options.env,
        };
        let (handle, output) = sinks.open();
        let request = request(handle);
        Box::pin(async move {
            if let Err(error) = client.request(request).await {
                sinks.sinks().remove(&handle);
                return Err(failure(error));
            }
            Ok(Spawned {
                process: Process(Arc::new(HostProcess { client, handle })),
                output,
            })
        })
    }
}

struct HostProcess {
    client: Arc<HostClient<HostProtocol>>,
    handle: u64,
}

impl ProcessControl for HostProcess {
    fn write(&self, data: String, close: bool) -> BoxFuture<'static, Result<(), String>> {
        let client = self.client.clone();
        let handle = self.handle;
        Box::pin(async move {
            client
                .request(Request::ProcessWrite {
                    handle,
                    data,
                    close,
                })
                .await
                .map(drop)
                .map_err(failure)
        })
    }

    fn kill(&self) -> BoxFuture<'static, Result<(), String>> {
        let client = self.client.clone();
        let handle = self.handle;
        Box::pin(async move {
            client
                .request(Request::ProcessKill { handle })
                .await
                .map(drop)
                .map_err(failure)
        })
    }
}

/// Runs everything a process printed to completion: its exit code (`None`
/// for a signal or a lost host) and its stdout and stderr.
pub async fn collect(
    output: &mut mpsc::UnboundedReceiver<ProcessOutput>,
) -> (Option<i32>, String, String) {
    let (mut stdout, mut stderr) = (String::new(), String::new());
    while let Some(item) = output.recv().await {
        match item {
            ProcessOutput::Stdout(text) => stdout += &text,
            ProcessOutput::Stderr(text) => stderr += &text,
            ProcessOutput::Exit(code) => return (code, stdout, stderr),
        }
    }
    (None, stdout, stderr)
}
