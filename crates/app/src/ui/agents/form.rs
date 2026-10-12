//! Layout shared by the agent dialogs (ui.md §5a.7, §5a.8) and the agent
//! defaults dialog (§5a.11).

use buzz_ui::{Colors, Icon, IconName, Menu, MenuSize, TypeStyled, tokens::radius};
use gpui_kit::*;

/// `Field`: label, control, then the description (§6.3).
pub fn field(
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    c: &Colors,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(rems(0.5))
        .min_w_0()
        .child(div().text_label_sm().child(label.into()))
        .child(control)
        .children(
            description.map(|text| div().text_body_sm().text_color(c.text_subtle).child(text)),
        )
}

/// An accordion header (`Accordion variant="form"`): the title, then a
/// caret that points up while `open`. The caller adds the click.
pub fn accordion_header(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    open: bool,
    c: &Colors,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .py(rems(0.5))
        .px(rems(0.5))
        .mx(rems(-0.5))
        .rounded(radius::CONTROL)
        .cursor_pointer()
        .hover(|this| this.bg(c.affordance_subtle_hover))
        .child(div().text_label().child(title.into()))
        .child(
            Icon::new(if open {
                IconName::CaretUp
            } else {
                IconName::CaretDown
            })
            .size(16.)
            .color(c.text_subtle),
        )
}

/// A `<fieldset>` legend in `text-label`.
pub fn legend(text: &'static str) -> Div {
    div().text_label().child(text)
}

/// A select's menu. Long lists scroll inside the menu's surface, which
/// would otherwise cut them off.
#[derive(IntoElement)]
pub struct Options {
    id: &'static str,
    items: Vec<AnyElement>,
}

impl Options {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            items: Vec::new(),
        }
    }

    pub fn child(mut self, item: impl IntoElement) -> Self {
        self.items.push(item.into_any_element());
        self
    }
}

impl RenderOnce for Options {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Menu::new().size(MenuSize::Wide).child(
            div()
                .id(self.id)
                .flex()
                .flex_col()
                .max_h(rems(23.))
                .overflow_y_scroll()
                .children(self.items),
        )
    }
}
