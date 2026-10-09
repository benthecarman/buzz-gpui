//! Click-and-drag scrubbing, the `input type="range"` of the reference
//! players.

use gpui_kit::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Called with the pointer's position along the bar, 0..=1.
pub type OnSeek = Rc<dyn Fn(f32, &mut Window, &mut App)>;

thread_local! {
    /// The bar a press started on, so a drag keeps scrubbing it even when
    /// the pointer strays off its row.
    static DRAGGING: RefCell<Option<ElementId>> = const { RefCell::new(None) };
}

/// Covers its `relative` parent and reports presses and drags on it.
pub fn seek_area(id: impl Into<ElementId>, on_seek: Option<OnSeek>) -> impl IntoElement {
    let id = id.into();
    canvas(
        move |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let Some(on_seek) = on_seek else {
                return;
            };
            window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
            let fraction = move |x: Pixels| {
                ((x - bounds.left()) / bounds.size.width.max(px(1.))).clamp(0., 1.)
            };
            let (press_id, press_seek) = (id.clone(), on_seek.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                DRAGGING.with_borrow_mut(|dragging| *dragging = Some(press_id.clone()));
                press_seek(fraction(event.position.x), window, cx);
                cx.stop_propagation();
            });
            let drag_id = id.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                let ours = DRAGGING.with_borrow(|dragging| dragging.as_ref() == Some(&drag_id));
                if phase == DispatchPhase::Bubble && ours && event.dragging() {
                    on_seek(fraction(event.position.x), window, cx);
                }
            });
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, _| {
                if phase == DispatchPhase::Bubble {
                    DRAGGING.with_borrow_mut(|dragging| {
                        if dragging.as_ref() == Some(&id) {
                            *dragging = None;
                        }
                    });
                }
            });
        },
    )
    .absolute()
    .inset_0()
    .size_full()
}
