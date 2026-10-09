//! Client for Buzz relays.

pub mod discovery;
pub mod event;
pub mod fold;
pub mod http;
pub mod live;
pub mod origin;
pub mod profiles;
pub mod reader;
pub mod sidebar;
pub mod socket;
pub mod store;
pub mod window;

pub use discovery::{ChannelSummary, ChannelType, DiscoveryState, RosterScan, Visibility};
pub use event::{Verified, Verifier, kinds};
pub use fold::{FoldOptions, MessageProjection, MessageRow, fold_messages};
pub use http::{HttpError, RelayHttp};
pub use live::{Live, LiveOptions, LiveUpdate, Phase, RouteKey, RouteStatus};
pub use origin::RelayOrigin;
pub use profiles::{Profile, ProfileDirectory, Profiles};
pub use reader::{Priority, ReadError, ReadErrorKind, ReadOptions, Reader};
pub use sidebar::{
    ChannelActivity, Coordinate, SidebarError, SidebarPreferences, SidebarSection, SidebarWrite,
    sidebar_sections,
};
pub use socket::{
    Filter, PublishError, RelaySocket, SocketOptions, SocketState, Subscription, SubscriptionEvent,
};
pub use store::{Partition, Store, StoreError};
