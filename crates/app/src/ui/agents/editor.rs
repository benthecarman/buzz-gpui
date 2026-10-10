//! What the agent dialogs share: their events and save copy.

use crate::hosts::HostConnection;
use buzz_host_rpc::ConnectionState;
use gpui_kit::{App, Entity, SharedString};

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
