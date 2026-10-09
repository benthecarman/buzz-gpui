//! One timeline row (ui.md §4.4–4.5).

use super::time;
use buzz_relay_client::fold::{Delivery, MessageRow};
use buzz_relay_client::{Profile, Profiles};
use buzz_ui::{ActiveTheme, Avatar, AvatarSize, IconName, TypeStyled};
use gpui_kit::component::text::TextView;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::sync::Arc;

/// A message plus how it sits in the timeline.
#[derive(Clone)]
pub struct Item {
    pub row: Arc<MessageRow>,
    /// Starts a group: shows avatar and byline.
    pub head: bool,
    /// A day divider goes above this row.
    pub day: Option<SharedString>,
}

/// Groups rows as buzz-app's `continuesMessageGroup` does: same author, at
/// most five minutes apart, same local day, and neither row a system,
/// workflow or sent-from-thread message.
pub fn items(rows: &[Arc<MessageRow>]) -> Vec<Item> {
    let mut items = Vec::with_capacity(rows.len());
    let mut previous: Option<&Arc<MessageRow>> = None;
    for row in rows {
        let new_day = previous.is_none_or(|p| !time::same_day(p.created_at, row.created_at));
        let continues = !new_day
            && previous.is_some_and(|p| {
                p.author == row.author
                    && row.created_at >= p.created_at
                    && row.created_at - p.created_at <= 300
                    && p.membership.is_none()
                    && row.membership.is_none()
                    && p.workflow_owner.is_none()
                    && row.workflow_owner.is_none()
                    && p.sent_from_thread.is_none()
                    && row.sent_from_thread.is_none()
            });
        items.push(Item {
            row: row.clone(),
            head: !continues,
            day: new_day.then(|| time::day(row.created_at).into()),
        });
        previous = Some(row);
    }
    items
}

pub fn display_name(profiles: &Profiles, pubkey: &str) -> SharedString {
    profiles
        .get(pubkey)
        .map(|profile| profile.name.clone())
        .unwrap_or_else(|| Profile::fallback(pubkey).name)
        .into()
}

pub struct Actions {
    pub on_open_thread: Option<Arc<dyn Fn(&Arc<MessageRow>, &mut Window, &mut App)>>,
    pub on_react: Option<Arc<dyn Fn(&Arc<MessageRow>, &str, &mut Window, &mut App)>>,
    pub on_retry: Option<Arc<dyn Fn(&Arc<MessageRow>, &mut Window, &mut App)>>,
}

pub fn render(
    item: &Item,
    viewer: &str,
    profiles: &Profiles,
    actions: &Actions,
    cx: &App,
) -> AnyElement {
    let c = cx.colors().clone();
    let row = &item.row;
    let author = row.author.to_hex();
    let profile = profiles.get(&author);
    let name = display_name(profiles, &author);
    let agent = row.is_agent_envelope() || profile.is_some_and(|p| p.is_agent);

    let mut container = div().flex().flex_col().w_full();
    if let Some(day) = &item.day {
        container = container.child(
            div()
                .flex()
                .items_center()
                .my(px(12.))
                .child(div().flex_1().h(px(1.)).bg(c.border_standard))
                .child(
                    div()
                        .px(px(12.))
                        .py(px(6.))
                        .text_caption()
                        .text_color(c.text_standard)
                        .child(day.clone()),
                )
                .child(div().flex_1().h(px(1.)).bg(c.border_standard)),
        );
    }

    if let Some(change) = &row.membership {
        let subject = display_name(profiles, &change.target.to_hex());
        let text = format!("{subject} {}", membership_text(change, profiles));
        return container
            .child(
                div()
                    .flex()
                    .justify_center()
                    .gap(px(12.))
                    .pt(px(8.))
                    .pb(px(16.))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(text),
            )
            .into_any_element();
    }

    let group = SharedString::from(format!("message-{}", row.id.to_hex()));
    let left = if item.head {
        let mut avatar = Avatar::new(name.clone()).size(AvatarSize::Large);
        if agent {
            avatar = avatar.squircle();
        }
        if let Some(picture) = profile.and_then(|p| p.picture.clone()) {
            avatar = avatar.image(picture);
        }
        div()
            .flex_none()
            .w(px(40.))
            .child(avatar)
            .into_any_element()
    } else {
        div()
            .flex_none()
            .w(px(40.))
            .flex()
            .justify_center()
            .text_caption()
            .line_height(px(20.))
            .text_color(c.text_subtle)
            .invisible()
            .group_hover(group.clone(), |style| style.visible())
            .child(time::compact(row.created_at))
            .into_any_element()
    };

    let mut body = div().flex().flex_col().flex_1().min_w_0();
    if item.head {
        body = body.child(
            div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap(px(8.))
                .text_body_sm()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c.text_standard)
                        .child(name),
                )
                .child(
                    div()
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child(time::byline(row.created_at)),
                ),
        );
    }
    if row.sent_from_thread.is_some() {
        body = body.child(
            div()
                .my(px(4.))
                .text_caption()
                .text_color(c.text_subtle)
                .child("Sent from Thread"),
        );
    }
    if !row.content.is_empty() {
        body = body.child(
            div()
                .mt(if item.head { px(4.) } else { px(0.) })
                .text_body_sm()
                .text_color(c.text_standard)
                .child(TextView::markdown(
                    SharedString::from(format!("md-{}", row.id.to_hex())),
                    row.content.clone(),
                )),
        );
    }
    if !row.reactions.is_empty() {
        body = body.child(reactions(row, viewer, actions, cx));
    }
    if row.reply_count > 0 {
        body = body.child(thread_summary(row, profiles, actions, cx));
    }
    if let Some(delivery) = &row.delivery
        && matches!(delivery.status, Delivery::Failed | Delivery::Unknown)
    {
        let text = if delivery.status == Delivery::Failed {
            "Couldn’t send this message."
        } else {
            "Delivery not yet confirmed."
        };
        let mut notice = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .mt(px(6.))
            .text_caption()
            .text_color(c.text_warning)
            .child(text);
        if let Some(retry) = actions.on_retry.clone() {
            let target = row.clone();
            notice = notice.child(
                buzz_ui::Button::new(
                    SharedString::from(format!("retry-{}", row.id.to_hex())),
                    "Retry",
                )
                .variant(buzz_ui::ButtonVariant::Ghost)
                .size(buzz_ui::ButtonSize::Sm)
                .on_click(move |_, window, cx| retry(&target, window, cx)),
            );
        }
        body = body.child(notice);
    }

    container
        .child(
            div()
                .id(group.clone())
                .group(group)
                .flex()
                .gap(px(12.))
                .pt(if item.head { px(14.) } else { px(2.) })
                .pb(px(2.))
                .child(left)
                .child(body),
        )
        .into_any_element()
}

fn reactions(row: &Arc<MessageRow>, viewer: &str, actions: &Actions, cx: &App) -> impl IntoElement {
    let c = cx.colors().clone();
    let mut pills = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.))
        .mt(px(6.));
    for (index, group) in row.reactions.iter().enumerate() {
        let mut people: Vec<String> = group.events.iter().map(|r| r.author.to_hex()).collect();
        people.sort();
        people.dedup();
        let mine = people.iter().any(|p| p == viewer);
        let mut pill = div()
            .id(SharedString::from(format!(
                "reaction-{}-{index}",
                row.id.to_hex()
            )))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .min_w(px(48.))
            .min_h(px(28.))
            .px(px(8.))
            .rounded_full()
            .border_1()
            .cursor_pointer()
            .child(div().text_size(px(14.)).child(group.content.clone()))
            .child(div().text_size(px(12.)).child(people.len().to_string()));
        pill = if mine {
            pill.border_color(c.border_prominent)
                .bg(c.affordance_selected)
                .text_color(c.affordance_prominent)
        } else {
            pill.border_color(c.border_standard)
                .bg(c.affordance_subtle)
                .text_color(c.text_standard)
                .hover(|style| style.bg(c.affordance_subtle_hover))
        };
        if let Some(react) = actions.on_react.clone() {
            let target = row.clone();
            let content = group.content.clone();
            pill = pill.on_click(move |_, window, cx| react(&target, &content, window, cx));
        }
        pills = pills.child(pill);
    }
    pills
}

fn thread_summary(
    row: &Arc<MessageRow>,
    profiles: &Profiles,
    actions: &Actions,
    cx: &App,
) -> impl IntoElement {
    let c = cx.colors().clone();
    let mut avatars = div().flex();
    for (index, participant) in row.participants.iter().take(3).enumerate() {
        let hex = participant.to_hex();
        let mut avatar = Avatar::new(display_name(profiles, &hex)).size(AvatarSize::Small);
        if let Some(picture) = profiles.get(&hex).and_then(|p| p.picture.clone()) {
            avatar = avatar.image(picture);
        }
        avatars = avatars.child(
            div()
                .when(index > 0, |d| d.ml(px(-4.)))
                .rounded_full()
                .p(px(1.5))
                .bg(c.surface_panel)
                .child(avatar),
        );
    }
    let label = if row.reply_count == 1 {
        "1 reply".to_string()
    } else {
        format!("{} replies", row.reply_count)
    };
    let mut summary = div()
        .id(SharedString::from(format!("thread-{}", row.id.to_hex())))
        .flex()
        .items_center()
        .gap(px(8.))
        .ml(px(-4.))
        .mt(px(4.))
        .p(px(4.))
        .pr(px(6.))
        .min_h(px(32.))
        .rounded(px(10.))
        .cursor_pointer()
        .text_caption()
        .text_color(c.text_standard)
        .hover(|style| style.bg(c.affordance_subtle_hover))
        .child(avatars)
        .child(label);
    if let Some(open) = actions.on_open_thread.clone() {
        let target = row.clone();
        summary = summary.on_click(move |_, window, cx| open(&target, window, cx));
    }
    summary
}

fn membership_text(
    change: &buzz_relay_client::fold::MembershipChange,
    profiles: &Profiles,
) -> String {
    use buzz_relay_client::fold::MembershipKind;
    match change.kind {
        MembershipKind::Joined => "joined".into(),
        MembershipKind::Left => "left the channel".into(),
        MembershipKind::Removed if change.actor != change.target => {
            format!(
                "was removed by {}",
                display_name(profiles, &change.actor.to_hex())
            )
        }
        MembershipKind::Removed => "was removed".into(),
    }
}

/// Icon for a channel in headers and the sidebar.
pub fn channel_icon(channel: &buzz_relay_client::ChannelSummary) -> IconName {
    use buzz_relay_client::ChannelType;
    match channel.channel_type {
        Some(ChannelType::Dm) => IconName::ChatCircle,
        _ if channel.private => IconName::Lock,
        _ => IconName::Hash,
    }
}
