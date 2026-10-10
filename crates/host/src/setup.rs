//! Harness setup on this host: installing Pi or Claude Code into the host's
//! own tools (`buzz_desktop::harness_setup`), and checking Claude Code's
//! sign-in.

use crate::api::{HarnessKind, InstallReport};
use crate::{AgentHost, harness};

impl AgentHost {
    /// Installs one harness. One install runs at a time; quitting the host
    /// kills the installer.
    pub(crate) async fn install(&self, target: HarnessKind) -> Result<InstallReport, String> {
        let report = self.install_harness(target).await?;
        crate::convert(&report).map_err(|_| "Invalid install report".into())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn install_harness(
        &self,
        target: HarnessKind,
    ) -> Result<buzz_desktop::harness_setup::InstallReport, String> {
        use buzz_desktop::harness_setup::{Restart, install_claude, install_pi};
        Self::tools_path()
            .await
            .map_err(|error| error.to_string())?;
        let claim = self.setup.claim()?;
        let app_data = harness::tools_root(&self.state_dir).to_path_buf();
        match target {
            HarnessKind::Pi => {
                let restart = |id: String| async move {
                    match self.restart_waiting_for_pi(id).await {
                        None => Restart::Skipped,
                        Some(true) => Restart::Running,
                        Some(false) => Restart::Failed,
                    }
                };
                install_pi(&claim, &app_data, self.waiting_for_pi(), restart).await
            }
            HarnessKind::Claude => install_claude(&claim, &app_data).await,
            _ => Err("This harness is installed outside Buzz".into()),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    async fn install_harness(
        &self,
        target: HarnessKind,
    ) -> Result<buzz_desktop::harness_setup::InstallReport, String> {
        Err(match target {
            HarnessKind::Pi => "Pi installation is supported only on macOS and Linux",
            HarnessKind::Claude => "Claude Code installation is supported only on macOS and Linux",
            _ => "This harness is installed outside Buzz",
        }
        .into())
    }

    /// Runs `claude auth status`: `Some(true)` signed in, `Some(false)`
    /// signed out, `None` when that cannot be confirmed.
    pub(crate) async fn claude_auth(&self) -> Option<bool> {
        Self::tools_path().await.ok()?;
        let claude = harness::claude(harness::tools_root(&self.state_dir));
        let mut path = buzz_agent_controller::tools_path().ok()?;
        if let Some(node_bin) = claude.node.as_ref().and_then(|node| node.parent()) {
            path = std::env::join_paths(
                std::iter::once(node_bin.to_path_buf()).chain(std::env::split_paths(&path)),
            )
            .ok()?;
        }
        let (output, status) = buzz_desktop::process::run_output(
            claude.cli.as_ref()?,
            &["auth".into(), "status".into()],
            std::time::Duration::from_secs(5),
            &path,
            4096,
        )
        .await?;
        let logged_in = serde_json::from_str::<serde_json::Value>(&output)
            .ok()?
            .get("loggedIn")?
            .as_bool()?;
        match (status.code(), logged_in) {
            (Some(0), true) => Some(true),
            (Some(1), false) => Some(false),
            _ => None,
        }
    }
}
