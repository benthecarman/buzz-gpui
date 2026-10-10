//! Keeps the appearance settings across launches: the color mode
//! (`buzz-appearance.v1`) and the interface size (`buzz-font-scale.v1`),
//! as device preferences in local storage.

use crate::community::AppState;
use buzz_ui::{AppearancePreference, Theme};
use gpui_kit::App;

const APPEARANCE: &str = "appearance";
const INTERFACE_SCALE: &str = "interface-scale";

/// Applies the saved settings, then saves every later change, whether it
/// comes from Settings or the Mod+= / Mod+- / Mod+0 shortcuts.
pub fn init(cx: &mut App) {
    let store = cx.global::<AppState>().store.clone();
    let saved = |name: &str| store.device_pref(name).ok().flatten();
    if let Some(preference) = saved(APPEARANCE).as_deref().and_then(parse) {
        Theme::set_preference(preference, cx);
    }
    if let Some(scale) = saved(INTERFACE_SCALE).and_then(|value| value.parse::<f32>().ok()) {
        Theme::set_interface_scale(scale, cx);
    }
    let mut last = current(cx);
    Theme::observe(cx, move |cx| {
        let now = current(cx);
        if now == last {
            return;
        }
        last = now;
        let store = &cx.global::<AppState>().store;
        let saved = store
            .set_device_pref(APPEARANCE, &format!("\"{}\"", name(now.0)))
            .and_then(|()| store.set_device_pref(INTERFACE_SCALE, &format!("{:.1}", now.1)));
        if let Err(error) = saved {
            tracing::warn!(%error, "could not save the appearance settings");
        }
    })
    .detach();
}

fn current(cx: &App) -> (AppearancePreference, f32) {
    let theme = Theme::base(cx);
    (theme.preference(), theme.interface_scale())
}

fn name(preference: AppearancePreference) -> &'static str {
    match preference {
        AppearancePreference::System => "system",
        AppearancePreference::Light => "light",
        AppearancePreference::Dark => "dark",
    }
}

fn parse(json: &str) -> Option<AppearancePreference> {
    match json.trim_matches('"') {
        "system" => Some(AppearancePreference::System),
        "light" => Some(AppearancePreference::Light),
        "dark" => Some(AppearancePreference::Dark),
        _ => None,
    }
}
