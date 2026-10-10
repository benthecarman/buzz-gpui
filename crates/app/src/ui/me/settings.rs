//! "Session settings" (`sessions/WorkspaceSettings.tsx`,
//! `DraftSessionSettings.tsx`, `WorkspaceFields.tsx`): project folders,
//! a new worktree and Canvas for a session, a Me group's defaults, or a
//! new conversation's draft.

use super::dialogs::DialogEvent;
use super::pending::SessionSetup;
use crate::community::Community;
use buzz_relay_client::Session;
use buzz_relay_client::me_kit::{self as kit, GroupsId, KitEntry, REMOVED_SECTION};
use buzz_relay_client::session::AgentRules;
use buzz_relay_client::sidebar_store::PreferencesStatus;
use buzz_ui::{Button, Dialog, Surface};
use gpui_kit::*;
use sha2::{Digest, Sha256};

/// The template record holding a group's session defaults
/// (`sectionTemplateId`): `me-section-<sha256 of the group id>`.
pub fn section_template_id(section: &str, personal: bool) -> String {
    let hash = Sha256::digest(section.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-section-{hex}", if personal { "me" } else { "session" })
}

/// A group's defaults (`sectionDefault`): its own template record, else
/// the group's default template. `require` refuses a default that names
/// a missing template.
pub fn section_default(
    entries: &[KitEntry],
    section: &str,
    personal: bool,
    require: bool,
) -> Result<Option<serde_json::Value>, String> {
    if let Some(own) = kit::template(entries, &section_template_id(section, personal)) {
        return Ok(Some(own.record["value"].clone()));
    }
    let id = if personal {
        GroupsId::Me
    } else {
        GroupsId::Personal
    };
    let default = kit::groups(entries, id).and_then(|groups| {
        groups
            .groups
            .into_iter()
            .find(|group| group.id == section)
            .map(|group| group.default_template_id)
            .filter(|id| !id.is_empty())
    });
    let Some(default) = default else {
        return Ok(None);
    };
    match kit::template(entries, &default) {
        Some(template) => Ok(Some(template.record["value"].clone())),
        None if require => Err("This section’s default template is unavailable. Choose new section defaults before starting a session.".into()),
        None => Ok(None),
    }
}

/// The defaults a new conversation in `section` starts with
/// (`loadSessionSetup`).
pub async fn load_session_setup(
    session: &Session,
    section: &str,
    personal: bool,
) -> Result<SessionSetup, String> {
    let kit = session.me_kit();
    kit.me_refresh();
    let mut state = kit.watch_me();
    let _ = state
        .wait_for(|state| state.status != PreferencesStatus::Loading)
        .await;
    let me = kit.me_state();
    if !me.writable
        || !me
            .data
            .as_ref()
            .is_some_and(|data| data.sections.iter().any(|s| s.id == section))
    {
        return Err(
            "This section is unavailable. Refresh sections before starting a session.".into(),
        );
    }
    kit.refresh().await;
    let snapshot = kit.snapshot();
    if !snapshot.ready() {
        return Err(snapshot
            .error()
            .unwrap_or("Section defaults couldn’t load. Retry before starting the session.")
            .to_owned());
    }
    let Some(template) = section_default(&snapshot.entries, section, personal, true)? else {
        return Ok(SessionSetup {
            section_id: Some(section.to_owned()),
            canvas: String::new(),
            agents: Vec::new(),
        });
    };
    let mut agents: Vec<String> = template["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|key| key.as_str().map(str::to_owned))
        .collect();
    for team in template["teamIds"].as_array().into_iter().flatten() {
        let team = team.as_str().unwrap_or_default();
        let entry = snapshot
            .entries
            .iter()
            .find(|e| e.kind() == "team" && e.id() == team && !e.deleted())
            .ok_or_else(|| {
                format!("A selected team is unavailable ({team}); remove or replace it")
            })?;
        for key in entry.record["value"]["agents"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(key) = key.as_str()
                && !agents.iter().any(|known| known == key)
            {
                agents.push(key.to_owned());
            }
        }
    }
    Ok(SessionSetup {
        section_id: Some(section.to_owned()),
        canvas: template["canvas"].as_str().unwrap_or_default().to_owned(),
        agents,
    })
}

/// Applies a new conversation's defaults once it exists
/// (`applySessionSetup`): its Canvas, its group, its agents.
pub async fn apply_session_setup(
    session: &Session,
    channel: &str,
    setup: &SessionSetup,
    personal: bool,
    rules: &AgentRules,
) -> Result<(), String> {
    let kit = session.me_kit();
    let head = kit.canvas_read(channel).await?;
    match head {
        Some(head) if head.event().content != setup.canvas => {
            return Err(
                "This session already has a different Canvas. Review it before retrying setup."
                    .into(),
            );
        }
        None if !setup.canvas.is_empty() => {
            kit.canvas_save(channel, &setup.canvas, None).await?;
        }
        _ => {}
    }
    if let Some(section) = &setup.section_id
        && personal
    {
        kit.me_refresh();
        let mut state = kit.watch_me();
        let _ = state
            .wait_for(|state| state.status != PreferencesStatus::Loading)
            .await;
        let me = kit.me_state();
        let Some(data) = me
            .data
            .as_ref()
            .filter(|_| me.status == PreferencesStatus::Ready)
        else {
            return Err("Sections couldn’t load. Retry before continuing setup.".into());
        };
        if !data.sections.iter().any(|s| s.id == *section) {
            return Err(REMOVED_SECTION.into());
        }
        if data.assignments.get(channel) != Some(section) {
            kit.me_assign(channel, Some(section.clone())).await?;
        }
    }
    if !setup.agents.is_empty() {
        session.add_agents(channel, &setup.agents, rules).await?;
    }
    Ok(())
}

/// What the settings are for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsTarget {
    /// A conversation's Canvas.
    Session { id: String, name: String },
    /// A Me group's defaults for new conversations.
    Section { id: String, name: String },
    /// A new conversation before it starts: saved on this device under
    /// its draft key, applied by the first send.
    Draft {
        key: String,
        section: Option<String>,
    },
}

pub struct WorkspaceSettings {
    _community: Entity<Community>,
    _target: SettingsTarget,
    busy: bool,
}

impl EventEmitter<DialogEvent> for WorkspaceSettings {}

impl WorkspaceSettings {
    pub fn new(
        community: Entity<Community>,
        target: SettingsTarget,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Self {
        Self {
            _community: community,
            _target: target,
            busy: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }
}

impl Render for WorkspaceSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Dialog::new("me-session-settings", "Session settings")
            .on_close(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close)))
            .footer(
                Button::new("me-settings-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close))),
            )
    }
}
