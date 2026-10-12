//! What the agent dialogs share: their events and save copy.

use crate::hosts::HostConnection;
use crate::hosts::control::{Failure, Lane};
use buzz_host::api::{AgentEdit, Request, Response};
use buzz_host_rpc::{ConnectionState, RpcError};
use gpui_kit::{App, Entity, SharedString, Task};

/// The dialog is done; the page closes it.
pub struct Dismissed;

/// Settings were saved; the page confirms it with a toast.
pub struct Saved(pub SharedString);

/// buzz-app's `savedMessage`: what a save restarted.
pub fn saved_message(restarted: u32, failures: u32) -> String {
    let agents = |n: u32| format!("{n} agent{}", if n == 1 { "" } else { "s" });
    let saved = if restarted == 0 {
        "Saved.".to_string()
    } else {
        format!("Saved. Restarted {}.", agents(restarted))
    };
    if failures == 0 {
        saved
    } else {
        format!(
            "{saved} {} couldn’t restart with the new settings; check Agents.",
            agents(failures)
        )
    }
}

/// Creates an agent on `host` in `relay_url`: the host generates its key,
/// `identity` attests it, and the host saves it, stopped. Retrying with the
/// same `request_id` reuses the key, and skips the commit when an earlier
/// try already saved it. Resolves to the new agent's id.
pub fn create_agent(
    host: &Entity<HostConnection>,
    request_id: String,
    relay_url: String,
    identity: std::sync::Arc<buzz_identity::Identity>,
    edit: AgentEdit,
    cx: &mut App,
) -> Task<Result<String, Failure>> {
    let failure = |message: String, reason: Option<String>| Failure { message, reason };
    let prepare = host.update(cx, |host, cx| {
        host.request(
            Request::CreatePrepare {
                request_id: request_id.clone(),
                relay_url,
                owner: identity.pubkey_hex(),
            },
            cx,
        )
    });
    let host = host.downgrade();
    cx.spawn(async move |cx| {
        let (id, pubkey) = match prepare.await {
            Ok(Response::Prepared {
                id, saved: true, ..
            }) => return Ok(id),
            Ok(Response::Prepared { id, pubkey, .. }) => (id, pubkey),
            Ok(_) => return Err(failure("The host sent an unexpected reply.".into(), None)),
            Err(RpcError::Failed(reason)) => return Err(failure(reason.clone(), Some(reason))),
            Err(error) => return Err(failure(error.to_string(), None)),
        };
        let auth = identity.attest_agent(&pubkey).map_err(|error| {
            let error = error.to_string();
            failure(error.clone(), Some(error))
        })?;
        let commit = host
            .update(cx, |host, cx| {
                host.write(
                    Request::CreateCommit {
                        request_id,
                        edit,
                        auth,
                    },
                    Lane {
                        credential_write: true,
                        ..Default::default()
                    },
                    cx,
                )
            })
            .map_err(|_| failure("The host went away.".into(), None))?;
        commit.await.map(|_| id)
    })
}

pub fn connected(host: &Entity<HostConnection>, cx: &App) -> bool {
    matches!(host.read(cx).state, ConnectionState::Connected { .. })
}

#[cfg(test)]
mod tests {
    use super::saved_message;

    #[test]
    fn saved_messages_follow_the_reference() {
        assert_eq!(saved_message(0, 0), "Saved.");
        assert_eq!(saved_message(1, 0), "Saved. Restarted 1 agent.");
        assert_eq!(
            saved_message(2, 1),
            "Saved. Restarted 2 agents. 1 agent couldn’t restart with the new settings; \
             check Agents."
        );
    }
}
