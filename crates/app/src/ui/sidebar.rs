//! The channel sidebar (ui.md §3).

use super::message::{channel_icon, display_name};
use buzz_relay_client::{ChannelSummary, ChannelType, Profiles, SessionSnapshot};
use buzz_ui::{ActiveTheme, Avatar, AvatarSize, Icon, IconName, NavigationItem, TypeStyled};
use gpui_kit::*;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Messages,
    Agents,
}

pub struct SidebarProps {
    pub page: Page,
    pub selected: Option<String>,
    pub on_select_channel: Arc<dyn Fn(&ChannelSummary, &mut Window, &mut App)>,
    pub on_select_page: Arc<dyn Fn(Page, &mut Window, &mut App)>,
}

/// DM label: the other participants' names, or "Notes to self".
pub fn channel_label(channel: &ChannelSummary, profiles: &Profiles) -> SharedString {
    match (&channel.channel_type, &channel.participants) {
        (Some(ChannelType::Dm), Some(participants)) if participants.is_empty() => {
            "Notes to self".into()
        }
        (Some(ChannelType::Dm), Some(participants)) => participants
            .iter()
            .map(|p| display_name(profiles, p).to_string())
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
        _ => channel.name.clone().into(),
    }
}

fn active(channel: &ChannelSummary) -> bool {
    !channel.archived && (!channel.hidden || channel.channel_type == Some(ChannelType::Dm))
}

pub fn render(snapshot: &SessionSnapshot, props: SidebarProps, cx: &App) -> AnyElement {
    let c = cx.colors().clone();
    let profiles = &snapshot.profiles;
    let mut streams: Vec<&ChannelSummary> = Vec::new();
    let mut forums: Vec<&ChannelSummary> = Vec::new();
    let mut dms: Vec<(SharedString, &ChannelSummary)> = Vec::new();
    for channel in snapshot.channels.iter().filter(|c| active(c)) {
        match channel.channel_type {
            Some(ChannelType::Dm) => dms.push((channel_label(channel, profiles), channel)),
            Some(ChannelType::Forum) => forums.push(channel),
            _ => streams.push(channel),
        }
    }
    let by_name = |a: &str, b: &str| a.to_lowercase().cmp(&b.to_lowercase());
    streams.sort_by(|a, b| by_name(&a.name, &b.name).then(a.id.cmp(&b.id)));
    forums.sort_by(|a, b| by_name(&a.name, &b.name).then(a.id.cmp(&b.id)));
    dms.sort_by(|a, b| by_name(&a.0, &b.0).then(a.1.id.cmp(&b.1.id)));

    let section = |label: &'static str| {
        div()
            .flex()
            .items_center()
            .gap(rems(0.25))
            .pl(rems(0.5))
            .min_h(rems(2.))
            .text_body_sm()
            .text_color(c.text_unavailable)
            .child(label)
            .child(Icon::new(IconName::CaretDown).size(15.).stroke_width(2.5))
    };
    let rows = || {
        div()
            .flex()
            .flex_col()
            .gap(rems(0.125))
            .mr(rems(0.625))
            .ml(rems(0.25))
    };
    let channel_row = |channel: &ChannelSummary,
                       label: SharedString,
                       leading: Option<AnyElement>| {
        let unread = snapshot.unread.channel(&channel.id);
        let mut item = NavigationItem::new(SharedString::from(format!("ch-{}", channel.id)), label)
            .sidebar()
            .selected(props.selected.as_deref() == Some(channel.id.as_str()))
            .unread(unread.bold);
        item = match leading {
            Some(leading) => item.leading(leading),
            None => item.icon(channel_icon(channel)).icon_stroke(2.5),
        };
        if unread.mentions > 0 {
            item = item.trailing(
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
                    .child(unread.mentions.to_string()),
            );
        }
        let select = props.on_select_channel.clone();
        let target = channel.clone();
        item.on_click(move |_, window, cx| select(&target, window, cx))
    };

    let select_page = props.on_select_page.clone();
    let mut list = div()
        .id("sidebar")
        .flex()
        .flex_col()
        .size_full()
        .overflow_y_scroll()
        .child(
            rows().mb(rems(0.75)).child(
                NavigationItem::new("page-agents", "Agents")
                    .sidebar()
                    .icon(IconName::Robot)
                    .icon_stroke(2.5)
                    .selected(props.page == Page::Agents)
                    .on_click(move |_, window, cx| select_page(Page::Agents, window, cx)),
            ),
        )
        .child(section("Channels"))
        .child(
            rows().children(
                streams
                    .iter()
                    .map(|ch| channel_row(ch, ch.name.clone().into(), None)),
            ),
        );
    if !forums.is_empty() {
        list = list
            .child(div().h(rems(1.)))
            .child(section("Forums"))
            .child(
                rows().children(
                    forums
                        .iter()
                        .map(|ch| channel_row(ch, ch.name.clone().into(), None)),
                ),
            );
    }
    list = list
        .child(div().h(rems(1.)))
        .child(section("Direct messages"))
        .child(rows().children(dms.iter().map(|(label, channel)| {
            let first = channel
                .participants
                .as_ref()
                .and_then(|p| p.first())
                .cloned();
            let mut avatar = Avatar::new(label.clone()).size(AvatarSize::Px(22.));
            if let Some(picture) = first
                .as_ref()
                .and_then(|p| profiles.get(p))
                .and_then(|profile| profile.picture.clone())
            {
                avatar = avatar.image(picture);
            }
            channel_row(channel, label.clone(), Some(avatar.into_any_element()))
        })));
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(260.))
        .h_full()
        .px(rems(0.5))
        .py(rems(0.5))
        .border_r_1()
        .border_color(c.border_standard)
        .child(list)
        .into_any_element()
}
