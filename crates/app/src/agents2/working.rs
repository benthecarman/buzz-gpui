//! How long a run counts as working (buzz-app's `working` in
//! `service.ts`), and what shows it meanwhile: 👀 and 💬 reactions on the
//! mention (`working-reactions.ts`) and the agent typing in the mention's
//! thread on this device (`local-typing.ts`). Both are STOPGAPs; the
//! working hold stays.

use super::attention::EventData;
use super::runtime::AgentHandle;
use buzz_host::api::AppAgentEvent;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use std::sync::{Arc, Mutex, PoisonError};

type End = Box<dyn FnOnce() + Send>;

struct State {
    /// Work still holding the run: the run itself and what it handed off.
    /// Below zero once stopped.
    holds: i64,
    ends: Vec<End>,
}

/// Keeps a run counted as working until it and everything it handed to
/// [`Working::until`] have settled. Cheap to clone; clones are one hold.
#[derive(Clone)]
pub struct Working {
    state: Arc<Mutex<State>>,
    runtime: tokio::runtime::Handle,
}

impl Working {
    /// A hold held by the run itself, which ends what `ends` started once
    /// every hold is released, however long that takes: the harness, not
    /// Buzz, decides how long an agent works.
    pub(crate) fn new(ends: Vec<End>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            state: Arc::new(Mutex::new(State { holds: 1, ends })),
            runtime,
        }
    }

    /// A run with nothing to show.
    pub fn none(runtime: tokio::runtime::Handle) -> Self {
        Self::new(Vec::new(), runtime)
    }

    /// Keeps the run counted as working after `run` returns, until `done`
    /// settles, for a type that hands its work off and returns early.
    pub fn until(&self, done: impl std::future::Future<Output = ()> + Send + 'static) {
        {
            let mut state = self.lock();
            if state.holds <= 0 {
                return;
            }
            state.holds += 1;
        }
        let working = self.clone();
        self.runtime.spawn(async move {
            done.await;
            working.release();
        });
    }

    /// Lets go of one hold.
    pub(crate) fn release(&self) {
        let ended = {
            let mut state = self.lock();
            if state.holds <= 0 {
                return;
            }
            state.holds -= 1;
            state.holds == 0
        };
        if ended {
            self.stop();
        }
    }

    fn stop(&self) {
        let ends = {
            let mut state = self.lock();
            if state.holds < 0 {
                return;
            }
            state.holds = -1;
            std::mem::take(&mut state.ends)
        };
        for end in ends {
            end();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// STOPGAP(agent-working-reactions): DELETE once agents show as typing
// through the relay. Keep `Working`, which typing indicators need too.
const EMOJI: [&str; 2] = ["👀", "💬"];

/// Reacts to `event` as the agent now with 👀 and 💬; the returned end
/// takes the reactions back once they have been published.
pub(crate) fn react_while_working(
    agent: &AgentHandle,
    event: &EventData,
    channel: &str,
    runtime: &tokio::runtime::Handle,
) -> End {
    let reactions: Shared<BoxFuture<'static, Vec<Result<String, String>>>> = {
        let (agent, id, channel) = (agent.clone(), event.id.clone(), channel.to_owned());
        let published = runtime.spawn(async move {
            futures::future::join_all(EMOJI.map(|emoji| {
                let agent = agent.clone();
                let event = AppAgentEvent {
                    kind: 7,
                    content: emoji.into(),
                    tags: vec![
                        vec!["h".into(), channel.clone()],
                        vec!["e".into(), id.clone()],
                    ],
                };
                async move {
                    let signed = agent.publish(event).await?;
                    signed
                        .get("id")
                        .and_then(|id| id.as_str())
                        .map(str::to_owned)
                        .ok_or_else(|| "Invalid agent event".to_string())
                }
            }))
            .await
        });
        async move { published.await.unwrap_or_default() }
            .boxed()
            .shared()
    };
    let (agent, channel, runtime) = (agent.clone(), channel.to_owned(), runtime.clone());
    Box::new(move || {
        runtime.spawn(async move {
            let results = reactions.await;
            let ids: Vec<String> = results.iter().filter_map(|r| r.clone().ok()).collect();
            if ids.len() < EMOJI.len() {
                tracing::warn!("agent working reactions were not all published");
            }
            // The relay takes one target per deletion.
            let removed = futures::future::join_all(ids.into_iter().map(|id| {
                agent.publish(AppAgentEvent {
                    kind: 5,
                    content: String::new(),
                    tags: vec![
                        vec!["h".into(), channel.clone()],
                        vec!["e".into(), id],
                        vec!["k".into(), "7".into()],
                    ],
                })
            }))
            .await;
            if let Some(Err(error)) = removed.into_iter().find(Result::is_err) {
                tracing::warn!(%error, "agent working reactions were not removed");
            }
        });
    })
}

// STOPGAP(agent-local-typing): DELETE once agents' typing pulses reach the
// relay, with `Session::type_locally`.
/// Clients show a pulse for 8 s, so one every 3 s keeps it steady.
pub(crate) const PULSE: std::time::Duration = std::time::Duration::from_secs(3);

/// The thread an agent answering `event` works in: its root, or the event
/// itself when it starts one (NIP-10, the last marked tags winning).
pub(crate) fn thread_root(event: &EventData) -> String {
    let (mut root, mut reply) = (None, None);
    for tag in &event.tags {
        if tag.first().map(String::as_str) != Some("e") {
            continue;
        }
        let Some(id) = tag.get(1).filter(|id| !id.is_empty()) else {
            continue;
        };
        match tag.get(3).map(String::as_str) {
            Some("root") => root = Some(id.clone()),
            Some("reply") => reply = Some(id.clone()),
            _ => {}
        }
    }
    match reply {
        Some(reply) => root.unwrap_or(reply),
        None => event.id.clone(),
    }
}

#[cfg(test)]
mod tests;
