//! What the agent types' tabs share: buzz-app's form fields, the select
//! their Settings tabs choose with, and the saved settings they edit.

use crate::agents2::{AgentView, Agents2};
use crate::ui::overlay::{self, Align};
use buzz_ui::{Colors, Menu, MenuItem, Select, Surface, TypeStyled, tokens::space};
use gpui_kit::*;
use serde_json::Value;
use std::rc::Rc;

/// The choices a select offers: its value and label.
pub type Options = Vec<(String, String)>;

/// `options` given as string literals.
pub fn options(options: &[(&str, &str)]) -> Options {
    options
        .iter()
        .map(|(value, label)| (value.to_string(), label.to_string()))
        .collect()
}

/// `buzz-field`: label, control, then the description.
pub fn field(
    label: &'static str,
    control: impl IntoElement,
    description: Option<SharedString>,
    c: &Colors,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(space::S2)
        .min_w_0()
        .child(div().text_label_sm().child(label))
        .child(control)
        .children(description.map(|text| description_text(text, c)))
}

/// `buzz-field-description`.
pub fn description_text(text: impl Into<SharedString>, c: &Colors) -> Div {
    div()
        .text_body_sm()
        .text_color(c.text_subtle)
        .child(text.into())
}

/// The agent's saved settings, as they are now: they can change in
/// another view.
pub fn saved_config(view: &AgentView, cx: &App) -> Value {
    Agents2::global(cx)
        .and_then(|agents| {
            agents
                .read(cx)
                .find(&view.agent.pubkey)
                .map(|agent| agent.config.clone())
        })
        .unwrap_or_else(|| view.agent.config.clone())
}

/// A view with selects, at most one of them open.
pub trait Picking: Sized + 'static {
    type Picker: Copy + PartialEq + 'static;
    /// The open select, if any.
    fn open(&mut self) -> &mut Option<Self::Picker>;
    /// Takes `value` from `picker`'s menu; the menu is already closed.
    fn pick(&mut self, picker: Self::Picker, value: String, cx: &mut Context<Self>);
}

/// A select showing `value` among `options`, its menu below it while
/// `open` is `picker`.
#[allow(clippy::too_many_arguments)]
pub fn choice<T: Picking>(
    open: Option<T::Picker>,
    picker: T::Picker,
    id: &'static str,
    value: &str,
    options: Options,
    disabled: bool,
    window: &Window,
    cx: &mut Context<T>,
) -> Div {
    let open = open == Some(picker) && !disabled;
    let label = options
        .iter()
        .find(|(option, _)| option == value)
        .or(options.first())
        .map(|(_, label)| label.clone())
        .unwrap_or_default();
    let mut select = div().relative().w_full().child(
        Select::new(id)
            .value(label)
            .open(open)
            .surface(Surface::Floating)
            .disabled(disabled)
            .on_click(cx.listener(move |this: &mut T, _, _, cx| {
                let open = this.open();
                *open = (*open != Some(picker)).then_some(picker);
                cx.notify();
            })),
    );
    if open {
        let items = options.into_iter().map(|(option, label)| {
            MenuItem::new(SharedString::from(format!("{id}-{option}")), label)
                .checked(option == value)
                .on_click(cx.listener(move |this: &mut T, _, _, cx| {
                    *this.open() = None;
                    this.pick(picker, option.clone(), cx);
                    cx.notify();
                }))
        });
        let page = cx.entity().downgrade();
        select = select.child(overlay::dropdown(
            window,
            Align::Start,
            div().min_w(rems(16.)).child(Menu::new().children(items)),
            Rc::new(move |_, cx| {
                let _ = page.update(cx, |this, cx| {
                    *this.open() = None;
                    cx.notify();
                });
            }),
        ));
    }
    select
}
