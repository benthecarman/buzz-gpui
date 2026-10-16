//! How long a run counts as working (buzz-app's `working` in
//! `service.ts`), and what shows it meanwhile: the request's local work
//! row (`local-activity.ts`), and the agent typing in the mention's
//! thread on this device (`local-typing.ts`, a STOPGAP). Nothing public
//! is published while an agent works.

use super::attention::EventData;
use crate::agent_activity::local::{WorkHandle, WorkOutcome};
use std::sync::{Arc, Mutex, PoisonError};

type End = Box<dyn FnOnce() + Send>;

struct State {
    /// Work still holding the run: the run itself and what it handed off.
    /// Below zero once stopped.
    holds: i64,
    /// The first failure any hold reported.
    outcome: WorkOutcome,
    ends: Vec<End>,
}

/// Keeps a run counted as working until it and everything it handed to
/// [`Working::until`] have settled. Cheap to clone; clones are one hold.
#[derive(Clone)]
pub struct Working {
    state: Arc<Mutex<State>>,
    /// The mention's local work row, if the run has one.
    work: Option<WorkHandle>,
    runtime: tokio::runtime::Handle,
}

impl Working {
    /// A hold held by the run itself, which ends what `ends` started once
    /// every hold is released, however long that takes: the harness, not
    /// Buzz, decides how long an agent works. Its work row is working from
    /// now on, and settles with the outcome the holds report.
    pub(crate) fn new(
        ends: Vec<End>,
        work: Option<WorkHandle>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        if let Some(work) = &work {
            work.started();
        }
        Self {
            state: Arc::new(Mutex::new(State {
                holds: 1,
                outcome: WorkOutcome::ok(),
                ends,
            })),
            work,
            runtime,
        }
    }

    /// The run's local work row, if it has one.
    pub fn work(&self) -> Option<WorkHandle> {
        self.work.clone()
    }

    /// A run with nothing to show.
    pub fn none(runtime: tokio::runtime::Handle) -> Self {
        Self::new(Vec::new(), None, runtime)
    }

    /// Keeps the run counted as working after `run` returns, until `done`
    /// settles, for a type that hands its work off and returns early. A
    /// failed outcome shows the request failed.
    pub fn until(&self, done: impl std::future::Future<Output = WorkOutcome> + Send + 'static) {
        {
            let mut state = self.lock();
            if state.holds <= 0 {
                return;
            }
            state.holds += 1;
        }
        let working = self.clone();
        self.runtime.spawn(async move {
            let outcome = done.await;
            working.release_with(outcome);
        });
    }

    /// Lets go of one hold.
    pub(crate) fn release(&self) {
        self.release_with(WorkOutcome::ok());
    }

    /// Lets go of one hold that ended with `outcome`.
    pub(crate) fn release_with(&self, outcome: WorkOutcome) {
        let ended = {
            let mut state = self.lock();
            if state.holds <= 0 {
                return;
            }
            if !outcome.ok && state.outcome.ok {
                state.outcome = WorkOutcome::failed(
                    outcome
                        .error
                        .unwrap_or_else(|| "Could not finish this request".into()),
                );
            }
            state.holds -= 1;
            (state.holds == 0).then(|| state.outcome.clone())
        };
        if let Some(outcome) = ended {
            if let Some(work) = &self.work {
                work.finish(outcome);
            }
            self.stop();
        }
    }

    /// The agent went away mid-run: nothing is left to show.
    pub(crate) fn abort(&self) {
        if let Some(work) = &self.work {
            work.remove();
        }
        self.stop();
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
