//! What the agent dialogs share: their events and save copy.

use crate::hosts::HostConnection;
use buzz_host_rpc::ConnectionState;
use buzz_host::api::{AgentEdit, Request, Response};
use gpui_kit::{App, AppContext, Entity, SharedString, Task};

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
/// same `request_id` reuses the key. Resolves to the new agent's id.
pub fn create_agent(
    host: &Entity<HostConnection>,
    request_id: String,
    relay_url: String,
    identity: std::sync::Arc<buzz_identity::Identity>,
    edit: AgentEdit,
    cx: &mut App,
) -> Task<Result<String, String>> {
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
    cx.spawn(async move |cx| match prepare.await {
        Ok(Response::Prepared { id, pubkey }) => {
            let auth = identity
                .attest_agent(&pubkey)
                .map_err(|error| error.to_string())?;
            let commit = host
                .update(cx, |host, cx| {
                    host.request(
                        Request::CreateCommit {
                            request_id,
                            edit,
                            auth,
                        },
                        cx,
                    )
                })
                .map_err(|_| "The host went away.".to_string())?;
            commit.await.map(|_| id).map_err(|e| e.to_string())
        }
        Ok(_) => Err("The host sent an unexpected reply.".into()),
        Err(error) => Err(error.to_string()),
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
