//! The GitHub plugin (`buzz.github`, `src/bundled/github`): a panel beside
//! the conversation for GitHub repository, pull request, issue and commit
//! links, read from GitHub's public REST API.

mod attachments;
mod body;
mod body_view;
mod checks;
mod checks_view;
mod client;
mod conversation;
mod conversation_view;
mod data;
mod html;
mod panel;
mod preview;
mod references;

use crate::ui::channel_panels::{self, ChannelPanel};
use crate::ui::toasts::{self, Notice};
use buzz_ui::{ActiveTheme, IconName, ToastTone};
use gpui_kit::*;
use std::time::Duration;

pub const PANEL: &str = "buzz.github/github";

pub fn init(cx: &mut App) {
    channel_panels::register(
        ChannelPanel {
            key: PANEL,
            title: "GitHub",
            icon: IconName::GithubLogo,
            matches: |target| references::parse(target).is_some(),
            tool: false,
            launcher: None,
            menu: None,
            build: |opening, window, cx| {
                cx.new(|cx| panel::GitHubPanel::new(opening, window, cx))
                    .into()
            },
        },
        cx,
    );
}

/// Development only: `BUZZ_DEV_GITHUB_LINK=<url>` opens that link's panel
/// beside the first channel the app selects, so headless runs can
/// screenshot it.
pub fn dev_open_link() -> Option<String> {
    static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if TAKEN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    std::env::var("BUZZ_DEV_GITHUB_LINK").ok()
}

/// An inline link (`InlineReference.module.css` `.link`): link colour,
/// a chip-shaped hover fill, opening `url` in the browser.
fn inline_link(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    url: String,
    cx: &App,
) -> Stateful<Div> {
    let c = cx.colors();
    div()
        .id(id)
        .px(px(2.))
        .mx(px(-2.))
        .rounded(px(4.))
        .text_color(c.text_link)
        .cursor_pointer()
        .hover(|s| s.bg(c.affordance_link_hover))
        .child(label.into())
        .on_click(move |_, _, cx| cx.open_url(&url))
}

/// `relativeTimestamp` for an ISO time.
fn relative_time(value: &str) -> Option<String> {
    let seconds = data::seconds(value)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Some(crate::ui::link_preview::relative(seconds, now))
}

/// The local date and time, for a timestamp's tooltip
/// (`toLocaleString()`).
fn local_time(value: &str) -> Option<String> {
    let time = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    Some(
        time.with_timezone(&chrono::Local)
            .format("%-m/%-d/%Y, %-I:%M:%S %p")
            .to_string(),
    )
}

/// Copies `url` and confirms it in a toast.
fn copy_link(url: &str, title: &'static str, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
    toasts::show(
        Notice {
            key: "github-copy-link".into(),
            title: title.into(),
            description: None,
            tone: ToastTone::Success,
            timeout: Some(Duration::from_secs(4)),
            dismissible: true,
            actions: Vec::new(),
        },
        cx,
    );
}
