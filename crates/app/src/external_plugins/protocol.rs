//! What a plugin folder holds and what its program says (`docs/plugins.md`).
//!
//! A plugin is a program the app starts. It speaks JSON-RPC 2.0, one
//! message per line, on stdin and stdout, so a short script in any
//! language can be one. The app asks it what it contributes, opens views,
//! and forwards clicks and typing; the plugin answers with element trees
//! that the app draws with its own components.

use serde::Deserialize;
use serde_json::{Value, json};

/// The protocol version the app speaks.
pub const API_VERSION: u32 = 1;

/// The manifest's file name in a plugin folder.
pub const MANIFEST: &str = "buzz-plugin.json";

/// Longest line the app reads from a plugin.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

const MAX_PANELS: usize = 16;
const MAX_PAGES: usize = 8;
const MAX_NODES: usize = 5_000;
const MAX_DEPTH: usize = 32;

/// `buzz-plugin.json`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Reverse-DNS style, such as `com.example.weather`. Unique per app.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The program and its arguments. A program with a `/` is relative to
    /// the plugin folder; otherwise it is found on `PATH`.
    pub command: Vec<String>,
    #[serde(default = "api_version")]
    pub api_version: u32,
}

fn api_version() -> u32 {
    API_VERSION
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self =
            serde_json::from_str(text).map_err(|error| format!("{MANIFEST}: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), String> {
        if !valid_plugin_id(&self.id) {
            return Err(format!(
                "“{}” is not a plugin id. Use lowercase letters, digits, “.”, “-” and “_”, \
                 such as com.example.weather.",
                self.id
            ));
        }
        if self.id == "buzz" || self.id.starts_with("buzz.") {
            return Err("Plugin ids starting with “buzz.” are reserved for Buzz.".into());
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 64 {
            return Err("The plugin name must be 1 to 64 characters.".into());
        }
        if self.command.is_empty() || self.command[0].trim().is_empty() {
            return Err("The manifest has no command to run.".into());
        }
        if self.command.len() > 64 || self.command.iter().any(|arg| arg.contains('\0')) {
            return Err("The manifest's command is not valid.".into());
        }
        if self.api_version != API_VERSION {
            return Err(format!(
                "This plugin needs plugin API {}; this app speaks {API_VERSION}.",
                self.api_version
            ));
        }
        Ok(())
    }

    /// The command as the import disclosure shows it.
    pub fn command_line(&self) -> String {
        self.command
            .iter()
            .map(|arg| {
                if arg.is_empty() || arg.contains(char::is_whitespace) {
                    format!("\"{arg}\"")
                } else {
                    arg.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn valid_plugin_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 128
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

fn valid_contribution_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_".contains(c))
}

/// The `initialize` result: what the plugin adds to the app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contributions {
    #[serde(default)]
    pub panels: Vec<PanelDecl>,
    #[serde(default)]
    pub pages: Vec<PageDecl>,
}

/// A panel beside the conversation (`panels.register`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelDecl {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
    /// Link prefixes the panel shows, such as `https://example.com/`. A
    /// matching link clicked in a message opens the panel.
    #[serde(default)]
    pub matches: Vec<String>,
    /// A button in the channel header that toggles the panel.
    #[serde(default)]
    pub launcher: Option<LauncherDecl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherDecl {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub placement: LauncherPlacement,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LauncherPlacement {
    #[default]
    Side,
    Bottom,
}

/// A page with a row in the channel sidebar (`pages.register`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDecl {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
}

impl Contributions {
    pub fn parse(value: Value) -> Result<Self, String> {
        let contributions: Self = serde_json::from_value(value)
            .map_err(|error| format!("The plugin's initialize result is not valid: {error}"))?;
        if contributions.panels.len() > MAX_PANELS || contributions.pages.len() > MAX_PAGES {
            return Err(format!(
                "A plugin can add at most {MAX_PANELS} panels and {MAX_PAGES} pages."
            ));
        }
        let ids = contributions
            .panels
            .iter()
            .map(|panel| (&panel.id, &panel.title))
            .chain(
                contributions
                    .pages
                    .iter()
                    .map(|page| (&page.id, &page.title)),
            );
        let mut seen = std::collections::HashSet::new();
        for (id, title) in ids {
            if !valid_contribution_id(id) {
                return Err(format!(
                    "“{id}” is not a contribution id. Use lowercase letters, digits, “-” and “_”."
                ));
            }
            if !seen.insert(id) {
                return Err(format!("The plugin adds “{id}” twice."));
            }
            if title.trim().is_empty() || title.chars().count() > 64 {
                return Err(format!("“{id}” needs a title of 1 to 64 characters."));
            }
        }
        if let Some(panel) = contributions
            .panels
            .iter()
            .find(|panel| panel.matches.iter().any(|prefix| prefix.len() < 4))
        {
            return Err(format!(
                "“{}” matches links too broadly. Use a prefix such as https://example.com/.",
                panel.id
            ));
        }
        Ok(contributions)
    }
}

/// One element of a view, as the plugin sends it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Node {
    Column {
        #[serde(default)]
        children: Vec<Node>,
        #[serde(default)]
        gap: Gap,
    },
    Row {
        #[serde(default)]
        children: Vec<Node>,
        #[serde(default)]
        gap: Gap,
        #[serde(default)]
        justify: Justify,
        #[serde(default)]
        wrap: bool,
    },
    Card {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        text: String,
        #[serde(default)]
        style: TextStyle,
        #[serde(default)]
        tone: Tone,
    },
    Markdown {
        text: String,
    },
    Button {
        id: String,
        label: String,
        #[serde(default)]
        variant: ButtonStyle,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        disabled: bool,
        #[serde(default)]
        loading: bool,
    },
    Link {
        label: String,
        url: String,
    },
    /// A text field. The app keeps what is typed, so typing never waits
    /// on the plugin; each change and Enter is sent as an event.
    Input {
        id: String,
        /// The text the field starts with.
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        placeholder: Option<String>,
        /// Bumping it puts `value` into the field again.
        #[serde(default)]
        version: u64,
        /// Empties the field when Enter sends it, without waiting on the
        /// plugin.
        #[serde(default)]
        clear_on_submit: bool,
    },
    Switch {
        id: String,
        checked: bool,
        #[serde(default)]
        label: Option<String>,
    },
    Badge {
        text: String,
        #[serde(default)]
        tone: Tone,
    },
    Icon {
        name: String,
    },
    Divider,
    Spinner,
    /// An element this app does not know; it draws nothing.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Gap {
    None,
    #[default]
    Sm,
    Md,
    Lg,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    Between,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextStyle {
    #[default]
    Body,
    Small,
    Label,
    Title,
    Heading,
    Code,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    #[default]
    Default,
    Subtle,
    Accent,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ButtonStyle {
    Prominent,
    #[default]
    Subtle,
    Ghost,
    Outline,
    Destructive,
}

impl Node {
    /// Parses a `view/render` tree and checks its size.
    pub fn parse(value: Value) -> Result<Self, String> {
        let node: Self = serde_json::from_value(value)
            .map_err(|error| format!("The plugin sent a view that is not valid: {error}"))?;
        let mut count = 0;
        node.check(1, &mut count)?;
        Ok(node)
    }

    fn children(&self) -> &[Node] {
        match self {
            Node::Column { children, .. }
            | Node::Row { children, .. }
            | Node::Card { children, .. } => children,
            _ => &[],
        }
    }

    fn check(&self, depth: usize, count: &mut usize) -> Result<(), String> {
        *count += 1;
        if *count > MAX_NODES {
            return Err(format!(
                "The plugin sent a view with over {MAX_NODES} elements."
            ));
        }
        if depth > MAX_DEPTH {
            return Err(format!(
                "The plugin sent a view nested over {MAX_DEPTH} deep."
            ));
        }
        self.children()
            .iter()
            .try_for_each(|child| child.check(depth + 1, count))
    }
}

/// A line from the plugin.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        result: Result<Value, String>,
    },
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RawError>,
}

#[derive(Deserialize)]
struct RawError {
    #[serde(default)]
    message: String,
}

impl Incoming {
    pub fn parse(line: &str) -> Result<Self, String> {
        let raw: Raw =
            serde_json::from_str(line).map_err(|error| format!("not JSON-RPC: {error}"))?;
        let params = raw.params.unwrap_or(Value::Null);
        match (raw.method, raw.id) {
            (Some(method), Some(id)) => Ok(Self::Request { id, method, params }),
            (Some(method), None) => Ok(Self::Notification { method, params }),
            (None, Some(id)) => {
                let id = id.as_u64().ok_or("a response id the app did not send")?;
                let result = match raw.error {
                    Some(error) => Err(error.message),
                    None => Ok(raw.result.unwrap_or(Value::Null)),
                };
                Ok(Self::Response { id, result })
            }
            (None, None) => Err("a message with no method and no id".into()),
        }
    }
}

pub fn request(id: u64, method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
}

pub fn notification(method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()
}

pub fn response(id: &Value, result: Result<Value, (i64, String)>) -> String {
    match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    }
    .to_string()
}

/// JSON-RPC's "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC's "invalid params".
pub const INVALID_PARAMS: i64 = -32602;

#[cfg(test)]
mod tests;
