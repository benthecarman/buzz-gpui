//! `$XDG_CONFIG_HOME/buzz-gpui/config.toml`.
//!
//! ```toml
//! [embedded]
//! credentials = "keyring"   # or "files" on machines without a keyring
//!
//! [[host]]
//! name = "devserver"
//! address = "tcp://devserver.tailnet.ts.net:7420"
//! # The token is read from the OS keyring; `token = "..."` also works.
//! ```

use serde::Deserialize;
use std::path::PathBuf;

/// Keyring service holding remote host tokens, one account per address.
pub const HOST_TOKEN_SERVICE: &str = "dev.local.buzz.gpui.host-tokens";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub embedded: Embedded,
    #[serde(rename = "host")]
    pub hosts: Vec<RemoteHost>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Embedded {
    pub credentials: CredentialsBackend,
    /// Default working directory for agents on this machine.
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialsBackend {
    #[default]
    Keyring,
    Files,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteHost {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub token: Option<String>,
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = dirs::config_dir().map(|dir| dir.join("buzz-gpui/config.toml")) else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                tracing::error!(path = %path.display(), %error, "invalid config; using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }
}
