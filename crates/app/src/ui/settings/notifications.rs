//! Settings → Notifications (§5b.5 item 3): the selected community's
//! notification preferences. Banners are always silent here, so there are
//! no sound choices.

use super::{header, inline_header, note, preference_row, rows_group, section};
use crate::community::AppState;
use crate::notifications::Preferences;
use buzz_ui::Switch;
use gpui_kit::*;

pub(super) fn render(cx: &App) -> AnyElement {
    let Some(community) = AppState::current(cx) else {
        return section(vec![
            header("Notifications", None, cx).into_any_element(),
            note("Add a community to choose its notifications.", cx).into_any_element(),
        ]);
    };
    let store = cx.global::<AppState>().store.clone();
    let (name, partition) = {
        let community = community.read(cx);
        (community.name(), community.partition())
    };
    let preferences = Preferences::load(&store, &partition);
    let toggle = |id: &'static str,
                  title: &'static str,
                  description: Option<&'static str>,
                  checked: bool,
                  enabled: bool,
                  apply: fn(&mut Preferences, bool)| {
        let store = store.clone();
        let partition = partition.clone();
        let current = preferences.clone();
        preference_row(
            None,
            title,
            description.map(IntoElement::into_any_element),
            vec![
                Switch::new(id, checked)
                    .disabled(!enabled)
                    .on_change(move |value, window, _| {
                        let mut next = current.clone();
                        apply(&mut next, value);
                        next.save(&store, &partition);
                        window.refresh();
                    })
                    .into_any_element(),
            ],
            cx,
        )
        .into_any_element()
    };
    let enabled = preferences.enabled;
    section(vec![
        header(
            "Notifications",
            Some(format!("For {name}. Only the selected community shows alerts.").into()),
            cx,
        )
        .into_any_element(),
        div()
            .child(inline_header("Notify me about", None, Vec::new(), cx))
            .child(rows_group(
                vec![
                    toggle(
                        "notify-mentions",
                        "Mentions",
                        None,
                        preferences.mention,
                        enabled,
                        |p, v| p.mention = v,
                    ),
                    toggle(
                        "notify-direct",
                        "Direct messages",
                        None,
                        preferences.direct,
                        enabled,
                        |p, v| p.direct = v,
                    ),
                    toggle(
                        "notify-thread",
                        "Thread replies",
                        None,
                        preferences.thread,
                        enabled,
                        |p, v| p.thread = v,
                    ),
                ],
                cx,
            ))
            .into_any_element(),
        div()
            .child(inline_header("Delivery", None, Vec::new(), cx))
            .child(rows_group(
                vec![toggle(
                    "notify-desktop",
                    "Desktop alerts",
                    Some("Show a silent banner through your desktop’s notification service."),
                    enabled,
                    true,
                    |p, v| p.enabled = v,
                )],
                cx,
            ))
            .into_any_element(),
    ])
}
