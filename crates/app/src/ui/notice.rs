//! One toast at a time for message actions (`ToastNotice`): copies, sends
//! to channel, reports and link menus. Success toasts leave after 4s (or
//! the given time); errors stay until dismissed.

use buzz_ui::{Toast, ToastTone};
use gpui_kit::*;
use std::time::Duration;

#[derive(Default)]
struct Notices {
    current: Option<Notice>,
    generation: usize,
}

impl Global for Notices {}

#[derive(Clone)]
struct Notice {
    text: SharedString,
    tone: ToastTone,
    generation: usize,
}

/// Shows `text`. `timeout` of `None` keeps it until dismissed.
pub fn show(
    text: impl Into<SharedString>,
    tone: ToastTone,
    timeout: Option<Duration>,
    cx: &mut App,
) {
    let notices = cx.default_global::<Notices>();
    notices.generation += 1;
    let generation = notices.generation;
    notices.current = Some(Notice {
        text: text.into(),
        tone,
        generation,
    });
    cx.refresh_windows();
    if let Some(timeout) = timeout {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(timeout).await;
            cx.update(|cx| dismiss(generation, cx));
        })
        .detach();
    }
}

/// A success toast for 4 seconds.
pub fn success(text: impl Into<SharedString>, cx: &mut App) {
    show(text, ToastTone::Success, Some(Duration::from_secs(4)), cx);
}

/// An error toast that stays until dismissed.
pub fn error(text: impl Into<SharedString>, cx: &mut App) {
    show(text, ToastTone::Error, None, cx);
}

fn dismiss(generation: usize, cx: &mut App) {
    let notices = cx.default_global::<Notices>();
    if notices
        .current
        .as_ref()
        .is_some_and(|n| n.generation == generation)
    {
        notices.current = None;
        cx.refresh_windows();
    }
}

/// The current toast, 128px from the top right, 384px wide.
pub fn render(window: &Window, cx: &App) -> Option<AnyElement> {
    let notice = cx.try_global::<Notices>()?.current.clone()?;
    let right = window.viewport_size().width - px(16.) - px(384.);
    let generation = notice.generation;
    Some(
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(point(right, px(128.)))
                .child(
                    div().w(px(384.)).child(
                        Toast::new(("message-notice", generation), notice.text)
                            .tone(notice.tone)
                            .on_dismiss(move |_, _, cx| dismiss(generation, cx)),
                    ),
                ),
        )
        .with_priority(3)
        .into_any_element(),
    )
}
