//! PanelHeader and PanelHeaderLabel (6.12).

use crate::{ActiveTheme, Icon, IconName, TypeStyled};
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::FluentBuilder, px, rems,
};

/// A 40px pane header with a title or label and trailing actions, over a
/// hairline inset 4px from each side (flush inside joined panels).
#[derive(IntoElement)]
pub struct PanelHeader {
    title: Option<SharedString>,
    label: Option<(Option<IconName>, SharedString)>,
    leading: Vec<AnyElement>,
    actions: Vec<AnyElement>,
    joined: bool,
    divider: bool,
}

impl PanelHeader {
    pub fn new() -> Self {
        Self {
            title: None,
            label: None,
            leading: Vec::new(),
            actions: Vec::new(),
            joined: false,
            divider: true,
        }
    }

    /// An `h2` title in `text-label` (16/24/500).
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// A PanelHeaderLabel: body-sm with a 16px icon slot, padding-inline 8.
    pub fn label(mut self, icon: Option<IconName>, text: impl Into<SharedString>) -> Self {
        self.label = Some((icon, text.into()));
        self
    }

    /// Extra content after the title.
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading.push(element.into_any_element());
        self
    }

    /// Trailing actions, usually 32px ghost IconButtons.
    pub fn action(mut self, element: impl IntoElement) -> Self {
        self.actions.push(element.into_any_element());
        self
    }

    /// Inside a joined panel the hairline runs edge to edge.
    pub fn joined(mut self, joined: bool) -> Self {
        self.joined = joined;
        self
    }

    pub fn divider(mut self, divider: bool) -> Self {
        self.divider = divider;
        self
    }
}

impl Default for PanelHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for PanelHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(rems(0.5))
                    .min_h(rems(2.5))
                    .px(rems(0.25))
                    .text_color(c.text_standard)
                    .when_some(self.title, |this, title| {
                        this.child(div().px(rems(0.5)).text_label().child(title))
                    })
                    .when_some(self.label, |this, (icon, text)| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(rems(0.5))
                                .px(rems(0.5))
                                .text_body_sm()
                                .children(icon.map(|icon| Icon::new(icon).size(16.)))
                                .child(text),
                        )
                    })
                    .children(self.leading)
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.25))
                            .children(self.actions),
                    ),
            )
            .when(self.divider, |this| {
                this.child(
                    div()
                        .h(px(1.))
                        .bg(c.border_standard)
                        .when(!self.joined, |this| this.mx(rems(0.25))),
                )
            })
    }
}
