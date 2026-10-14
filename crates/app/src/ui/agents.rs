//! What agent views share: form layout, profile pictures, the Add host
//! dialog, community origins and the community's identity archives.

pub(crate) mod form;
mod host_dialog;
pub(crate) mod inventory;
mod picture;
pub(crate) mod relay;

pub(crate) use host_dialog::{AddHost, Dismissed};
pub(crate) use picture::{choose_picture, upload_agent_picture};
