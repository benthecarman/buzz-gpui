//! Conversation activity over the composer (`TypingIndicator.tsx`): the
//! viewer's working agents as an avatar stack on the right, opening the
//! "Working now" popover, and people typing on the left.

use super::popover::{Hover, Part, working_now};
use super::row::AgentActivityRow;
use crate::agent_activity::current::{CurrentActivity, current_activity, typing_entries};
use crate::agent_activity::observer;
use crate::community::Community;
use crate::ui::message::display_name;
use crate::ui::{media, typing};
use buzz_relay_client::MessageRow;
use buzz_ui::tokens::radius;
use buzz_ui::{ActiveTheme, Avatar, AvatarSize, TypeStyled};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

/// "Alice is working", "Alice, Bob are working".
pub fn working_label(names: &[String]) -> String {
    let verb = if names.len() == 1 {
        " is working"
    } else {
        " are working"
    };
    format!("{}{verb}", names.join(", "))
}

/// The scopes an agent works in (`scopeLabel`): two at most, then "+N
/// more". Thread views name no scope.
pub fn scope_label(
    activity: &CurrentActivity,
    thread: Option<&str>,
    excerpt: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if thread.is_some() {
        return None;
    }
    let mut labels: Vec<String> = activity
        .roots
        .iter()
        .take(2)
        .map(|root| match root {
            Some(None) => "Channel conversation".to_string(),
            None => "Work with unconfirmed thread".to_string(),
            Some(Some(root)) => format!(
                "Thread · {}",
                excerpt(root).unwrap_or_else(|| root.chars().take(8).collect())
            ),
        })
        .collect();
    if activity.roots.len() > 2 {
        labels.push(format!("+{} more", activity.roots.len() - 2));
    }
    Some(labels.join(" / "))
}

/// The indicator for `channel` (or one thread of it), anchored to the
/// composer's top edge. `None` when nobody works or types there.
pub fn overlay(
    community: &Entity<Community>,
    channel: &str,
    thread: Option<&str>,
    rows: &[Arc<MessageRow>],
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let snapshot = observer::snapshot(community, cx);
    let (entries, profiles, media_source, owned) = {
        let community = community.read(cx);
        (
            typing_entries(&community.snapshot),
            community.snapshot.profiles.clone(),
            community.session.media().clone(),
            super::owned_agents(community, cx),
        )
    };
    let details: BTreeMap<String, CurrentActivity> =
        current_activity(Some(&snapshot), &entries, &owned, channel, thread);
    let humans: Vec<String> = entries
        .iter()
        .filter(|(pubkey, c, root)| {
            c == channel && root.as_deref() == thread && !details.contains_key(pubkey)
        })
        .map(|(pubkey, ..)| pubkey.clone())
        .collect();
    let hover = window.use_keyed_state(
        SharedString::from(format!(
            "working-chooser-{channel}-{}",
            thread.unwrap_or("")
        )),
        cx,
        |_, _| Hover::default(),
    );
    if details.is_empty() {
        if hover.read(cx).open {
            Hover::set_open(&hover, false, cx);
        }
        if humans.is_empty() {
            return None;
        }
    }
    let c = *cx.colors();
    let agents: Vec<String> = details.keys().cloned().collect();
    let name = |pubkey: &str| display_name(&profiles, pubkey).to_string();
    let chooser = (!agents.is_empty()).then(|| {
        let label = match agents.as_slice() {
            [one] => format!("Activity: {} working", name(one)),
            many => format!("Activity: {} agents working", many.len()),
        };
        // Overlapping 24px tiles, each 4px over the last
        // (`margin-inline-end: -space-1`).
        let shown = agents.len().min(4);
        let avatars = div()
            .relative()
            .flex_none()
            .h(px(24.))
            .w(px(24. + 20. * shown.saturating_sub(1) as f32))
            .children(agents.iter().take(4).enumerate().map(|(index, pubkey)| {
                let mut avatar = Avatar::new(name(pubkey))
                    .size(AvatarSize::Px(20.))
                    .squircle();
                if let Some(picture) = profiles.get(pubkey).and_then(|p| p.picture.as_deref()) {
                    avatar = avatar.image(media::avatar(picture, &media_source));
                }
                div()
                    .absolute()
                    .top_0()
                    .left(px(20. * index as f32))
                    .size(px(24.))
                    .p(px(2.))
                    .rounded(radius::CONTROL)
                    .bg(c.surface_panel)
                    .shadow_sm()
                    .child(avatar)
            }));
        let overflow = agents.len().saturating_sub(4);
        let trigger_hover = hover.clone();
        let toggle = hover.clone();
        let trigger = div()
            .id(SharedString::from(format!("working-trigger-{channel}")))
            .flex()
            .items_center()
            .min_h(rems(2.))
            .p(rems(0.25))
            .pr(rems(0.5))
            .rounded(radius::CONTROL)
            .cursor_pointer()
            .hover(|s| s.bg(c.affordance_subtle_hover))
            .role(Role::Button)
            .aria_label(label)
            .on_hover(move |hovered, _, cx| {
                Hover::hovered(&trigger_hover, Part::Trigger, *hovered, cx)
            })
            .on_click(move |_, _, cx| {
                let open = !toggle.read(cx).open;
                Hover::set_open(&toggle, open, cx);
            })
            .child(avatars)
            .when(overflow > 0, |this| {
                this.child(div().pl(rems(0.5)).child(format!("+{overflow}")))
            });
        let popup = hover.read(cx).open.then(|| {
            let rows = agents
                .iter()
                .map(|pubkey| {
                    let detail = &details[pubkey];
                    agent_row(
                        community,
                        channel,
                        thread,
                        rows,
                        pubkey,
                        detail,
                        &name(pubkey),
                        &hover,
                        cx,
                    )
                })
                .collect();
            div().absolute().bottom_full().right_0().child(
                deferred(
                    anchored()
                        .anchor(Anchor::BottomRight)
                        .snap_to_window_with_margin(px(8.))
                        .child(div().mb(rems(0.25)).child(working_now(
                            SharedString::from(format!("working-now-{channel}")),
                            rows,
                            &hover,
                            cx,
                        ))),
                )
                .with_priority(2),
            )
        });
        div().relative().child(trigger).children(popup)
    });
    // The live region reads "X is working. Y is typing".
    let status = [
        (!agents.is_empty())
            .then(|| working_label(&agents.iter().map(|a| name(a)).collect::<Vec<_>>())),
        (!humans.is_empty())
            .then(|| typing::label(&humans.iter().map(|p| name(p).into()).collect::<Vec<_>>())),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(". ");
    let typing_pill = (!humans.is_empty()).then(|| {
        let names: Vec<SharedString> = humans.iter().map(|p| name(p).into()).collect();
        div()
            .max_w(relative(0.5))
            .flex()
            .items_center()
            .gap(rems(0.5))
            .min_w_0()
            .px(rems(0.5))
            .py(rems(0.25))
            .rounded(radius::CONTROL)
            .bg(c.surface_panel)
            .child(typing::dots(c.text_subtle, cx))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(typing::label(&names)),
            )
    });
    Some(
        div()
            .absolute()
            .top_0()
            .left(rems(1.))
            .right(rems(1.))
            .h_0()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "conversation-activity-{channel}"
                    )))
                    .role(Role::Status)
                    .aria_label(status)
                    .absolute()
                    .bottom(rems(0.5))
                    .left_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(rems(0.375))
                    .text_caption()
                    .text_color(c.text_subtle)
                    // People typing on the left, working agents on the right.
                    .child(div().flex_1().min_w_0().children(typing_pill))
                    .children(chooser),
            )
            .into_any_element(),
    )
}

#[allow(clippy::too_many_arguments)]
fn agent_row(
    community: &Entity<Community>,
    channel: &str,
    thread: Option<&str>,
    rows: &[Arc<MessageRow>],
    pubkey: &str,
    detail: &CurrentActivity,
    name: &str,
    hover: &Entity<Hover>,
    cx: &App,
) -> AnyElement {
    let community_ref = community.read(cx);
    let profile = community_ref.snapshot.profiles.get(pubkey);
    // Unknown or several scopes open channel activity, never a guess.
    let root = thread
        .map(str::to_string)
        .or_else(|| match detail.roots.as_slice() {
            [Some(Some(root))] => Some(root.clone()),
            _ => None,
        });
    let known: Vec<&String> = detail
        .roots
        .iter()
        .filter_map(|root| root.as_ref().and_then(Option::as_ref))
        .collect();
    let conversation_root = thread
        .map(str::to_string)
        .or_else(|| (known.len() == 1).then(|| known[0].clone()));
    let conversation = match &conversation_root {
        Some(root) => format!("buzz://message?channel={channel}&id={root}&thread={root}"),
        None => format!("buzz://channel/{channel}"),
    };
    // Thread scopes are named from the loaded channel window.
    let excerpt = |root: &str| -> Option<String> {
        rows.iter()
            .find(|row| row.id.to_hex() == root)
            .map(|row| {
                row.content
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(60)
                    .collect::<String>()
            })
            .filter(|text| !text.is_empty())
    };
    let scope = scope_label(detail, thread, excerpt);
    let (close, close_activity) = (hover.clone(), hover.clone());
    let (agent, channel_id) = (pubkey.to_string(), channel.to_string());
    let media_source = community_ref.session.media().clone();
    AgentActivityRow {
        id: SharedString::from(format!("working-{channel}-{pubkey}")),
        name: name.to_string().into(),
        picture: profile
            .and_then(|p| p.picture.as_deref())
            .map(|picture| media::avatar(picture, &media_source)),
        status: detail.label.clone().into(),
        meta: None,
        scope: scope.map(Into::into),
        open_label: format!(
            "Open {} for {name}",
            if conversation_root.is_some() {
                "thread"
            } else {
                "conversation"
            }
        )
        .into(),
        on_open: Some(Rc::new(move |window, cx| {
            Hover::set_open(&close, false, cx);
            crate::ui::navigation::open_url(&conversation, window, cx);
        })),
        on_open_activity: Some(Rc::new(move |window, cx| {
            Hover::set_open(&close_activity, false, cx);
            super::open(&agent, Some(&channel_id), root.as_deref(), window, cx);
        })),
    }
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn labels_follow_the_reference() {
        assert_eq!(working_label(&["Ada".into()]), "Ada is working");
        assert_eq!(
            working_label(&["Ada".into(), "Bo".into()]),
            "Ada, Bo are working"
        );
        let activity = CurrentActivity {
            label: "Working".into(),
            roots: vec![Some(None), Some(Some("a".repeat(64))), None],
        };
        assert_eq!(
            scope_label(&activity, None, |_| Some("Fix the build".into())).unwrap(),
            "Channel conversation / Thread · Fix the build / +1 more"
        );
        assert_eq!(scope_label(&activity, Some("x"), |_| None), None);
        let unknown = CurrentActivity {
            label: "Working".into(),
            roots: vec![None],
        };
        assert_eq!(
            scope_label(&unknown, None, |_| None).unwrap(),
            "Work with unconfirmed thread"
        );
    }
}
