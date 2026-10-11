//! Modal dialogs and dropdown menus drawn above the rest of the window.

use buzz_ui::Scrim;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

pub type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// Dialogs paint above the page; menus (even inside dialogs) above them.
const MODAL_LAYER: usize = 1;
const MENU_LAYER: usize = 3;

thread_local! {
    /// A modal was drawn in the frame being built, and in the last one.
    static MODALS: std::cell::Cell<(bool, bool)> = const { std::cell::Cell::new((false, false)) };
}

/// Starts a frame: what the last one drew is what the window shows.
pub fn begin_frame() {
    MODALS.with(|modals| {
        let (building, _) = modals.get();
        modals.set((false, building));
    });
}

/// A modal dialog shows, so Tab stays where it is rather than reaching the
/// page behind it.
pub fn modal_shown() -> bool {
    MODALS.with(|modals| {
        let (building, shown) = modals.get();
        building || shown
    })
}

/// A full-window layer that swallows the pointer, calling `on_dismiss` on
/// a press outside its content.
fn layer(id: &'static str, window: &Window, on_dismiss: Option<Dismiss>) -> Stateful<Div> {
    let size = window.viewport_size();
    div()
        .id(id)
        .occlude()
        .w(size.width)
        .h(size.height)
        .when_some(on_dismiss, |this, dismiss| {
            this.on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss(window, cx))
        })
}

/// Centers `content` over the modal scrim. A press on the scrim calls
/// `on_dismiss` when given.
pub fn modal(
    window: &Window,
    content: impl IntoElement,
    on_dismiss: Option<Dismiss>,
) -> impl IntoElement {
    MODALS.with(|modals| modals.set((true, modals.get().1)));
    deferred(
        anchored().position(point(px(0.), px(0.))).child(
            layer("modal", window, on_dismiss).child(
                Scrim::new().child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(content),
                ),
            ),
        ),
    )
    .with_priority(MODAL_LAYER)
}

/// Where a dropdown hangs from its trigger.
#[derive(Clone, Copy)]
pub enum Align {
    Start,
    End,
}

/// A menu hanging 4px below the trigger it follows in a `relative`
/// parent. Any press outside it calls `on_dismiss`; the press does not
/// reach what is under it, so the trigger cannot reopen the menu.
pub fn dropdown(
    window: &Window,
    align: Align,
    content: impl IntoElement,
    on_dismiss: Dismiss,
) -> impl IntoElement {
    let anchor = match align {
        Align::Start => Anchor::TopLeft,
        Align::End => Anchor::TopRight,
    };
    div()
        .absolute()
        .top_full()
        .map(|this| match align {
            Align::Start => this.left_0(),
            Align::End => this.right_0(),
        })
        .child(
            deferred(anchored().position(point(px(0.), px(0.))).child(layer(
                "menu-backdrop",
                window,
                Some(on_dismiss),
            )))
            .with_priority(MENU_LAYER),
        )
        .child(
            deferred(
                anchored()
                    .anchor(anchor)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().id("dropdown").occlude().mt(px(4.)).child(content)),
            )
            .with_priority(MENU_LAYER + 1),
        )
}
