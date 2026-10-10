//! Settings → Appearance (§1.1, §5b.5 item 2): color mode thumbnails and
//! the interface size stepper.

use super::{button, header, preference_row, rows_group, section};
use buzz_ui::{
    ActiveTheme, Appearance, AppearancePreference, ButtonSize, Colors, INTERFACE_SCALE_MAX,
    INTERFACE_SCALE_MIN, IconButton, IconButtonSize, IconName, Theme, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

pub(super) fn render(cx: &App) -> AnyElement {
    let theme = Theme::base(cx);
    let preference = theme.preference();
    let scale = theme.interface_scale();
    let picker = div().flex().gap(rems(0.75)).children(
        [
            (AppearancePreference::System, "System"),
            (AppearancePreference::Light, "Light"),
            (AppearancePreference::Dark, "Dark"),
        ]
        .into_iter()
        .map(|(mode, label)| thumbnail(mode, label, preference == mode, cx)),
    );
    let percent = (scale * 100.).round() as i32;
    let size = div()
        .flex()
        .items_center()
        .gap(rems(0.75))
        .child(
            button("interface-smaller", "−")
                .size(ButtonSize::Sm)
                .disabled(scale <= INTERFACE_SCALE_MIN + 0.01)
                .tooltip("Decrease interface size")
                .on_click(|_, _, cx| Theme::decrease_interface_scale(cx)),
        )
        .child(
            div()
                .min_w(rems(2.5))
                .text_center()
                .text_body_sm()
                .child(format!("{percent}%")),
        )
        .child(
            button("interface-larger", "+")
                .size(ButtonSize::Sm)
                .disabled(scale >= INTERFACE_SCALE_MAX - 0.01)
                .tooltip("Increase interface size")
                .on_click(|_, _, cx| Theme::increase_interface_scale(cx)),
        )
        .when(percent != 100, |this| {
            this.child(
                IconButton::new("interface-reset", IconName::ArrowCounterClockwise)
                    .size(IconButtonSize::Sm)
                    .tooltip("Reset interface size")
                    .on_click(|_, _, cx| Theme::reset_interface_scale(cx)),
            )
        });
    section(vec![
        header("Appearance", None, cx).into_any_element(),
        rows_group(
            vec![
                preference_row(
                    None,
                    "Color mode",
                    None,
                    vec![picker.into_any_element()],
                    cx,
                )
                .into_any_element(),
                preference_row(
                    None,
                    "Interface size",
                    None,
                    vec![size.into_any_element()],
                    cx,
                )
                .into_any_element(),
            ],
            cx,
        )
        .into_any_element(),
    ])
}

/// One `ThemePicker` radio: an 80×56 preview, the system one split down
/// the middle, outlined 1px (2px focus color when chosen) 2px outside.
fn thumbnail(
    mode: AppearancePreference,
    label: &'static str,
    selected: bool,
    cx: &App,
) -> Stateful<Div> {
    let c = cx.colors();
    let unit = |value: f32| rems(value / 16.);
    let previews: Vec<AnyElement> = match mode {
        AppearancePreference::Light => vec![preview(Appearance::Light, 0., 80.)],
        AppearancePreference::Dark => vec![preview(Appearance::Dark, 0., 80.)],
        AppearancePreference::System => vec![
            preview(Appearance::Light, 0., 40.),
            preview(Appearance::Dark, 40., 40.),
        ],
    };
    let (ring, width) = if selected {
        (c.border_focus, 2.)
    } else {
        (c.border_standard, 1.)
    };
    let hover = c.border_prominent;
    div()
        .id(SharedString::from(format!("theme-{label}")))
        .flex_none()
        .p(unit(2. - (width - 1.)))
        .border(px(width))
        .border_color(ring)
        .rounded(unit(8. + 3.))
        .cursor_pointer()
        .when(!selected, |this| this.hover(move |s| s.border_color(hover)))
        .tooltip(buzz_ui::Tooltip::text(label))
        .on_click(move |_, _, cx| Theme::set_preference(mode, cx))
        .child(
            div()
                .relative()
                .w(unit(80.))
                .h(unit(56.))
                .rounded(unit(8.))
                .overflow_hidden()
                .children(previews),
        )
}

/// The 80×56 preview drawing (`ThemePicker.tsx`), showing `width` units
/// of it from `left`.
fn preview(appearance: Appearance, left: f32, width: f32) -> AnyElement {
    let c: Colors = Theme::colors_for(appearance);
    let unit = |value: f32| rems(value / 16.);
    let rect = |x: f32, y: f32, w: f32, h: f32| {
        div()
            .absolute()
            .left(unit(x))
            .top(unit(y))
            .w(unit(w))
            .h(unit(h))
    };
    // Stroked paths are 2 wide with round caps.
    let line = |x: f32, y: f32, length: f32| {
        rect(x - 1., y - 1., length + 2., 2.)
            .rounded_full()
            .bg(c.text_metadata)
    };
    div()
        .absolute()
        .left(unit(left))
        .top_0()
        .w(unit(width))
        .h(unit(56.))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left(unit(-left))
                .top_0()
                .w(unit(80.))
                .h(unit(56.))
                .bg(c.surface_base)
                .child(rect(5., 5., 70., 46.).rounded(unit(5.)).bg(c.surface_panel))
                .child(rect(22.5, 5., 1., 46.).bg(c.border_standard))
                .child(line(10., 13., 7.))
                .child(line(10., 18., 5.))
                .child(line(29., 17., 30.))
                .child(line(29., 22., 24.))
                .child(line(29., 27., 32.))
                .child(
                    rect(45., 10., 23., 3.)
                        .rounded_full()
                        .bg(c.affordance_accent_prominent),
                )
                .child(
                    rect(49., 32., 19., 3.)
                        .rounded_full()
                        .bg(c.affordance_accent_prominent),
                )
                .child(
                    rect(29., 41., 39., 6.)
                        .rounded_full()
                        .border_1()
                        .border_color(c.border_standard),
                ),
        )
        .into_any_element()
}
