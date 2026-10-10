//! Agent snapshots: portable agent configuration files, as buzz-app
//! exports, imports and shares them. The dialogs are in
//! `ui/agents/{snapshots,share,snapshot_preview}.rs`.

pub mod manifest;
pub mod memory;
pub mod png;
pub mod send;
pub mod share;

/// What a snapshot file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Agent,
    Team,
}

impl Kind {
    /// The lowercase noun the copy uses ("agent", "team").
    pub fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Team => "team",
        }
    }

    /// The capitalized noun ("Agent", "Team").
    pub fn title(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Team => "Team",
        }
    }
}
