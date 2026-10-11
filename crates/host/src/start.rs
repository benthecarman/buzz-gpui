//! Starting agents, as buzz-app's native host does (`src-tauri/src/agents.rs`,
//! `start_guarded`).
//!
//! A start is admitted on the host thread and given a ticket. The waits that
//! follow (the tool PATH, the Pi version check, the owner check and the OS
//! credential read) run between host jobs, so a Stop sent meanwhile removes
//! the ticket and the late start does nothing. Each agent shows "waiting"
//! until its key is read, then "starting".

use crate::api::AgentsSnapshot;
use crate::{AgentHost, Owner};
use buzz_agent_controller::{Action, AgentView, ProcessStatus};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub(crate) const START_CANCELLED: &str = "Start cancelled by a newer action";
const NO_SAVE_RESTART: &str = "Agent no longer needs a save restart";
// Pi is installed only on macOS and Linux.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(crate) const NOT_WAITING_FOR_PI: &str = "Agent no longer waiting for Pi";
const REPLAY_NOT_ATTACHED: &str = "Saved settings changed; mention replay was not attached";

/// A start between admission and launch.
pub(crate) struct PendingStart {
    ticket: u64,
    /// The Databricks workspace whose sign-in the start reads.
    workspace: Option<String>,
    pub(crate) status: ProcessStatus,
    revision: u64,
    replay_floor: Option<u64>,
}

/// A mention attached to an agent that starts with the host, before its
/// start is admitted.
pub(crate) struct MentionReplay {
    revision: u64,
    floor: u64,
}

pub(crate) struct LogChallenge {
    id: String,
    pubkey: String,
    relay_url: String,
    issued: Instant,
}

/// Refuses a start whose agent no longer qualifies, with the reason.
pub(crate) type Guard = (fn(&AgentView) -> bool, &'static str);

#[derive(Default)]
pub(crate) struct StartOptions {
    /// Started because it starts with the host, not by a person.
    pub restore: bool,
    pub replay_floor: Option<u64>,
    /// The human asking; `None` skips the owner check.
    pub owner: Option<String>,
    pub guard: Option<Guard>,
}

fn is_running(agent: &AgentView) -> bool {
    agent.enabled && agent.status == ProcessStatus::Running
}

fn needs_save_restart(agent: &AgentView) -> bool {
    is_running(agent) && !agent.restart_diff.is_empty()
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn waiting_for_pi(agent: &AgentView) -> bool {
    buzz_desktop::harness_setup::waiting_for_pi(agent)
}

impl Owner {
    fn check_guard(&mut self, id: &str, guard: Option<Guard>) -> Result<(), String> {
        let Some((eligible, refusal)) = guard else {
            return Ok(());
        };
        if self
            .controller
            .snapshot()?
            .agents
            .iter()
            .any(|agent| agent.id == id && eligible(agent))
        {
            Ok(())
        } else {
            Err(refusal.into())
        }
    }

    fn pending(&self, id: &str, ticket: u64) -> Result<(), String> {
        self.starts
            .get(id)
            .filter(|pending| pending.ticket == ticket)
            .map(drop)
            .ok_or_else(|| START_CANCELLED.into())
    }

    fn take_start(&mut self, id: &str, ticket: u64) -> Result<PendingStart, String> {
        self.pending(id, ticket)?;
        self.starts.remove(id).ok_or_else(|| START_CANCELLED.into())
    }

    /// Old Buzz may still run an agent imported from it; both must not.
    fn refuse_legacy(&self, id: &str) -> Result<(), String> {
        if self.controller.requires_legacy_handover(id)? {
            (self.legacy_check)()
        } else {
            Ok(())
        }
    }

    /// Adds a mention's replay floor to a waiting start; never starts.
    pub(crate) fn attach_mention(
        &mut self,
        id: &str,
        revision: u64,
        floor: u64,
    ) -> Result<(), String> {
        if !self
            .controller
            .snapshot()?
            .agents
            .iter()
            .any(|agent| agent.id == id && agent.revision == revision)
        {
            return Err(REPLAY_NOT_ATTACHED.into());
        }
        if let Some(pending) = self.starts.get_mut(id) {
            if pending.revision != revision {
                return Err(REPLAY_NOT_ATTACHED.into());
            }
            pending.replay_floor = Some(pending.replay_floor.map_or(floor, |old| old.min(floor)));
        } else if let Some(replay) = self.queued.get_mut(id) {
            if replay.as_ref().is_some_and(|old| old.revision != revision) {
                return Err(REPLAY_NOT_ATTACHED.into());
            }
            let floor = replay.as_ref().map_or(floor, |old| old.floor.min(floor));
            *replay = Some(MentionReplay { revision, floor });
        } else {
            return Err(
                "Launch already finished or was cancelled; mention replay could not be confirmed"
                    .into(),
            );
        }
        Ok(())
    }

    /// A Disconnect also cancels starts waiting on that workspace's
    /// sign-in, so a late sign-in cannot start against the removed cache.
    pub(crate) fn cancel_starts_for(&mut self, workspace: &str) {
        let cancelled: Vec<_> = self
            .starts
            .iter()
            .filter(|(_, pending)| pending.workspace.as_deref() == Some(workspace))
            .map(|(id, _)| id.clone())
            .collect();
        for id in cancelled {
            self.starts.remove(&id);
            self.controller.record_error(
                &id,
                "Start cancelled by Disconnect; reconnect and retry Start".into(),
            );
        }
        let queued: Vec<_> = self.queued.keys().cloned().collect();
        for id in queued {
            if self
                .controller
                .credential_request(&id)
                .is_ok_and(|request| request.3.as_deref() == Some(workspace))
            {
                self.queued.remove(&id);
                self.acted.insert(id.clone());
                self.controller.record_error(
                    &id,
                    "Start cancelled by Disconnect; reconnect and retry Start".into(),
                );
            }
        }
    }

    /// A nonce the GUI signs with the human key to read the agent's log.
    pub(crate) fn log_challenge(
        &mut self,
        id: String,
        pubkey: String,
        relay_url: String,
    ) -> Result<String, String> {
        self.controller.log_target(&id, &pubkey, &relay_url)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        self.log_challenges
            .retain(|_, pending| pending.issued.elapsed() <= Duration::from_secs(20));
        if self.log_challenges.len() >= 4 {
            return Err("Too many pending log authorizations".into());
        }
        self.log_challenges.insert(
            nonce.clone(),
            LogChallenge {
                id,
                pubkey,
                relay_url,
                issued: Instant::now(),
            },
        );
        Ok(nonce)
    }

    pub(crate) fn read_log(
        &mut self,
        id: &str,
        pubkey: &str,
        relay_url: &str,
        nonce: &str,
        signature: &str,
    ) -> Result<String, String> {
        // Consumed before any check, so even a failed proof cannot be replayed.
        let challenge = self
            .log_challenges
            .remove(nonce)
            .ok_or("Log authorization expired")?;
        if challenge.issued.elapsed() > Duration::from_secs(20)
            || challenge.id != id
            || challenge.pubkey != pubkey
            || challenge.relay_url != relay_url
        {
            return Err("Log authorization expired".into());
        }
        self.controller
            .read_log(id, pubkey, relay_url, nonce, signature)
    }
}

/// Advisory only: unmodified old Buzz does not share the controller's
/// lock. Never inspects other processes beyond their names.
pub(crate) fn refuse_legacy() -> Result<(), String> {
    let output = process_listing()
        .output()
        .map_err(|_| "Could not check old Buzz processes; Start refused")?;
    if !output.status.success() || output.stdout.len() > 4 * 1024 * 1024 {
        return Err("Could not check old Buzz processes; Start refused".into());
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    #[cfg(windows)]
    let listing = tasklist_listing(&listing);
    refuse_legacy_listing(&listing)
}

#[cfg(not(windows))]
fn process_listing() -> std::process::Command {
    let mut command = std::process::Command::new("ps");
    command
        .args(["-axo", "pid=,comm="])
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    command
}

/// Windows has no `ps`; `tasklist` names every process, as CSV rows.
#[cfg(windows)]
fn process_listing() -> std::process::Command {
    use std::os::windows::process::CommandExt;
    /// No console window opens beside the app.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let system = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| r"C:\Windows".into());
    let mut command = std::process::Command::new(system.join("System32").join("tasklist.exe"));
    command
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW);
    command
}

/// `tasklist`'s `"name.exe","pid",…` rows as the `pid name` lines `ps`
/// prints, the name lowercased and without `.exe`.
#[cfg(any(windows, test))]
fn tasklist_listing(csv: &str) -> String {
    csv.lines()
        .filter_map(|line| {
            let mut fields = line.trim().split("\",\"");
            let name = fields.next()?.strip_prefix('"')?.to_ascii_lowercase();
            let pid = fields.next()?.trim_end_matches('"');
            let name = name.strip_suffix(".exe").unwrap_or(&name);
            Some(format!("{pid} {name}\n"))
        })
        .collect()
}

fn refuse_legacy_listing(listing: &str) -> Result<(), String> {
    for line in listing.lines() {
        let executable = line
            .trim()
            .split_once(char::is_whitespace)
            .map(|(_, exe)| exe.trim())
            .unwrap_or("");
        let name = std::path::Path::new(executable)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name == "buzz-desktop"
            || executable.contains("/Buzz.app/Contents/MacOS/")
            || executable.contains("/Buzz Dev.app/Contents/MacOS/")
        {
            return Err(
                "Stop old Buzz before starting agents here; simultaneous ownership is unsupported"
                    .into(),
            );
        }
    }
    Ok(())
}

impl AgentHost {
    /// Starts the agents queued to start with the host, one at a time.
    pub(crate) fn restore(self: &std::sync::Arc<Self>) {
        let host = self.clone();
        self.runtime.spawn(async move {
            let ids = host
                .run(|owner| {
                    let ids = match owner.controller.launch_ids() {
                        Ok(ids) => ids,
                        Err(error) => {
                            for (id, _) in std::mem::take(&mut owner.queued) {
                                owner.controller.record_error(&id, error.clone());
                            }
                            return Err(error);
                        }
                    };
                    let mut queued = std::mem::take(&mut owner.queued);
                    owner.queued = ids
                        .into_iter()
                        .filter(|id| !owner.acted.contains(id))
                        .map(|id| {
                            let replay = queued.remove(&id).flatten();
                            (id, replay)
                        })
                        .collect();
                    Ok(owner.queued.keys().cloned().collect::<Vec<_>>())
                })
                .await
                .unwrap_or_default();
            for id in ids {
                if let Err(error) = host
                    .start_agent(
                        id.clone(),
                        Action::Start,
                        StartOptions {
                            restore: true,
                            ..Default::default()
                        },
                    )
                    .await
                {
                    tracing::warn!(agent = %id, %error, "agent did not start with the host");
                }
            }
        });
    }

    /// Starts or restarts an agent. Returns the snapshot after the attempt;
    /// a failed launch shows as the agent's error, not as `Err`.
    pub(crate) async fn start_agent(
        &self,
        id: String,
        action: Action,
        options: StartOptions,
    ) -> Result<AgentsSnapshot, String> {
        let StartOptions {
            restore,
            replay_floor,
            owner: signed_in,
            guard,
        } = options;
        let target = id.clone();
        let prepared = self
            .run_text(move |owner| {
                let id = target;
                // A GUI's host starts only the signed-in key's agents.
                let gate = owner.gate_owner()?;
                let queued_replay = owner.queued.remove(&id).flatten();
                if restore
                    && (owner.acted.contains(&id) || !owner.controller.launch_ids()?.contains(&id))
                {
                    return Err("Agent disabled before restore".into());
                }
                // Re-checked on the host thread: Stop or Edit may have
                // changed the agent since the caller chose it.
                owner.check_guard(&id, guard)?;
                if owner.starts.contains_key(&id) {
                    return Err("Agent start already in progress; use Stop to cancel".into());
                }
                if !restore {
                    owner.acted.insert(id.clone());
                }
                let request = match owner.controller.credential_request(&id) {
                    Ok(request) => request,
                    Err(error) => {
                        owner.controller.record_error(&id, error.clone());
                        return Err(error);
                    }
                };
                if queued_replay
                    .as_ref()
                    .is_some_and(|replay| replay.revision != request.2)
                {
                    let error = "Saved settings changed; retry Start for pending mentions";
                    owner.controller.record_error(&id, error.into());
                    return Err(error.into());
                }
                let replay_floor = queued_replay.map_or(replay_floor, |replay| {
                    Some(replay_floor.map_or(replay.floor, |floor| floor.min(replay.floor)))
                });
                if let Err(error) = owner.refuse_legacy(&id) {
                    owner.controller.record_error(&id, error.clone());
                    return Err(error);
                }
                owner.next_start = owner
                    .next_start
                    .checked_add(1)
                    .ok_or("Start sequence exhausted")?;
                let ticket = owner.next_start;
                owner.starts.insert(
                    id.clone(),
                    PendingStart {
                        ticket,
                        workspace: request.3.clone(),
                        status: ProcessStatus::Waiting,
                        revision: request.2,
                        replay_floor,
                    },
                );
                let attested = owner.controller.attested_owner(&id)?;
                Ok((request, ticket, owner.credentials.clone(), attested, gate))
            })
            .await?;
        let ((credential, pubkey, revision, _workspace), ticket, credentials, attested, gate) =
            prepared;
        let signed_in = signed_in.or(gate);
        let _ = Self::tools_path().await;
        let target = id.clone();
        let pi = self
            .run_text(move |owner| {
                owner.pending(&target, ticket)?;
                Ok(owner.controller.pi_launch_context(&target, revision))
            })
            .await?;
        let probed_pi = matches!(&pi, Ok(Some(_)));
        let preflight = match pi {
            Ok(Some(pi)) => buzz_desktop::pi_models::verify(pi)
                .await
                .map(|pi| buzz_agent_controller::pi::LaunchPreflight::new(Some(pi))),
            Ok(None) => Ok(buzz_agent_controller::pi::LaunchPreflight::new(None)),
            Err(error) => Err(error),
        };
        if probed_pi && preflight.is_ok() {
            let target = id.clone();
            self.run_text(move |owner| owner.pending(&target, ticket))
                .await?;
        }
        // Kept agents answer to the owner who authorized them: a start
        // asked for by someone else is refused before the key is read.
        let ready = match &signed_in {
            Some(signed_in) => {
                buzz_agent_controller::check_owner(attested.as_deref(), Some(signed_in))
            }
            None => Ok(()),
        }
        .and(preflight.as_ref().map(drop).map_err(Clone::clone));
        let target = id.clone();
        self.run_text(move |owner| owner.pending(&target, ticket))
            .await?;
        let acquired = match ready {
            Err(error) => Err(error),
            Ok(()) => tokio::task::spawn_blocking(move || {
                if !restore && replay_floor.is_none() && guard.is_none() {
                    credentials.retry();
                }
                credentials.read(&credential, &pubkey)
            })
            .await
            .map_err(|_| "Native credential operation failed".to_string())
            .and_then(|read| read)
            .and_then(|key| {
                key.ok_or_else(|| "Saved agent key is unavailable; nothing was started".into())
            }),
        };
        if acquired.is_ok() {
            let target = id.clone();
            self.run_text(move |owner| {
                owner.pending(&target, ticket)?;
                if let Some(pending) = owner.starts.get_mut(&target) {
                    pending.status = ProcessStatus::Starting;
                }
                Ok(())
            })
            .await?;
        }
        self.run_text(move |owner| {
            let replay_floor = owner.take_start(&id, ticket)?.replay_floor;
            let key = match acquired {
                Ok(key) => key,
                Err(error) => {
                    owner.controller.record_error(&id, error);
                    return owner.snapshot();
                }
            };
            if let Err(error) = owner.refuse_legacy(&id) {
                owner.controller.record_error(&id, error);
                return owner.snapshot();
            }
            // A credential prompt can outlast the agent's eligibility.
            owner.check_guard(&id, guard)?;
            let preflight = preflight?;
            if let Err(error) = owner.controller.action_with_preflight(
                &id,
                action,
                revision,
                &key,
                replay_floor,
                &preflight,
            ) {
                owner.controller.record_error(&id, error);
            }
            owner.snapshot()
        })
        .await
    }

    /// Runs a save, then restarts running agents whose effective settings
    /// it changed. Stopped agents stay stopped. Returns how many restarted
    /// and how many did not.
    pub(crate) async fn save_and_restart(
        &self,
        save: impl FnOnce(&mut Owner) -> Result<(), String> + Send + 'static,
    ) -> Result<(u32, u32), buzz_host_rpc::RpcError> {
        let changed = self
            .run(move |owner| {
                let before = owner.controller.running_settings()?;
                save(owner)?;
                let after = owner.controller.running_settings()?;
                Ok(changed_running(&before, &after))
            })
            .await?;
        let (mut restarted, mut failures) = (0, 0);
        for id in changed {
            let result = self
                .start_agent(
                    id.clone(),
                    Action::Restart,
                    StartOptions {
                        guard: Some((needs_save_restart, NO_SAVE_RESTART)),
                        ..Default::default()
                    },
                )
                .await;
            match restart_outcome(&id, result) {
                Outcome::Restarted => restarted += 1,
                Outcome::Skipped => {}
                Outcome::Failed => failures += 1,
            }
        }
        Ok((restarted, failures))
    }

    /// Restarts an agent that waited for Pi, after a Pi install: whether it
    /// runs now, or `None` when it no longer waited.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    pub(crate) async fn restart_waiting_for_pi(&self, id: String) -> Option<bool> {
        let result = self
            .start_agent(
                id.clone(),
                Action::Restart,
                StartOptions {
                    guard: Some((waiting_for_pi, NOT_WAITING_FOR_PI)),
                    ..Default::default()
                },
            )
            .await;
        match result {
            Err(error) if error == NOT_WAITING_FOR_PI => None,
            Ok(snapshot) => {
                Some(snapshot.agents.iter().any(|agent| {
                    agent.id == id && agent.status == crate::api::ProcessStatus::Running
                }))
            }
            Err(_) => Some(false),
        }
    }

    /// Agents a Pi install should restart.
    #[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
    pub(crate) async fn waiting_for_pi(&self) -> Result<Vec<String>, String> {
        self.run_text(|owner| {
            Ok(owner
                .controller
                .snapshot()?
                .agents
                .iter()
                .filter(|agent| waiting_for_pi(agent))
                .map(|agent| agent.id.clone())
                .collect())
        })
        .await
    }
}

enum Outcome {
    Restarted,
    /// No longer needed, or a Stop won: not a failure.
    Skipped,
    Failed,
}

fn restart_outcome(id: &str, result: Result<AgentsSnapshot, String>) -> Outcome {
    match result {
        Err(error) if error == NO_SAVE_RESTART || error == START_CANCELLED => Outcome::Skipped,
        Err(_) => Outcome::Failed,
        Ok(snapshot) => match snapshot.agents.iter().find(|agent| agent.id == id) {
            // Only a launch of the saved settings counts as a restart.
            Some(agent)
                if agent.enabled
                    && agent.status == crate::api::ProcessStatus::Running
                    && agent.restart_diff.is_empty() =>
            {
                Outcome::Restarted
            }
            Some(agent) if !agent.enabled => Outcome::Skipped,
            _ => Outcome::Failed,
        },
    }
}

/// Agents running both before and after a save whose launch settings
/// differ.
fn changed_running(
    before: &BTreeMap<String, serde_json::Value>,
    after: &BTreeMap<String, serde_json::Value>,
) -> Vec<String> {
    after
        .iter()
        .filter(|(id, settings)| before.get(*id).is_some_and(|old| old != *settings))
        .map(|(id, _)| id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_buzz_processes_refuse_a_start() {
        assert!(refuse_legacy_listing("  1 /usr/bin/buzz-desktop\n").is_err());
        assert!(refuse_legacy_listing("12 /Applications/Buzz.app/Contents/MacOS/Buzz\n").is_err());
        assert!(refuse_legacy_listing("1 /usr/bin/bash\n2 buzz-gpui\n").is_ok());
    }

    #[test]
    fn tasklist_rows_read_as_ps_lines() {
        let csv = "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\
                   \"Buzz-Desktop.EXE\",\"4120\",\"Console\",\"1\",\"91,204 K\"\r\n\
                   \"buzz-gpui.exe\",\"77\",\"Console\",\"1\",\"1,024 K\"\r\n";
        assert_eq!(
            tasklist_listing(csv),
            "0 system idle process\n4120 buzz-desktop\n77 buzz-gpui\n"
        );
        assert!(refuse_legacy_listing(&tasklist_listing(csv)).is_err());
        assert!(refuse_legacy_listing(&tasklist_listing("\"buzz-gpui.exe\",\"7\"\r\n")).is_ok());
    }

    #[test]
    fn only_running_agents_with_changed_settings_restart() {
        let before = BTreeMap::from([
            ("a".to_string(), serde_json::json!(1)),
            ("b".to_string(), serde_json::json!(1)),
        ]);
        let after = BTreeMap::from([
            ("a".to_string(), serde_json::json!(2)),
            ("b".to_string(), serde_json::json!(1)),
            ("c".to_string(), serde_json::json!(1)),
        ]);
        assert_eq!(changed_running(&before, &after), ["a"]);
    }
}
