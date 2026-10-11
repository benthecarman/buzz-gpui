//! The community rail (`CommunityRail.tsx`, `CommunityRailItem.tsx`):
//! Personal space, one button per community with its NIP-11 icon or
//! initials, and each item's context menu: mark everything read, copy the
//! community URL, invite people to the selected community (owners and
//! admins only), open the community's settings, and leave it.
//!
//! Communities can be reordered by dragging them on the rail or with Move
//! up / Move down in their menu; buzz-app's rail keeps the order they were
//! added in.

use super::modal::Dismissed;
use super::navigation::{Place, Target};
use super::settings::Section;
use super::workspace::Workspace;
use super::{moderation, notice};
use crate::community::{AppState, Community};
use buzz_relay_client::SessionStatus;
use buzz_ui::{
    ActiveTheme, AlertDialog, Avatar, AvatarSize, Button, IconButton, IconName, Menu, MenuItem,
    MenuLabel, MenuSeparator, MenuSize, Surface, ToastTone,
};
use gpui_kit::*;
use std::sync::Arc;

/// The open menu: the rail item and where the pointer opened it.
pub type MenuState = Entity<Option<(usize, Point<Pixels>)>>;

pub fn state(window: &mut Window, cx: &mut App) -> MenuState {
    window.use_keyed_state("community-rail-menu", cx, |_, _| None)
}

/// The rail: Personal space, the communities, then "Add a community".
pub fn rail(window: &mut Window, cx: &mut Context<Workspace>) -> impl IntoElement {
    let menu = state(window, cx);
    let state = cx.global::<AppState>();
    let selected = state.selected;
    let communities = state.communities.clone();
    let mut rail = div()
        .flex()
        .flex_col()
        .items_center()
        .gap(rems(0.5))
        .w(rems(3.))
        .py(rems(0.5))
        .child(
            IconButton::new("personal-space", IconName::Globe)
                .tooltip("Personal space")
                .selected(selected.is_none())
                .on_click(cx.listener(|this, _, window, cx| this.select_personal(window, cx))),
        );
    for (index, community) in communities.iter().enumerate() {
        let (name, icon, media) = {
            let community = community.read(cx);
            (
                community.name(),
                community.icon().map(str::to_owned),
                community.session.media().clone(),
            )
        };
        let image = icon.and_then(|icon| icon_source(&icon, &media));
        let mut avatar = Avatar::new(name.clone()).squircle().size(AvatarSize::Small);
        if let Some(image) = image.clone() {
            avatar = avatar.image(image);
        }
        let menu = menu.clone();
        let dragged = DraggedCommunity {
            index,
            name: name.clone().into(),
            image,
        };
        rail = rail.child(
            div()
                .id(SharedString::from(format!("rail-item-{index}")))
                .rounded(rems(0.5))
                .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                .drag_over::<DraggedCommunity>(|style, _, _, cx| {
                    style.bg(cx.colors().affordance_accent)
                })
                .on_drop(cx.listener(move |_, dragged: &DraggedCommunity, _, cx| {
                    AppState::move_community(dragged.index, index, cx);
                    cx.notify();
                }))
                .on_mouse_down(MouseButton::Right, move |event, _, cx| {
                    open(&menu, index, event.position, cx)
                })
                .child(
                    IconButton::new(
                        SharedString::from(format!("community-{index}")),
                        IconName::Globe,
                    )
                    .tooltip(name)
                    .selected(selected == Some(index))
                    .child(avatar)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_community(index, window, cx)
                    })),
                ),
        );
    }
    rail.children(render(&menu, cx)).child(
        IconButton::new("add-community", IconName::Plus)
            .tooltip("Add a community")
            .on_click(cx.listener(|this, _, window, cx| this.open_add_community(window, cx))),
    )
}

/// A community being dragged on the rail, drawn as its icon under the
/// pointer.
#[derive(Clone)]
struct DraggedCommunity {
    index: usize,
    name: SharedString,
    image: Option<ImageSource>,
}

impl Render for DraggedCommunity {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut avatar = Avatar::new(self.name.clone())
            .squircle()
            .size(AvatarSize::Small);
        if let Some(image) = self.image.clone() {
            avatar = avatar.image(image);
        }
        div().opacity(0.85).child(avatar)
    }
}

/// A community icon: inline icons decode here, HTTPS icons load like any
/// remote image.
fn icon_source(icon: &str, media: &buzz_relay_client::media::Media) -> Option<ImageSource> {
    if !icon.starts_with("data:") {
        return Some(super::media::image(icon, media));
    }
    let format = match icon.strip_prefix("data:image/")?.split([';', ',']).next()? {
        "svg+xml" => ImageFormat::Svg,
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        _ => return None,
    };
    let bytes = buzz_relay_client::community_icon::inline_bytes(icon)?;
    Some(ImageSource::Image(Arc::new(Image::from_bytes(
        format, bytes,
    ))))
}

/// Opens the menu for rail item `index`. For the selected community the
/// roster is read again: registration is not authorization, so "Invite to
/// community" follows the viewer's current role.
pub fn open(state: &MenuState, index: usize, at: Point<Pixels>, cx: &mut App) {
    if cx.global::<AppState>().selected == Some(index) {
        moderation::refresh_role(cx);
    }
    state.update(cx, |open, cx| {
        *open = Some((index, at));
        cx.notify();
    });
}

/// Why "Mark all as read" is unavailable, if it is (`MarkAllReadItem`):
/// read state belongs to the selected community's ready session.
fn mark_all_reason(community: &Community, selected: bool) -> Option<String> {
    if !selected {
        Some("Only the selected community can be marked as read.".into())
    } else if community.snapshot.status != SessionStatus::Ready {
        Some(format!("Waiting for {} to connect.", community.name()))
    } else if community.snapshot.cached {
        Some("Read state can’t sync on this connection.".into())
    } else {
        None
    }
}

fn close(state: &MenuState, cx: &mut App) {
    state.update(cx, |open, cx| {
        *open = None;
        cx.notify();
    })
}

pub fn render(state: &MenuState, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let (index, at) = (*state.read(cx))?;
    let app = cx.global::<AppState>();
    let selected = app.selected == Some(index);
    let community = app.communities.get(index)?.clone();
    let origin = community.read(cx).origin.clone();
    let settings = |section: Section| Target {
        community: Some(origin.to_string()),
        place: Place::Settings(section),
    };
    let reason = mark_all_reason(community.read(cx), selected);
    let mut menu = Menu::new().size(MenuSize::Compact).child(
        MenuItem::new("rail-mark-read", "Mark all as read")
            .icon(IconName::Checks)
            .icon_size(14.)
            .size(MenuSize::Compact)
            .disabled(reason.is_some())
            .on_click({
                let (state, session) = (state.clone(), community.read(cx).session.clone());
                move |_, _, cx| {
                    close(&state, cx);
                    let session = session.clone();
                    cx.spawn(async move |cx| {
                        let result = session.mark_all_read().await;
                        cx.update(|cx| match result {
                            Ok(()) => notice::success("Marked all as read.", cx),
                            Err(_) => notice::error("Couldn’t mark everything as read.", cx),
                        });
                    })
                    .detach();
                }
            }),
    );
    if let Some(reason) = reason {
        menu = menu.child(MenuLabel::new(reason).quiet());
    }
    let url = origin.http();
    menu = menu.child(MenuSeparator::new()).child(
        MenuItem::new("rail-copy-url", "Copy community URL")
            .icon(IconName::Link)
            .icon_size(14.)
            .size(MenuSize::Compact)
            .on_click({
                let state = state.clone();
                move |_, _, cx| {
                    close(&state, cx);
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                    notice::success("Community URL copied.", cx);
                }
            }),
    );
    if selected && moderation::manager(cx) {
        let (state, target) = (
            state.clone(),
            settings(Section::Card(moderation::MEMBERSHIP)),
        );
        menu = menu.child(
            MenuItem::new("rail-invite", "Invite to community")
                .icon(IconName::Ticket)
                .icon_size(14.)
                .size(MenuSize::Compact)
                .on_click(cx.listener(move |this, _, window, cx| {
                    close(&state, cx);
                    this.navigate(target.clone(), window, cx);
                })),
        );
    }
    let count = app.communities.len();
    if count > 1 {
        let (up, down) = (state.clone(), state.clone());
        menu = menu
            .child(MenuSeparator::new())
            .child(
                MenuItem::new("rail-move-up", "Move up")
                    .icon(IconName::ArrowUp)
                    .icon_size(14.)
                    .size(MenuSize::Compact)
                    .disabled(index == 0)
                    .on_click(move |_, _, cx| {
                        close(&up, cx);
                        AppState::move_community(index, index - 1, cx);
                    }),
            )
            .child(
                MenuItem::new("rail-move-down", "Move down")
                    .icon(IconName::ArrowDown)
                    .icon_size(14.)
                    .size(MenuSize::Compact)
                    .disabled(index + 1 >= count)
                    .on_click(move |_, _, cx| {
                        close(&down, cx);
                        AppState::move_community(index, index + 1, cx);
                    }),
            );
    }
    let (close_state, target) = (state.clone(), settings(Section::Profile));
    menu = menu.child(
        MenuItem::new("rail-settings", "Community settings")
            .icon(IconName::Gear)
            .icon_size(14.)
            .size(MenuSize::Compact)
            .on_click(cx.listener(move |this, _, window, cx| {
                close(&close_state, cx);
                this.navigate(target.clone(), window, cx);
            })),
    );
    let leave_state = state.clone();
    menu = menu.child(MenuSeparator::new()).child(
        MenuItem::new("rail-leave", "Leave community")
            .icon(IconName::SignOut)
            .icon_size(14.)
            .size(MenuSize::Compact)
            .danger()
            .on_click(cx.listener(move |this, _, _, cx| {
                close(&leave_state, cx);
                let community = community.clone();
                let dialog = cx.new(|_| LeaveCommunity {
                    community,
                    busy: false,
                });
                this.show_dialog(dialog, cx);
            })),
    );
    let outside = state.clone();
    Some(
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(at)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .occlude()
                        .on_mouse_down_out(move |_, _, cx| close(&outside, cx))
                        .child(menu),
                ),
        )
        .with_priority(2)
        .into_any_element(),
    )
}

/// "Leave <name>?" from the rail. The rail toasts any failure and closes,
/// as buzz-app's rail does; Settings → Communities keeps its own dialog.
struct LeaveCommunity {
    community: Entity<Community>,
    busy: bool,
}

impl EventEmitter<Dismissed> for LeaveCommunity {}

impl LeaveCommunity {
    fn leave(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        let name = self.community.read(cx).name();
        let task = AppState::leave(self.community.clone(), cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |_, cx| {
                match result {
                    Ok((outcome, residue)) => {
                        let (text, tone) = super::settings::left_text(&name, outcome, residue);
                        let timeout = (tone == ToastTone::Success)
                            .then_some(std::time::Duration::from_secs(4));
                        notice::show(text, tone, timeout, cx);
                    }
                    Err(error) => notice::error(leave_failure(&name, &error), cx),
                }
                cx.emit(Dismissed);
            });
        })
        .detach();
        cx.notify();
    }
}

/// `leaveFailureText`: the owner's refusal by name, anything else one retry
/// message.
fn leave_failure(name: &str, error: &str) -> String {
    if error == buzz_relay_client::account::OWNER_CANNOT_LEAVE {
        format!("The relay owner can’t leave {name}.")
    } else {
        format!("Couldn’t leave {name}. Check your connection and try again.")
    }
}

impl Render for LeaveCommunity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.community.read(cx).name();
        AlertDialog::new(format!("Leave {name}?"))
            .description(format!(
                "This sends a leave request to the community’s relay, then removes {name} \
                 from this device along with its saved drafts and reading positions. \
                 Rejoining may need a new invite."
            ))
            .action(
                Button::new("rail-leave-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(self.busy)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
            )
            .action(
                Button::new("rail-leave-confirm", "Leave community")
                    .destructive()
                    .loading(self.busy)
                    .on_click(cx.listener(|this, _, _, cx| this.leave(cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn leave_failures_follow_the_reference() {
        assert_eq!(
            leave_failure("Hive", buzz_relay_client::account::OWNER_CANNOT_LEAVE),
            "The relay owner can’t leave Hive."
        );
        assert_eq!(
            leave_failure("Hive", "timed out"),
            "Couldn’t leave Hive. Check your connection and try again."
        );
    }
}
