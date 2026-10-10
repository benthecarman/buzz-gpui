//! Settings → Shortcuts (§5b.5 item 4), read-only: the app's keybindings
//! cannot be changed yet.

use super::{header, inline_header, preference_row, rows_group, section};
use buzz_ui::Kbd;
use gpui_kit::*;

/// `Mod` as this platform writes it.
fn modifier() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl+"
    }
}

pub(super) fn render(cx: &App) -> AnyElement {
    let m = modifier();
    let shortcuts: [(&str, String); 7] = [
        ("Open settings", format!("{m},")),
        ("Increase interface size", format!("{m}=")),
        ("Decrease interface size", format!("{m}-")),
        ("Reset interface size", format!("{m}0")),
        ("Send message", "Enter".into()),
        ("New line", "Shift+Enter".into()),
        ("Cancel an edit or close a thread", "Esc".into()),
    ];
    let rows = shortcuts
        .into_iter()
        .map(|(title, keys)| {
            preference_row(
                None,
                title,
                Some("Reserved by Buzz".into_any_element()),
                vec![Kbd::new(keys).into_any_element()],
                cx,
            )
            .into_any_element()
        })
        .collect();
    section(vec![
        header("Shortcuts", None, cx).into_any_element(),
        div()
            .child(inline_header("Buzz", None, Vec::new(), cx))
            .child(rows_group(rows, cx))
            .into_any_element(),
    ])
}
