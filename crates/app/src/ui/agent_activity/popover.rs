//! Hover popovers listing working agents: the composer indicator's
//! "Working now" (`TypingIndicator.tsx` `AgentChooser`) and the sidebar
//! channel's activity popover (`ChannelActivityPopover.tsx`, its "Working
//! now" section).

use super::row::{AgentActivityRow, section_label};
use crate::agent_activity::current::{working_details, working_duration};
use crate::agent_activity::observer::{self, now_ms};
use crate::community::Community;
use crate::ui::media;
use buzz_ui::{Popover, PopoverPadding, PopoverWidth};
use gpui_kit::*;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

/// Opens this long after the pointer arrives...
pub const OPEN_DELAY: Duration = Duration::from_millis(250);
/// ...and closes this long after it leaves trigger and popup.
pub const CLOSE_DELAY: Duration = Duration::from_millis(150);

/// A hover popover's state: open after a delay, closed once neither the
/// trigger nor the popup has the pointer.
#[derive(Default)]
pub struct Hover {
    pub open: bool,
    trigger: bool,
    popup: bool,
    pending: Option<Task<()>>,
    /// Redraws once a second while open, for durations.
    ticker: Option<Task<()>>,
}

#[derive(Clone, Copy)]
pub enum Part {
    Trigger,
    Popup,
}

impl Hover {
    pub fn hovered(this: &Entity<Self>, part: Part, hovered: bool, cx: &mut App) {
        this.update(cx, |hover, cx| {
            match part {
                Part::Trigger => hover.trigger = hovered,
                Part::Popup => hover.popup = hovered,
            }
            let wanted = hover.trigger || hover.popup;
            if wanted == hover.open {
                hover.pending = None;
                return;
            }
            let delay = if wanted { OPEN_DELAY } else { CLOSE_DELAY };
            hover.pending = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |hover, cx| {
                    hover.open = hover.trigger || hover.popup;
                    hover.pending = None;
                    if !hover.open {
                        hover.ticker = None;
                    }
                    cx.notify();
                    // Rows kept outside a view's own state redraw too.
                    cx.refresh_windows();
                });
            }));
        });
    }

    pub fn set_open(this: &Entity<Self>, open: bool, cx: &mut App) {
        this.update(cx, |hover, cx| {
            hover.open = open;
            hover.pending = None;
            if !open {
                hover.trigger = false;
                hover.popup = false;
                hover.ticker = None;
            }
            cx.notify();
            cx.refresh_windows();
        });
    }
}

/// Hover states kept per key (a channel row), for views that render rows
/// without a window.
#[derive(Default)]
struct Hovers(HashMap<String, Entity<Hover>>);

impl Global for Hovers {}

pub fn hover_state(key: &str, cx: &mut App) -> Entity<Hover> {
    if let Some(found) = cx.default_global::<Hovers>().0.get(key) {
        return found.clone();
    }
    let hover = cx.new(|_| Hover::default());
    let hovers = &mut cx.global_mut::<Hovers>().0;
    if hovers.len() > 512 {
        hovers.clear();
    }
    hovers.insert(key.to_string(), hover.clone());
    hover
}

/// The popup's surface: a wide list popover with the "Working now" rows,
/// at most 22.5rem tall.
pub fn working_now(
    id: impl Into<ElementId>,
    rows: Vec<AnyElement>,
    hover: &Entity<Hover>,
    cx: &App,
) -> AnyElement {
    let popup = hover.clone();
    div()
        .id(id)
        .occlude()
        .on_hover(move |hovered, _, cx| Hover::hovered(&popup, Part::Popup, *hovered, cx))
        .child(
            Popover::new()
                .width(PopoverWidth::Wide)
                .padding(PopoverPadding::List)
                .child(
                    div()
                        .id("working-now-list")
                        .flex()
                        .flex_col()
                        .max_h(rems(22.5))
                        .overflow_y_scroll()
                        .child(section_label("Working now", cx))
                        .children(rows),
                ),
        )
        .into_any_element()
}

/// The sidebar channel's activity popover: who works in `channel` now,
/// how long, and links to their conversation and activity. `None` while
/// nobody works there or it is closed.
pub fn channel_activity(
    community: &Entity<Community>,
    channel: &buzz_relay_client::ChannelSummary,
    agents: &[String],
    hover: &Entity<Hover>,
    cx: &mut App,
) -> Option<AnyElement> {
    if agents.is_empty() {
        if hover.read(cx).open {
            Hover::set_open(hover, false, cx);
        }
        return None;
    }
    if !hover.read(cx).open {
        return None;
    }
    let snapshot = observer::snapshot(community, cx);
    let (profiles, media_source) = {
        let community = community.read(cx);
        (
            community.snapshot.profiles.clone(),
            community.session.media().clone(),
        )
    };
    let keys = super::public_key_labels(agents.iter());
    let now = now_ms();
    // The durations tick while it is open.
    hover.update(cx, |hover, cx| {
        if hover.ticker.is_none() {
            hover.ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let open = this.update(cx, |hover, cx| {
                        cx.refresh_windows();
                        hover.open
                    });
                    if !matches!(open, Ok(true)) {
                        return;
                    }
                }
            }));
        }
    });
    let rows: Vec<AnyElement> = agents
        .iter()
        .map(|agent| {
            let profile = profiles.get(agent);
            let name = profile
                .filter(|p| !p.name_is_fallback)
                .map(|p| p.name.clone())
                .or_else(|| keys.get(agent).cloned())
                .unwrap_or_else(|| "Agent".into());
            let details = working_details(&snapshot, &channel.id, agent);
            let message = details.as_ref().and_then(|d| d.message_id.clone());
            let meta = details
                .as_ref()
                .map_or("—".to_string(), |d| working_duration(d.started_at, now));
            let open_url = match &message {
                Some(id) => format!("buzz://message?channel={}&id={id}&thread={id}", channel.id),
                None => format!("buzz://channel/{}", channel.id),
            };
            let close = hover.clone();
            let close_activity = hover.clone();
            let (agent_key, channel_id) = (agent.clone(), channel.id.clone());
            AgentActivityRow {
                id: SharedString::from(format!("channel-working-{}-{agent}", channel.id)),
                name: name.clone().into(),
                picture: profile
                    .and_then(|p| p.picture.as_deref())
                    .map(|picture| media::avatar(picture, &media_source)),
                status: "Working".into(),
                meta: Some(meta.into()),
                scope: None,
                open_label: format!(
                    "Open {} for {name} in {}",
                    if message.is_some() {
                        "thread"
                    } else {
                        "conversation"
                    },
                    channel.name
                )
                .into(),
                on_open: Some(Rc::new(move |window, cx| {
                    Hover::set_open(&close, false, cx);
                    crate::ui::navigation::open_url(&open_url, window, cx);
                })),
                // Activity opens beside the channel it is about.
                on_open_activity: (snapshot.status != crate::agent_activity::FeedStatus::Disabled)
                    .then(|| {
                        Rc::new(move |window: &mut Window, cx: &mut App| {
                            Hover::set_open(&close_activity, false, cx);
                            crate::ui::navigation::open_url(
                                &format!("buzz://channel/{channel_id}"),
                                window,
                                cx,
                            );
                            super::open(&agent_key, Some(&channel_id), None, window, cx);
                        }) as super::row::Action
                    }),
            }
            .into_any_element()
        })
        .collect();
    let popup = working_now(
        SharedString::from(format!("channel-activity-{}", channel.id)),
        rows,
        hover,
        cx,
    );
    Some(
        div()
            .absolute()
            .top_0()
            .left_full()
            .child(deferred(anchored().offset(point(px(6.), px(0.))).child(popup)).with_priority(2))
            .into_any_element(),
    )
}

/// The sidebar's working agents in `channel`: the viewer's agents typing
/// there, and agents with observed working turns or typing there.
pub fn with_observed(
    mut agents: Vec<String>,
    community: &Entity<Community>,
    channel: &str,
    cx: &App,
) -> Vec<String> {
    let snapshot = observer::snapshot(community, cx);
    agents.extend(crate::agent_activity::current::working_agents(
        &snapshot, channel,
    ));
    agents.sort();
    agents.dedup();
    agents
}

/// The pointer entered or left a sidebar row.
pub fn row_hovered(hover: &Entity<Hover>, hovered: bool, cx: &mut App) {
    Hover::hovered(hover, Part::Trigger, hovered, cx);
}

/// The hover state's key for a sidebar row.
pub fn row_key(community: &Entity<Community>, channel: &str) -> String {
    format!("{}:{channel}", community.entity_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn delays_follow_the_reference() {
        assert_eq!(OPEN_DELAY, Duration::from_millis(250));
        assert_eq!(CLOSE_DELAY, Duration::from_millis(150));
    }
}
