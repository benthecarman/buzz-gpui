//! Dialogs a plugin opens itself (`ui/openDialog`), such as the form a
//! message action shows once its menu closes (buzz-app's message action
//! `component`). One shows at a time, over the workspace.

use super::capabilities::Failure;
use super::protocol::INVALID_PARAMS;
use super::{ExternalPlugins, Status};
use crate::ui::overlay;
use gpui_kit::*;
use serde_json::{Value, json};
use std::rc::Rc;

/// The open dialog: its plugin and view.
#[derive(Default)]
struct OpenDialog(Option<(String, AnyView)>);

impl Global for OpenDialog {}

/// The open plugin dialog over the workspace, if any.
pub fn dialog(window: &Window, cx: &App) -> Option<AnyElement> {
    let (plugin, view) = cx.try_global::<OpenDialog>()?.0.clone()?;
    // A plugin switched off takes its dialog with it.
    if ExternalPlugins::status(&plugin, cx) == Status::Off {
        return None;
    }
    Some(overlay::modal(window, view, None).into_any_element())
}

fn close(cx: &mut App) {
    cx.default_global::<OpenDialog>().0 = None;
    cx.refresh_windows();
}

/// `ui/openDialog`: a dialog titled `title` whose body is a view of kind
/// `dialog` with `contribution` set to `id` and the given `context`.
/// Replaces a dialog the plugin already has open.
pub fn open(plugin: &str, params: &Value, cx: &mut App) -> Result<Value, Failure> {
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    let title = text("title")
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .ok_or((INVALID_PARAMS, "title is required".to_owned()))?;
    let id = text("id").unwrap_or("dialog");
    if !super::protocol::valid_contribution_id(id) {
        return Err((INVALID_PARAMS, "id is not a contribution id".to_owned()));
    }
    let context = params.get("context").cloned().unwrap_or(Value::Null);
    if context.to_string().len() > 16_384 {
        return Err((INVALID_PARAMS, "context is over 16 KiB".to_owned()));
    }
    if cx
        .try_global::<OpenDialog>()
        .and_then(|open| open.0.as_ref())
        .is_some_and(|(owner, _)| owner != plugin)
    {
        return Err((1, "Another dialog is open.".to_owned()));
    }
    let title: SharedString = title.chars().take(64).collect::<String>().into();
    let (view, id) = super::view::ExternalView::framed(
        plugin,
        id,
        title,
        Rc::new(|_, cx| close(cx)),
        json!({ "context": context }),
        cx,
    );
    cx.default_global::<OpenDialog>().0 = Some((plugin.to_owned(), view));
    cx.refresh_windows();
    Ok(json!({ "view": id }))
}
