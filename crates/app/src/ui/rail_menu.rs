//! The community rail item's context menu (`CommunityRailItem.tsx`): copy
//! the community URL, invite people to the selected community (owners and
//! admins only) and open the community's settings.

use super::navigation::{Place, Target};
use super::settings::Section;
use super::workspace::Workspace;
use super::{moderation, notice};
use crate::community::AppState;
use buzz_ui::{IconName, Menu, MenuItem, MenuSize};
use gpui_kit::*;

/// The open menu: the rail item and where the pointer opened it.
pub type MenuState = Entity<Option<(usize, Point<Pixels>)>>;

pub fn state(window: &mut Window, cx: &mut App) -> MenuState {
    window.use_keyed_state("community-rail-menu", cx, |_, _| None)
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

pub fn render(state: &MenuState, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let (index, at) = (*state.read(cx))?;
    let app = cx.global::<AppState>();
    let selected = app.selected == Some(index);
    let origin = app.communities.get(index)?.read(cx).origin.clone();
    let close = |state: &MenuState, cx: &mut App| {
        state.update(cx, |open, cx| {
            *open = None;
            cx.notify();
        })
    };
    let settings = |section: Section| Target {
        community: Some(origin.to_string()),
        place: Place::Settings(section),
    };
    let url = origin.http();
    let mut menu = Menu::new().size(MenuSize::Compact).child(
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
