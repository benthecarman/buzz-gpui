//! The app's views.

mod account;
mod add_community;
mod agents;
mod channel;
mod channel_menu;
mod composer;
mod emoji_picker;
mod gif_picker;
mod markdown;
mod media;
mod members;
mod message;
mod modal;
pub mod navigation;
mod new_channel;
mod new_message;
mod onboarding;
mod overlay;
mod profile_card;
mod search;
pub mod settings;
mod sidebar;
/// Declared here until main.rs declares `mod sounds;`.
#[path = "../sounds.rs"]
pub(crate) mod sounds;
mod status;
mod thread;
mod time;
mod typing;
mod window_controls;
pub mod workspace;
