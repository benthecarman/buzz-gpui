//! What the GUI knows about one host's agent controls, and how it writes to
//! them: buzz-app's `AgentControl` (`src/features/agents/control.ts`).
//!
//! Writes go one at a time per host. A failed write leaves the status
//! unconfirmed until a refresh reads it again, and the failure copy says so.
//! Stop is recovery: it may overtake a start waiting on a keyring prompt.

use super::HostConnection;
use buzz_host::api::{Agent, AgentAction, ProcessStatus, Request, Response};
use buzz_host_rpc::{ConnectionState, RpcError};
use gpui_kit::{Context, Task};
use std::collections::HashMap;

/// `AgentControlState["status"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    Error,
}

#[derive(Debug, Default)]
pub struct Control {
    pub busy: bool,
    /// A start waiting on its key; only Stop may interrupt it.
    pub pending_launch: Option<String>,
    pub pending_credential_write: bool,
    pub stopping: bool,
    /// `state.error`: why the status is unconfirmed.
    pub error: Option<String>,
    /// A write or refresh failed; a refresh must succeed before more writes.
    failed: bool,
    /// Accepted process actions, by agent, so outcome notices know whether
    /// they still describe the latest action.
    pub action_versions: HashMap<String, u64>,
    generation: u64,
    /// A mention could not start an agent.
    pub mention_error: Option<String>,
}

/// A failed write: the full message, and the host's own reason, if any.
#[derive(Clone, Debug)]
pub struct Failure {
    pub message: String,
    pub reason: Option<String>,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// How a write takes part in the one-at-a-time lane.
#[derive(Default)]
pub struct Lane {
    /// Stop, which may overtake a waiting start.
    pub recovery_stop: bool,
    /// The agent whose start this is.
    pub launch: Option<String>,
    /// A write that may wait on the OS keyring.
    pub credential_write: bool,
}

const AGAIN: &str = "Could not confirm the operation. Check current status and saved settings \
                     before retrying; the operation will not be repeated automatically. Your \
                     edits are retained.";

impl HostConnection {
    pub fn status(&self) -> Status {
        match self.state {
            ConnectionState::Connected { .. } if self.received && !self.control.failed => {
                Status::Ready
            }
            ConnectionState::Connected { .. } if !self.received => Status::Loading,
            ConnectionState::Connecting { .. } if !self.received => Status::Loading,
            _ => Status::Error,
        }
    }

    /// Reads the host's agents again, unless a write is in flight.
    pub fn refresh_status(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.control.busy {
            return Task::ready(());
        }
        let task = self.refresh(cx);
        cx.spawn(async move |_, _| task.await)
    }

    pub(super) fn refreshed(&mut self, ok: bool) {
        if ok {
            self.control.failed = false;
            self.control.error = None;
        } else if !self.control.busy {
            self.control.failed = true;
            self.control.error =
                Some("Could not refresh local agents. Current host status is unconfirmed.".into());
        }
    }

    /// Sends a write through the one-at-a-time lane.
    pub fn write(
        &mut self,
        request: Request,
        lane: Lane,
        cx: &mut Context<Self>,
    ) -> Task<Result<Response, Failure>> {
        let refuse = |message: &str| {
            Task::ready(Err(Failure {
                message: message.into(),
                reason: None,
            }))
        };
        if self.control.busy && !lane.recovery_stop {
            return refuse("Another agent operation is in progress.");
        }
        if self.status() != Status::Ready && !lane.recovery_stop {
            return refuse("Refresh local agents before trying again.");
        }
        let control = &mut self.control;
        control.generation += 1;
        let generation = control.generation;
        control.busy = true;
        control.error = None;
        if let Some(id) = &lane.launch {
            control.pending_launch = Some(id.clone());
        }
        if lane.credential_write {
            control.pending_credential_write = true;
        }
        control.stopping = lane.recovery_stop;
        if let Request::Action { id, .. } = &request {
            control.action_versions.insert(id.clone(), generation);
        }
        cx.notify();
        let response = self.request_within(request, super::PERSON_WAIT, cx);
        cx.spawn(async move |this, cx| {
            let result = response.await;
            let outcome = this.update(cx, |this, cx| {
                let current = this.control.generation == generation;
                let outcome = match result {
                    Ok(response) => Ok(response),
                    Err(error) => {
                        let reason = match error {
                            RpcError::Failed(reason) => Some(reason),
                            _ => None,
                        };
                        let detail = reason.as_deref().map(|reason| {
                            if reason.ends_with(['.', '!', '?']) {
                                reason.to_string()
                            } else {
                                format!("{reason}.")
                            }
                        });
                        let message = match &detail {
                            Some(detail) => format!("{detail} {AGAIN}"),
                            None => AGAIN.to_string(),
                        };
                        if current {
                            this.control.failed = true;
                            this.control.error = Some(message.clone());
                        }
                        Err(Failure {
                            message,
                            reason: detail,
                        })
                    }
                };
                let control = &mut this.control;
                if lane.launch.is_some() || lane.credential_write {
                    if lane.launch.is_some() {
                        control.pending_launch = None;
                    } else {
                        control.pending_credential_write = false;
                    }
                    control.busy = control.stopping;
                } else if current {
                    control.stopping = false;
                    control.busy =
                        control.pending_launch.is_some() || control.pending_credential_write;
                }
                cx.notify();
                outcome
            });
            outcome.unwrap_or_else(|_| {
                Err(Failure {
                    message: "Agent controls are no longer available.".into(),
                    reason: None,
                })
            })
        })
    }

    /// Start, Stop or Restart, as the lane allows.
    pub fn act(
        &mut self,
        id: &str,
        action: AgentAction,
        owner: Option<String>,
        replay_floor: Option<u64>,
        cx: &mut Context<Self>,
    ) -> Task<Result<Response, Failure>> {
        let stop = action == AgentAction::Stop;
        let lane = Lane {
            recovery_stop: stop && self.can_stop(id),
            launch: (!stop).then(|| id.to_string()),
            credential_write: false,
        };
        self.write(
            Request::Action {
                id: id.to_string(),
                action,
                replay_floor,
                owner,
            },
            lane,
            cx,
        )
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.agents.iter().find(|agent| agent.id == id)
    }

    /// `agentLaunchBlock`: why Start is unavailable, if it is.
    pub fn launch_block(&self, agent: &Agent) -> Option<String> {
        if !agent.configured {
            return Some("Choose Use here before starting this imported identity.".into());
        }
        if self.status() != Status::Ready {
            return Some("Refresh status before starting.".into());
        }
        if self.control.busy {
            return Some("Waiting for the current operation.".into());
        }
        if !self.agents.runtime_available {
            return Some(
                self.agents
                    .runtime_message
                    .clone()
                    .filter(|message| !message.is_empty())
                    .unwrap_or_else(|| "The bundled agent runtime is unavailable.".into()),
            );
        }
        match agent.status {
            ProcessStatus::Waiting => Some("Waiting to start; unlock Keychain if prompted.".into()),
            ProcessStatus::Starting | ProcessStatus::Stopping => {
                Some("Waiting for the process transition.".into())
            }
            _ => None,
        }
    }

    /// `canStopAgent`: Stop is recovery, so stale stopped evidence cannot
    /// veto it.
    pub fn can_stop(&self, id: &str) -> bool {
        let control = &self.control;
        if control.busy
            && ((control.pending_launch.is_none() && !control.pending_credential_write)
                || control.stopping)
        {
            return false;
        }
        let Some(agent) = self.agent(id) else {
            return false;
        };
        match self.status() {
            Status::Error => true,
            Status::Ready => {
                control.pending_launch.as_deref() == Some(id)
                    || agent.enabled
                    || agent.status != ProcessStatus::Stopped
            }
            Status::Loading => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AGAIN;

    #[test]
    fn failure_copy_follows_the_reference() {
        assert!(AGAIN.starts_with("Could not confirm the operation."));
        assert!(AGAIN.ends_with("Your edits are retained."));
    }
}
