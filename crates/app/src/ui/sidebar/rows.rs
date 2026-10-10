//! Sections and rows (ui.md §3.3–3.5, §3.7; `SidebarSection.tsx`,
//! `ChannelSidebarItem.tsx`, `ChannelSidebarRow.tsx`, `DmTypingBadge.tsx`,
//! `ChannelSidebarDnd.tsx`).

use super::{ChannelSidebar, DraggedChannel, RowMenu, SectionMenu, SidebarProps};
use crate::ui::message::{channel_icon, display_name};
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_relay_client::sidebar::{
    SidebarSection, is_channel_drop_section_key, is_channel_section_key,
};
use buzz_relay_client::sidebar_store::Destination;
use buzz_relay_client::{ChannelSummary, ChannelType, PresenceState, SessionSnapshot};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarShape, AvatarSize, Icon, IconButton, IconButtonSize, IconName,
    Presence, Surface, Tooltip, TypeStyled, tokens::radius, tokens::shadow,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

/// Rows inset 4px left and 10px right of the list content (§3.1).
pub(super) fn row_list() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(0.125))
        .mr(rems(0.625))
        .ml(rems(0.25))
}

/// Where a drop on section `key` moves a channel.
pub(super) fn drop_destination(key: &str) -> Destination {
    match key.strip_prefix("group:") {
        _ if key == "starred" => Destination::Starred,
        Some(id) => Destination::Section(Some(id.to_owned())),
        None => Destination::Section(None),
    }
}

/// `formatTypingLabel` (block/buzz `TypingIndicatorRow`).
pub(super) fn typing_label(names: &[String]) -> String {
    match names {
        [one] => format!("{one} is typing..."),
        [a, b] => format!("{a} and {b} are typing..."),
        [a, b, c] => format!("{a}, {b}, and {c} are typing..."),
        [a, b, rest @ ..] => format!("{a}, {b}, and {} others are typing...", rest.len()),
        [] => String::new(),
    }
}

/// The viewer's own agents, from every connected host.
fn own_agents(cx: &App) -> HashSet<String> {
    cx.try_global::<crate::hosts::Hosts>()
        .map(|hosts| {
            hosts
                .hosts
                .iter()
                .flat_map(|host| host.read(cx).agents.agents.iter().map(|a| a.pubkey.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Agents working in a channel: the viewer's own agents typing at its top
/// level (`working-agents.ts`; observer turns are not decoded here).
pub(super) fn working_agents(
    snapshot: &SessionSnapshot,
    channel: &str,
    own: &HashSet<String>,
) -> Vec<String> {
    let mut agents: Vec<String> = snapshot
        .typing
        .get(channel)
        .into_iter()
        .flatten()
        .filter(|person| own.contains(*person))
        .cloned()
        .collect();
    agents.sort();
    agents.dedup();
    agents
}

/// Three dots pulsing in sequence: `period`, per-dot `delay`, opacity from
/// `low` to 1 (and `lift` px up at the peak).
fn pulsing_dots(
    id: &'static str,
    size: f32,
    color: Hsla,
    period: Duration,
    delay: f32,
    low: f32,
    lift: f32,
) -> impl IntoElement {
    let seconds = period.as_secs_f32();
    div().flex().items_center().gap(px(2.)).with_animation(
        id,
        Animation::new(period).repeat(),
        move |row, delta| {
            row.children((0..3).map(|n| {
                let phase = (delta - n as f32 * delay / seconds).rem_euclid(1.);
                let peak = 1. - (2. * phase - 1.).abs();
                div()
                    .size(px(size))
                    .rounded_full()
                    .bg(color)
                    .opacity(low + (1. - low) * peak)
                    .mt(px(-lift * peak))
            }))
        },
    )
}

/// The drag overlay: icon and name on a popover card (§3.7).
pub(super) struct DragPreview {
    name: SharedString,
    icon: IconName,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .bg(c.surface_popover)
            .border_1()
            .border_color(c.border_standard)
            .rounded(px(12.))
            .shadow(shadow::sm())
            .text_body_sm()
            .text_color(c.text_standard)
            .cursor_grabbing()
            .child(Icon::new(self.icon).size(16.))
            .child(div().truncate().child(self.name.clone()))
    }
}

impl ChannelSidebar {
    /// One section: header with its actions, then its rows unless
    /// collapsed. Saved groups, Starred and Channels take dropped rows.
    pub(super) fn section(
        &mut self,
        index: usize,
        section: &SidebarSection<'_>,
        snapshot: &SessionSnapshot,
        props: &SidebarProps,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let key = section.key.clone();
        let open = self.is_open(&key);
        let sidebar = &snapshot.sidebar;
        let placement = sidebar.writable && sidebar.data.is_some();
        let droppable = placement && is_channel_drop_section_key(&key);
        let group: SharedString = format!("section-{index}").into();
        let menu_open = self
            .section_menu
            .as_ref()
            .is_some_and(|menu| menu.key == key);

        let toggle_key = key.clone();
        let mut summary = div()
            .id(SharedString::from(format!("summary-{key}")))
            .flex()
            .items_center()
            .gap(px(4.))
            .min_h(px(32.))
            .pl(px(8.))
            .pr(px(72.))
            .py(px(2.))
            .rounded(px(12.))
            .cursor_pointer()
            .text_body_sm()
            .text_color(c.text_unavailable)
            .on_click(cx.listener(move |this, _, _, cx| {
                let open = this.is_open(&toggle_key);
                this.toggle(&toggle_key, !open, cx);
            }));
        if let Some(icon) = &section.icon
            && key.starts_with("group:")
        {
            summary = summary.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.))
                    .mr(px(4.))
                    .child(crate::ui::status::emoji(icon, px(13.), &self.community, cx)),
            );
        }
        summary = summary
            .child(div().min_w_0().truncate().child(section.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .child(
                        Icon::new(if open {
                            IconName::CaretDown
                        } else {
                            IconName::CaretRight
                        })
                        .size(15.)
                        .stroke_width(2.5),
                    ),
            );

        let action = |id: String, icon: IconName| {
            IconButton::new(SharedString::from(id), icon)
                .size(IconButtonSize::Sm)
                .surface(Surface::Navigation)
                .icon_size(15.)
                .stroke_width(2.5)
                .icon_color(c.text_unavailable)
        };
        let menu_key = key.clone();
        let more = div()
            .relative()
            .child(
                action(format!("more-{key}"), IconName::DotsThree)
                    .when(!menu_open, |b| {
                        b.tooltip(format!("More actions for {}", section.title))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.row_menu = None;
                        this.section_menu = Some(SectionMenu {
                            key: menu_key.clone(),
                            sort_open: false,
                        });
                        cx.notify();
                    })),
            )
            .when(menu_open, |this| {
                let weak = cx.entity().downgrade();
                let dismiss: Dismiss = Rc::new(move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.section_menu = None;
                        cx.notify();
                    });
                });
                let content = self.section_menu_content(section, snapshot, cx);
                this.child(overlay::dropdown(window, Align::End, content, dismiss))
            });
        let mut actions = div()
            .absolute()
            .right(px(4.))
            .top_0()
            .bottom_0()
            .flex()
            .items_center()
            .when(!menu_open, |this| {
                this.opacity(0.)
                    .group_hover(group.clone(), |s| s.opacity(1.))
            })
            .child(more);
        if key == "dms" {
            let new_message = props.on_new_message.clone();
            actions = actions.child(
                action("new-message".into(), IconName::Plus)
                    .tooltip("New message")
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        new_message(window, cx)
                    }),
            );
        }
        if is_channel_section_key(&key) {
            let create = props.on_create_channel.clone();
            actions = actions.child(
                action(format!("create-channel-{key}"), IconName::Plus)
                    .tooltip(if create.is_some() {
                        "Create channel"
                    } else {
                        "Channel creation unavailable"
                    })
                    .disabled(create.is_none())
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        if let Some(create) = &create {
                            create(window, cx);
                        }
                    }),
            );
        }

        let own = own_agents(cx);
        let rows: Vec<AnyElement> = if open {
            section
                .rows
                .iter()
                .map(|channel| self.channel_row(channel, &key, snapshot, props, &own, cx))
                .collect()
        } else {
            Vec::new()
        };
        let drop_key = key.clone();
        let over_key = key.clone();
        let over = c.affordance_navigation_quiet;
        div()
            .id(SharedString::from(format!("section-{key}")))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .when(index > 0, |this| this.mt(rems(1.)))
            .when(droppable, |this| {
                this.drag_over::<DraggedChannel>(move |style, drag, _, _| {
                    if drag.section != over_key {
                        style.bg(over)
                    } else {
                        style
                    }
                })
                .on_drop(cx.listener(
                    move |this, drag: &DraggedChannel, _, cx| {
                        this.dragging = None;
                        if drag.section != drop_key {
                            this.move_channel(&drag.id, drop_destination(&drop_key), cx);
                        }
                    },
                ))
            })
            .child(
                div()
                    .id(group.clone())
                    .group(group)
                    .relative()
                    .child(summary)
                    .child(actions),
            )
            .child(row_list().children(rows))
            .into_any_element()
    }

    /// A channel or DM row (§3.3, §3.4).
    fn channel_row(
        &mut self,
        channel: &ChannelSummary,
        section: &str,
        snapshot: &SessionSnapshot,
        props: &SidebarProps,
        own_agents: &HashSet<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let ix = c.interaction(Surface::Navigation);
        let id = channel.id.clone();
        let selected = props.selected.as_deref() == Some(id.as_str());
        let unread = snapshot.unread.channel(&id);
        let dm = channel.channel_type == Some(ChannelType::Dm);
        let profiles = &snapshot.profiles;
        let agents = working_agents(snapshot, &id, own_agents);
        let name: SharedString = channel.name.clone().into();
        let peer = match channel.participants.as_deref() {
            Some([other]) if dm => Some(other.clone()),
            _ => None,
        };

        // The identity: the channel icon, or for DMs the avatar, the
        // participant count, or the dots of someone typing.
        let identity: AnyElement = if dm {
            let typing: Vec<String> = snapshot
                .typing_anywhere
                .get(&id)
                .into_iter()
                .flatten()
                .filter(|person| {
                    !agents.contains(person)
                        && !profiles
                            .get(*person)
                            .is_some_and(|profile| profile.is_agent)
                })
                .map(|person| display_name(profiles, person).to_string())
                .collect();
            if !typing.is_empty() {
                let label = typing_label(&typing);
                div()
                    .id(SharedString::from(format!("typing-{id}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_none()
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_full()
                    .bg(c.affordance_subtle)
                    .tooltip(Tooltip::text(label))
                    .child(pulsing_dots(
                        "dm-typing",
                        4.,
                        c.text_subtle,
                        Duration::from_millis(1400),
                        0.2,
                        0.35,
                        0.,
                    ))
                    .into_any_element()
            } else {
                match channel.participants.as_deref() {
                    Some([other]) => {
                        let presence = snapshot.presence.get(other).map(|state| match state {
                            PresenceState::Online => Presence::Online,
                            PresenceState::Away => Presence::Away,
                            PresenceState::Offline => Presence::Offline,
                        });
                        let profile = profiles.get(other);
                        let mut avatar = Avatar::new(name.clone())
                            .size(AvatarSize::Px(22.))
                            .shape(if profile.is_some_and(|p| p.is_agent) {
                                AvatarShape::Squircle
                            } else {
                                AvatarShape::Circle
                            })
                            .presence(presence);
                        if let Some(picture) = profile.and_then(|p| p.picture.as_deref()) {
                            let media = self.community.read(cx).session.media().clone();
                            avatar = avatar.image(crate::ui::media::avatar(picture, &media));
                        }
                        avatar.into_any_element()
                    }
                    Some(participants) if participants.len() > 1 => {
                        let count = participants.len();
                        div()
                            .id(SharedString::from(format!("count-{id}")))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(22.))
                            .rounded_full()
                            .bg(c.affordance_subtle)
                            .text_color(c.text_subtle)
                            .text_caption()
                            .tooltip(Tooltip::text(format!("{count} other participants")))
                            .child(count.to_string())
                            .into_any_element()
                    }
                    _ => Icon::new(IconName::ChatCircle).size(16.).into_any_element(),
                }
            }
        } else {
            Icon::new(channel_icon(channel))
                .size(16.)
                .stroke_width(2.5)
                .into_any_element()
        };

        // The 1:1 peer's status emoji, 8px after the name.
        let accessory = peer
            .as_ref()
            .and_then(|peer| self.statuses.get(peer))
            .filter(|status| !status.emoji.is_empty() || !status.text.is_empty())
            .map(|status| {
                let value = if status.emoji.is_empty() {
                    "💬".to_owned()
                } else {
                    status.emoji.clone()
                };
                let label = [status.emoji.as_str(), status.text.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                div()
                    .id(SharedString::from(format!("status-{id}")))
                    .flex_none()
                    .flex()
                    .items_center()
                    .ml(px(8.))
                    .tooltip(Tooltip::text(label))
                    .child(crate::ui::status::emoji(
                        &value,
                        px(13.),
                        &self.community,
                        cx,
                    ))
            });

        let mut trailing = Vec::new();
        if unread.mentions > 0 {
            trailing.push(
                div()
                    .min_w(px(18.))
                    .h(px(18.))
                    .px(px(5.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(c.affordance_prominent)
                    .text_color(c.text_inverse)
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(unread.mentions.to_string())
                    .into_any_element(),
            );
        }
        if !agents.is_empty() {
            let names = agents
                .iter()
                .map(|agent| display_name(profiles, agent).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trailing.push(
                div()
                    .id(SharedString::from(format!("working-{id}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(26.))
                    .h(px(15.))
                    .rounded(radius::PILL)
                    .bg(c.affordance_subtle_hover)
                    .tooltip(Tooltip::text(format!(
                        "{names} working in {}",
                        channel.name
                    )))
                    .child(pulsing_dots(
                        "working",
                        3.,
                        c.text_standard,
                        Duration::from_millis(1200),
                        0.08,
                        0.45,
                        2.,
                    ))
                    .into_any_element(),
            );
        }

        let draggable = snapshot.sidebar.writable
            && snapshot.sidebar.data.is_some()
            && !dm
            && is_channel_drop_section_key(section);
        let focus = self
            .focus
            .entry(id.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone();
        let select = props.on_select_channel.clone();
        let target = snapshot
            .channels
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .unwrap_or_else(|| channel.clone());
        let bounds = self.bounds.clone();
        let bounds_id = id.clone();
        let (menu_channel, key_channel) = (target.clone(), target.clone());
        let (menu_section, key_section) = (section.to_owned(), section.to_owned());
        let key_id = id.clone();
        let focus_down = focus.clone();
        let label_color = if unread.bold {
            Some(c.text_navigation_unread)
        } else {
            None
        };
        div()
            .id(SharedString::from(format!("ch-{id}")))
            .track_focus(&focus)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .h(rems(1.75))
            .pl(rems(0.25))
            .pr(rems(0.375))
            .py(rems(0.125))
            .gap(rems(0.375))
            .rounded(radius::ROW)
            .text_body_sm()
            .text_color(if selected {
                c.text_standard
            } else {
                c.text_navigation
            })
            .when(selected, |this| this.bg(ix.selected))
            .cursor_pointer()
            .hover(move |s| {
                let s = s.text_color(c.text_standard);
                if selected { s } else { s.bg(ix.row_hover) }
            })
            .when(self.dragging.as_deref() == Some(id.as_str()), |this| {
                this.opacity(0.3)
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .w(rems(1.25))
                    .h(rems(1.5))
                    .child(identity),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when_some(label_color, |this, color| {
                                this.text_color(color).font_weight(FontWeight::SEMIBOLD)
                            })
                            .child(name.clone()),
                    )
                    .children(accessory),
            )
            .when(!trailing.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(8.))
                        .pr(rems(0.25))
                        .text_color(c.text_metadata)
                        .children(trailing),
                )
            })
            .child(
                canvas(
                    move |row, _, _| {
                        bounds.borrow_mut().insert(bounds_id, row);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                focus_down.focus(window, cx)
            })
            .on_click(move |_, window, cx| select(&target, window, cx))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_row_menu(
                        RowMenu::new(menu_channel.clone(), menu_section.clone(), event.position),
                        window,
                        cx,
                    );
                }),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let keys = &event.keystroke;
                if keys.key == "menu" || (keys.key == "f10" && keys.modifiers.shift) {
                    cx.stop_propagation();
                    let at = this
                        .bounds
                        .borrow()
                        .get(&key_id)
                        .map(|b| b.bottom_left())
                        .unwrap_or_default();
                    this.open_row_menu(
                        RowMenu::new(key_channel.clone(), key_section.clone(), at),
                        window,
                        cx,
                    );
                }
            }))
            .when(draggable, |this| {
                let weak = cx.entity().downgrade();
                let drag = DraggedChannel {
                    id: id.clone(),
                    section: section.to_owned(),
                    name: name.clone(),
                    icon: channel_icon(channel),
                };
                this.on_drag(drag, move |drag, _, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.dragging = Some(drag.id.clone());
                        this.close_menus(cx);
                    });
                    cx.new(|_| DragPreview {
                        name: drag.name.clone(),
                        icon: drag.icon,
                    })
                })
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn typing_labels_match_buzz() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(typing_label(&names(&["Ann"])), "Ann is typing...");
        assert_eq!(
            typing_label(&names(&["Ann", "Bo"])),
            "Ann and Bo are typing..."
        );
        assert_eq!(
            typing_label(&names(&["Ann", "Bo", "Cy"])),
            "Ann, Bo, and Cy are typing..."
        );
        assert_eq!(
            typing_label(&names(&["Ann", "Bo", "Cy", "Di"])),
            "Ann, Bo, and 2 others are typing..."
        );
    }

    #[test]
    fn drops_map_to_destinations() {
        assert_eq!(drop_destination("starred"), Destination::Starred);
        assert_eq!(
            drop_destination("group:g"),
            Destination::Section(Some("g".into()))
        );
        assert_eq!(drop_destination("channels"), Destination::Section(None));
    }
}
