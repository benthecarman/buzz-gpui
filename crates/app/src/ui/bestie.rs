//! Bestie (`bundled/bestie`): the companion's home page until agent chat
//! connects. Off by default; Settings → Plugins turns it on.

use super::pages::{self, Page, PageSpec};
use buzz_ui::{ActiveTheme, IconName, PanelHeader, TypeStyled};
use gpui_kit::*;

pub fn register(cx: &mut App) {
    pages::register(
        PageSpec::sidebar(
            Page::new("buzz.bestie/bestie"),
            "Bestie",
            IconName::Bestie,
            |_, cx| cx.new(|_| BestiePage).into(),
        ),
        cx,
    );
}

struct BestiePage;

impl Render for BestiePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .text_color(c.text_standard)
            .child(
                PanelHeader::new()
                    .joined(true)
                    .label(Some(IconName::Bestie), "Bestie"),
            )
            .child(
                div()
                    .id("bestie")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(rems(1.))
                    .p(rems(1.5))
                    .text_center()
                    .child(img("icons/custom/bestie.png").size(rems(5.)))
                    .child(div().text_heading().child("Meet your Bestie"))
                    .child(
                        div()
                            .max_w(rems(20.))
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child(
                                "Your companion’s home in Buzz. Agent chat isn’t connected yet.",
                            ),
                    ),
            )
    }
}
