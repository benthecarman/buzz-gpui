//! The typing indicator above the composer (ui.md §4.10).

use super::message::display_name;
use buzz_relay_client::Profiles;
use buzz_ui::{ActiveTheme, TypeStyled};
use gpui_kit::*;
use std::time::Duration;

/// "Alice is typing", "Alice, Bob are typing", "Alice, Bob and 2 others
/// are typing".
pub fn label(names: &[SharedString]) -> String {
    let shown = names
        .iter()
        .take(2)
        .map(SharedString::as_ref)
        .collect::<Vec<&str>>()
        .join(", ");
    let others = match names.len() {
        0..=2 => String::new(),
        n => format!(" and {} others", n - 2),
    };
    let verb = if names.len() == 1 {
        " is typing"
    } else {
        " are typing"
    };
    format!("{shown}{others}{verb}")
}

/// Three dots fading to 35% and back over 1.4 s, 0.2 s apart.
fn dots(color: Hsla) -> impl IntoElement {
    const PERIOD: f32 = 1.4;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .with_animation(
            "typing-dots",
            Animation::new(Duration::from_millis(1400)).repeat(),
            move |row, delta| {
                row.children((0..3).map(|n| {
                    let phase = (delta - n as f32 * 0.2 / PERIOD).rem_euclid(1.);
                    // Down to the 50% keyframe and back, eased both ways.
                    let fade = ease_in_out(1. - (2. * phase - 1.).abs());
                    div()
                        .size(px(4.))
                        .rounded_full()
                        .bg(color)
                        .opacity(1. - 0.65 * fade)
                }))
            },
        )
}

/// A zero-height anchor for the top edge of the composer: the pill sits
/// 8px above it, inset 16px, over the bottom of the timeline. `None` when
/// nobody is typing.
pub fn overlay(typing: &[String], profiles: &Profiles, cx: &App) -> Option<AnyElement> {
    if typing.is_empty() {
        return None;
    }
    let c = cx.colors();
    let names: Vec<SharedString> = typing
        .iter()
        .map(|pubkey| display_name(profiles, pubkey))
        .collect();
    let pill = div()
        .absolute()
        .bottom(px(8.))
        .left_0()
        .max_w(relative(0.5))
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(12.))
        .bg(c.surface_panel)
        .text_caption()
        .text_color(c.text_subtle)
        .child(dots(c.text_subtle))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label(&names)),
        );
    Some(
        div()
            .absolute()
            .top_0()
            .left(px(16.))
            .right(px(16.))
            .h_0()
            .child(pill)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::label;
    use gpui_kit::SharedString;

    #[test]
    fn labels_follow_the_reference() {
        let names: Vec<SharedString> = ["Alice", "Bob", "Carol", "Dan"]
            .into_iter()
            .map(SharedString::from)
            .collect();
        assert_eq!(label(&names[..1]), "Alice is typing");
        assert_eq!(label(&names[..2]), "Alice, Bob are typing");
        assert_eq!(label(&names), "Alice, Bob and 2 others are typing");
    }
}
