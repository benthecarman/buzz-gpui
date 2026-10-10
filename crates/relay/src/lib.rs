//! Client for Buzz relays.

pub mod account;
pub mod archive;
pub mod budget;
pub mod community;
pub mod discovery;
pub mod dm;
pub mod emoji;
pub mod event;
pub mod feedback;
pub mod fold;
pub mod gifs;
pub mod http;
pub mod lifecycle;
pub mod links;
pub mod live;
pub mod media;
pub mod members;
pub mod messages;
pub mod origin;
pub mod outbox;
pub mod presence;
pub mod profiles;
pub mod read_state;
pub mod reader;
pub mod search;
pub mod session;
pub mod sidebar;
pub mod sidebar_store;
pub mod socket;
pub mod store;
pub mod threads;
pub mod typing;
pub mod unread;
pub mod upload;
pub mod user_status;
pub mod window;

#[cfg(test)]
mod testing;

pub use account::{LeaveOutcome, ProfileEdit, ProfileError};
pub use discovery::{ChannelSummary, ChannelType, DiscoveryState, RosterScan, Visibility};
pub use emoji::{CatalogStatus, EmojiSnapshot};
pub use event::{Verified, Verifier, kinds};
pub use fold::{FoldOptions, MessageProjection, MessageRow, fold_messages};
pub use http::{HttpError, RelayHttp};
pub use links::{BuzzLink, message_link, parse_buzz_link, profile_key};
pub use live::{
    Live, LiveConnection, LiveOptions, LiveRoute, LiveRouteStatus, LiveSnapshot, LiveUpdate, Phase,
    Replay, RouteKey, RouteStatus,
};
pub use media::{Media, MediaError};
pub use messages::{Attachment, CustomEmoji, Draft, EventTemplate, Mentions, MessageError};
pub use nostr::prelude::{EventId, PublicKey};
pub use origin::RelayOrigin;
pub use outbox::{
    Delivery, DeliveryWork, HttpPublisher, Journal, MemoryJournal, Operation, Outbox, OutboxError,
    OutboxOptions, Publisher, Recovery, SendObservation, SendObserver, SendOptions,
};
pub use presence::{PresenceMode, PresenceState};
pub use profiles::{Profile, ProfileDirectory, Profiles};
pub use read_state::{
    MarkCoverage, PublishCommand, PublishFailure, ReadCapability, ReadJournal, ReadLookup,
    ReadMutationResult, ReadState, ReadStateError, ReadStateJournal, ReadStateSync,
    ReadSyncSnapshot, ReadSyncStatus, ReadTarget, ReadUpdate, SnapshotDescriptor,
};
pub use reader::{Events, Priority, ReadError, ReadErrorKind, ReadOptions, Reader};
pub use session::{
    CatchUpState, ChannelUnread, ChannelView, FeedbackState, HeadCatchUp, IncomingMessage,
    LiveView, MessageSearch, PendingFeedback, PeoplePage, PublicChannels, READING_DWELL_MS,
    RelayInfo, RosterRefresh, RosterStatus, SearchOutcome, SendObserverHandle, Session,
    SessionSnapshot, SessionStatus, StatusWatch, ThreadUnread, TimelineSnapshot, TimelineStatus,
    UnreadCounts, UnreadDetail, UnreadWatch,
};
pub use sidebar::{
    ChannelActivity, Coordinate, SidebarError, SidebarPreferences, SidebarSection, SidebarWrite,
    sidebar_sections,
};
pub use socket::{
    Cooldown, Filter, PresenceSend, PublishError, RelaySocket, SocketOptions, SocketPublisher,
    SocketState, Subscription, SubscriptionEvent,
};
pub use store::{
    Partition, SqliteJournal, SqliteReadJournal, SqliteThreadFollows, Store, StoreError,
};
pub use threads::{
    Completion, TargetStatus, ThreadConfig, ThreadRead, ThreadSnapshot, ThreadState, ThreadStatus,
    thread_authority, verify_thread_windows,
};
pub use unread::{
    AttentionStatus, Category, ChannelInfo, Freshness, InboxItem, InboxSnapshot, Manual,
    MessageAttention, ThreadActivityItem, ThreadActivitySnapshot, ThreadFollowStore, Unread,
    UnreadError, UnreadOptions, UnreadSnapshot,
};
pub use upload::{UploadError, UploadFile};
pub use user_status::{StatusInput, Statuses, UserStatus};
