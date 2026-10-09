//! Toast card (6.9).

use super::ClickHandler;
use crate::{
    ActiveTheme, IconButton, IconButtonSize, IconName, MaterialStyled, Surface, TypeStyled,
};
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, prelude::FluentBuilder, rems,
};
use std::rc::Rc;

/// Title color by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToastTone {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// A toast card: floating surface, padding 16, gap 8. Stack them in a
/// column 384px wide with 12px gaps, 128px from the top right.
#[derive(IntoElement)]
pub struct Toast {
    id: ElementId,
    title: SharedString,
    description: Option<SharedString>,
    tone: ToastTone,
    actions: Vec<AnyElement>,
    on_dismiss: Option<ClickHandler>,
}

impl Toast {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            tone: ToastTone::default(),
            actions: Vec::new(),
            on_dismiss: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn tone(mut self, tone: ToastTone) -> Self {
        self.tone = tone;
        self
    }

    /// An action, typically an `sm` Button.
    pub fn action(mut self, element: impl IntoElement) -> Self {
        self.actions.push(element.into_any_element());
        self
    }

    /// Shows the close button.
    pub fn on_dismiss(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for Toast {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.colors();
        let title_color = match self.tone {
            ToastTone::Error => c.text_danger,
            ToastTone::Warning => c.text_warning,
            ToastTone::Info | ToastTone::Success => c.text_standard,
        };
        let id = self.id;
        div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .w(rems(24.))
            .p(rems(1.))
            .floating_surface(c)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(rems(0.5))
                    .child(
                        div()
                            .text_label_sm()
                            .text_color(title_color)
                            .child(self.title),
                    )
                    .when_some(self.on_dismiss, |this, f| {
                        this.child(
                            IconButton::new(id, IconName::X)
                                .size(IconButtonSize::Sm)
                                .surface(Surface::Floating)
                                .on_click(move |e, w, cx| f(e, w, cx)),
                        )
                    }),
            )
            .when_some(self.description, |this, text| {
                this.child(div().text_body_sm().child(text))
            })
            .when(!self.actions.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(rems(0.5))
                        .children(self.actions),
                )
            })
    }
}
