//! Layout shared by the agent dialogs (ui.md §5a.7, §5a.8) and the agent
//! defaults dialog (§5a.11).

use buzz_ui::{Colors, Menu, MenuSize, TypeStyled};
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
