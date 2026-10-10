//! The channel sidebar (ui.md §3).

use super::message::{channel_icon, display_name};
use super::pages::{self, Page, Placement};
use crate::community::{AppState, Community};
use buzz_relay_client::{ChannelSummary, ChannelType, PresenceState, Profiles, SessionSnapshot};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Icon, IconButton, IconButtonSize, IconName, NavigationItem,
    Presence, Surface, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::sync::Arc;

pub struct SidebarProps {
    pub page: Page,
    pub selected: Option<String>,
    pub on_select_channel: ChannelHandler,
    pub on_select_page: PageHandler,
    /// Section "+" actions (§3.5). Channel creation is `None` when the relay
    /// does not support it.
    pub on_create_channel: Option<ActionHandler>,
    pub on_new_message: ActionHandler,
}

pub type ActionHandler = Arc<dyn Fn(&mut Window, &mut App)>;

pub type ChannelHandler = Arc<dyn Fn(&ChannelSummary, &mut Window, &mut App)>;
pub type PageHandler = Arc<dyn Fn(Page, &mut Window, &mut App)>;

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

/// The first sidebar row other than `except`, in the order [`render`]
/// draws them: channels, forums, then direct messages, each by name.
pub fn first_row(snapshot: &SessionSnapshot, except: &str) -> Option<ChannelSummary> {
    let section = |channel: &ChannelSummary| match channel.channel_type {
        Some(ChannelType::Dm) => 2,
        Some(ChannelType::Forum) => 1,
        _ => 0,
    };
    snapshot
        .channels
        .iter()
        .filter(|c| active(c) && !snapshot.hidden_dms.contains(&c.id) && c.id != except)
        .map(|c| {
            let label = channel_label(c, &snapshot.profiles).to_lowercase();
            ((section(c), label, c.id.clone()), c)
        })
        .min_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, channel)| channel.clone())
}

pub fn render(snapshot: &SessionSnapshot, props: SidebarProps, cx: &App) -> AnyElement {
    let c = *cx.colors();
    let profiles = &snapshot.profiles;
    let mut streams: Vec<&ChannelSummary> = Vec::new();
    let mut forums: Vec<&ChannelSummary> = Vec::new();
    let mut dms: Vec<(SharedString, &ChannelSummary)> = Vec::new();
    let hidden = |channel: &ChannelSummary| snapshot.hidden_dms.contains(&channel.id);
    for channel in snapshot.channels.iter().filter(|c| active(c) && !hidden(c)) {
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
    // A section header whose "+" shows on hover (§3.5).
    let section_with = |label: &'static str,
                        id: &'static str,
                        tooltip: &'static str,
                        action: Option<ActionHandler>| {
        let group = SharedString::from(format!("section-{id}"));
        let button = IconButton::new(id, IconName::Plus)
            .size(IconButtonSize::Sm)
            .surface(Surface::Navigation)
            .icon_size(15.)
            .stroke_width(2.5)
            .icon_color(c.text_unavailable)
            .tooltip(if action.is_some() {
                tooltip
            } else {
                "Channel creation unavailable"
            })
            .disabled(action.is_none())
            .on_click(move |_, window, cx| {
                if let Some(action) = &action {
                    action(window, cx);
                }
            });
        div()
            .id(group.clone())
            .group(group.clone())
            .relative()
            .child(section(label).pr(rems(4.5)))
            .child(
                div()
                    .absolute()
                    .right(rems(0.25))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .child(button),
            )
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

    let page_rows = pages::placed(Placement::Sidebar, cx)
        .into_iter()
        .map(|spec| {
            let select_page = props.on_select_page.clone();
            let page = spec.page;
            let mut row = NavigationItem::new(
                SharedString::from(format!("page-{}", page.key())),
                spec.label,
            )
            .sidebar()
            .icon(spec.icon)
            .icon_stroke(2.5)
            .selected(props.page == page)
            .on_click(move |_, window, cx| select_page(page, window, cx));
            if let Some(badge) = spec.badge.and_then(|badge| badge(cx)) {
                row = row.trailing(badge);
            }
            row
        })
        .collect::<Vec<_>>();
    let mut list = div()
        .id("sidebar")
        .flex()
        .flex_col()
        .size_full()
        .overflow_y_scroll()
        .when(!page_rows.is_empty(), |list| {
            list.child(rows().mb(rems(0.75)).children(page_rows))
        })
        .child(section_with(
            "Channels",
            "create-channel",
            "Create channel",
            props.on_create_channel.clone(),
        ))
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
    // Pictures load through the community's media client (relay.md §5n.6).
    let media = crate::community::AppState::current(cx)
        .map(|community| community.read(cx).session.media().clone());
    list = list
        .child(div().h(rems(1.)))
        .child(section_with(
            "Direct messages",
            "new-message",
            "New message",
            Some(props.on_new_message.clone()),
        ))
        .child(rows().children(dms.iter().map(|(label, channel)| {
            let first = channel
                .participants
                .as_ref()
                .and_then(|p| p.first())
                .cloned();
            // One-to-one DMs show the other person's presence; unknown
            // shows nothing.
            let presence = match channel.participants.as_deref() {
                Some([other]) => snapshot.presence.get(other).map(|state| match state {
                    PresenceState::Online => Presence::Online,
                    PresenceState::Away => Presence::Away,
                    PresenceState::Offline => Presence::Offline,
                }),
                _ => None,
            };
            let mut avatar = Avatar::new(label.clone())
                .size(AvatarSize::Px(22.))
                .presence(presence);
            if let Some(picture) = first
                .as_ref()
                .and_then(|p| profiles.get(p))
                .and_then(|profile| profile.picture.as_deref())
                && let Some(media) = &media
            {
                avatar = avatar.image(super::media::avatar(picture, media));
            }
            channel_row(channel, label.clone(), Some(avatar.into_any_element()))
        })));
    // The shell's navigation column sets the width and the divider.
    div()
        .flex()
        .flex_col()
        .size_full()
        .px(rems(0.5))
        .py(rems(0.5))
        .child(list)
        .into_any_element()
}

/// View state key for the channel sidebar (`buzz-view.v1:[scope,
/// "channel-sidebar"]`), per community and viewer.
const SIDEBAR_VIEW: &str = "channel-sidebar";

/// The channel sidebar's saved view: its width, the collapsed sections and
/// where the list was scrolled (`useSidebarView.ts`).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SidebarView {
    /// `None` is the default width.
    pub width: Option<f32>,
    /// Section keys collapsed by the user.
    pub collapsed: Vec<String>,
    pub scroll_top: f32,
}

impl SidebarView {
    /// The saved view for `community`, or the default one.
    pub fn load(community: &Community, cx: &App) -> Self {
        cx.global::<AppState>()
            .store
            .view_state(&community.partition(), SIDEBAR_VIEW)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, community: &Community, cx: &App) {
        let json = serde_json::to_string(self).expect("the view serializes");
        let saved = cx.global::<AppState>().store.set_view_state(
            &community.partition(),
            SIDEBAR_VIEW,
            Some(&json),
        );
        if let Err(error) = saved {
            tracing::warn!(%error, "could not save the sidebar view");
        }
    }
}
