//! The Me page (`bundled/me`): personal conversations with agents.
//!
//! A placeholder until the Me slice lands. It registers the page, so the
//! header's Me pill, search, history and the Me sidebar slot already work;
//! filling it in means replacing [`MePage`] and [`MeSidebar`].

use super::pages::{self, Layout, Navigation, Page, PageSpec, Placement};
use crate::community::AppState;
use buzz_ui::{ActiveTheme, IconName, TypeStyled};
use gpui_kit::*;

pub fn register(cx: &mut App) {
    pages::register(
        PageSpec {
            page: Page::ME,
            label: "Me",
            // buzz-app gives Me no bundled icon, so search shows the
            // generic page glyph.
            icon: IconName::Browser,
            placement: Placement::Topbar,
            layout: Layout::Workspace,
            navigation: Navigation::Own(|_, cx| cx.new(|_| MeSidebar).into()),
            collapsible: true,
            badge: None,
            build: Some(|_, cx| cx.new(|_| MePage).into()),
        },
        cx,
    );
}

pub struct MePage;

impl Render for MePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        div()
            .flex_1()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_body_sm()
            .text_color(c.text_subtle)
            .child("Start a conversation with your agents.")
    }
}

/// The Me sidebar's frame: its history list goes here.
pub struct MeSidebar;

impl Render for MeSidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.colors();
        let text = match AppState::current(cx) {
            Some(_) => "Start a conversation with your agents.",
            None => "Connect to a community to see your conversations.",
        };
        div().id("me-sidebar").size_full().p(rems(0.5)).child(
            div()
                .px(rems(1.))
                .py(rems(2.))
                .text_body_sm()
                .text_color(c.text_subtle)
                .child(text),
        )
    }
}
