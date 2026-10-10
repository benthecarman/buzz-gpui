//! Keyboard shortcuts (`src/app/shortcuts.ts`, `src/features/shortcuts`):
//! the host shortcut registry, the person's rebinds saved on this device
//! (`buzz-shortcut-bindings.v1`), and the GPUI keymap built from both.
//!
//! Bindings use buzz-app's shape, a DOM key name plus `mod`, `shift` and
//! `alt` flags, so the saved overrides read the same in both apps. The
//! keymap is rebuilt from the registry on every change, so a rebind takes
//! effect at once and never needs the action's owner to bind again.

use crate::community::AppState;
use crate::ui::navigation;
use gpui_kit::*;
use serde::Serialize;
use std::any::TypeId;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};

/// Device-preference key for the overrides (`buzz-shortcut-bindings.v1`).
const PREFERENCE: &str = "shortcut-bindings";

const RESTORE_ERROR: &str = "Custom shortcuts could not be restored. Defaults are active; change a shortcut to try saving again.";
const SAVE_ERROR: &str =
    "Your shortcuts are active, but could not be saved on this device. Try again.";

/// `mod` is Command on Apple platforms and Control elsewhere.
pub const APPLE: bool = cfg!(target_os = "macos");
/// The packaged desktop app, where close and quit belong to the shell.
pub const DESKTOP: bool = true;

/// One chord: a DOM key name (`"k"`, `","`, `"Enter"`, `"ArrowLeft"`)
/// and its modifiers. Matching ignores the key's case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub key: Cow<'static, str>,
    #[serde(rename = "mod", default, skip_serializing_if = "is_false")]
    pub primary: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub shift: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub alt: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Binding {
    const fn new(key: &'static str, primary: bool, shift: bool, alt: bool) -> Self {
        Self {
            key: Cow::Borrowed(key),
            primary,
            shift,
            alt,
        }
    }

    const fn primary(key: &'static str) -> Self {
        Self::new(key, true, false, false)
    }

    /// The same chord under the dispatcher's rules: case-insensitive key,
    /// exact modifiers (`sameBinding`).
    pub fn same(&self, other: &Binding) -> bool {
        self.key.to_lowercase() == other.key.to_lowercase()
            && self.primary == other.primary
            && self.shift == other.shift
            && self.alt == other.alt
    }

    /// `formatBinding(...).text`: "Ctrl+Shift+K" here, "⇧⌘K" on macOS.
    pub fn text(&self, apple: bool) -> String {
        let key = match special_key(&self.key) {
            Some(name) => name.to_string(),
            None if self.key.chars().count() == 1 => self.key.to_uppercase(),
            None => self.key.to_string(),
        };
        let mut parts = Vec::new();
        if apple {
            // Control, Option, Shift, Command.
            for (on, glyph) in [(self.alt, "⌥"), (self.shift, "⇧"), (self.primary, "⌘")] {
                if on {
                    parts.push(glyph);
                }
            }
            parts.push(&key);
            parts.concat()
        } else {
            for (on, word) in [
                (self.primary, "Ctrl"),
                (self.alt, "Alt"),
                (self.shift, "Shift"),
            ] {
                if on {
                    parts.push(word);
                }
            }
            parts.push(&key);
            parts.join("+")
        }
    }

    /// The GPUI keystroke this chord is typed as, e.g. `secondary-shift-a`.
    fn keystroke(&self) -> String {
        let mut source = String::new();
        for (on, modifier) in [
            (self.primary, "secondary-"),
            (self.alt, "alt-"),
            (self.shift, "shift-"),
        ] {
            if on {
                source.push_str(modifier);
            }
        }
        source.push_str(&gpui_key(&self.key));
        source
    }
}

/// Display names for keys whose DOM name is not what a person reads.
fn special_key(key: &str) -> Option<&'static str> {
    Some(match key.to_lowercase().as_str() {
        " " => "Space",
        "enter" => "Enter",
        "escape" => "Escape",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "arrowup" => "↑",
        "arrowdown" => "↓",
        "arrowleft" => "←",
        "arrowright" => "→",
        _ => return None,
    })
}

/// GPUI's name for a DOM key.
fn gpui_key(key: &str) -> String {
    match key {
        " " => "space".into(),
        "ArrowUp" => "up".into(),
        "ArrowDown" => "down".into(),
        "ArrowLeft" => "left".into(),
        "ArrowRight" => "right".into(),
        other => other.to_lowercase(),
    }
}

/// The DOM name for a key GPUI reports.
fn dom_key(key: &str) -> Cow<'static, str> {
    let named = match key {
        "" | "voidsymbol" => "Unidentified",
        "space" => " ",
        "enter" => "Enter",
        "escape" => "Escape",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        key if key.starts_with("dead_") => "Dead",
        key if key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok() => {
            return Cow::Owned(key.to_uppercase());
        }
        key => return Cow::Owned(key.to_lowercase()),
    };
    Cow::Borrowed(named)
}

/// A chord pressed in the capture control (`CapturedChord`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    pub binding: Binding,
    /// Control on Apple platforms, or the Windows/Super key elsewhere:
    /// the dispatcher never matches it.
    pub other_primary: bool,
}

/// What the capture control does with a keystroke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Press {
    /// A modifier alone: keep listening.
    Ignore,
    /// Plain Tab: leave the control, as focus traversal.
    Leave,
    /// Escape, whatever else is held: stop listening.
    Cancel,
    Chord(Captured),
}

/// Reads one keystroke the way `KeyCaptureControl` reads a keydown.
pub fn press(keystroke: &Keystroke, apple: bool) -> Press {
    let modifiers = keystroke.modifiers;
    match keystroke.key.as_str() {
        "shift" | "control" | "alt" | "platform" | "function" | "capslock" | "numlock" => {
            return Press::Ignore;
        }
        "tab" if !modifiers.control && !modifiers.platform && !modifiers.alt => {
            return Press::Leave;
        }
        "escape" => return Press::Cancel,
        _ => {}
    }
    let (primary, other) = if apple {
        (modifiers.platform, modifiers.control)
    } else {
        (modifiers.control, modifiers.platform)
    };
    Press::Chord(Captured {
        binding: Binding {
            key: dom_key(&keystroke.key),
            primary,
            shift: modifiers.shift,
            alt: modifiers.alt,
        },
        other_primary: other,
    })
}

// ----- the registry -----

/// A shortcut the app itself owns (`registerHost`).
pub struct HostShortcut {
    /// buzz-app's host id, which keys the saved override.
    pub id: &'static str,
    pub title: &'static str,
    /// Presentation order in Settings (`HOST_SHORTCUT_ORDER`).
    order: u32,
    /// Aliases; the first is the one shown.
    pub defaults: &'static [Binding],
    action: fn() -> Box<dyn Action>,
}

/// The host shortcuts, in Settings order. Reload Buzz is left out: it
/// reloads the webview, which this app does not have.
pub const HOST: &[HostShortcut] = &[
    HostShortcut {
        id: "home",
        title: "Home",
        order: 10,
        defaults: &[Binding::new("a", true, true, false)],
        action: || Box::new(navigation::GoHome),
    },
    HostShortcut {
        id: "navigation-back",
        title: "Go back",
        order: 20,
        defaults: &[Binding::primary("[")],
        action: || Box::new(navigation::GoBack),
    },
    HostShortcut {
        id: "navigation-forward",
        title: "Go forward",
        order: 30,
        defaults: &[Binding::primary("]")],
        action: || Box::new(navigation::GoForward),
    },
    HostShortcut {
        id: "font-increase",
        title: "Increase interface size",
        order: 40,
        defaults: &[
            Binding::primary("="),
            Binding::new("=", true, true, false),
            Binding::primary("+"),
            Binding::new("+", true, true, false),
        ],
        action: || Box::new(buzz_ui::IncreaseInterfaceSize),
    },
    HostShortcut {
        id: "font-decrease",
        title: "Decrease interface size",
        order: 50,
        defaults: &[Binding::primary("-")],
        action: || Box::new(buzz_ui::DecreaseInterfaceSize),
    },
    HostShortcut {
        id: "font-reset",
        title: "Reset interface size",
        order: 60,
        defaults: &[Binding::primary("0")],
        action: || Box::new(buzz_ui::ResetInterfaceSize),
    },
    HostShortcut {
        id: "global-search",
        title: "Search Buzz",
        order: 70,
        defaults: &[Binding::primary("k")],
        action: || Box::new(navigation::SearchBuzz),
    },
    HostShortcut {
        id: "conversation-search",
        title: "Search this conversation",
        order: 71,
        defaults: &[Binding::primary("f")],
        action: || Box::new(navigation::SearchConversation),
    },
    HostShortcut {
        id: "settings",
        title: "Open Settings",
        order: 80,
        defaults: &[Binding::primary(",")],
        action: || Box::new(crate::ui::settings::OpenSettings),
    },
    HostShortcut {
        id: "close-tab",
        title: "Close tab or window",
        order: 85,
        defaults: &[Binding::primary("w")],
        action: || Box::new(navigation::CloseTab),
    },
];

/// Copy, paste, cut and select all: a shortcut there would take them
/// from every field.
const CLIPBOARD_CHORDS: &[Binding] = &[
    Binding::primary("c"),
    Binding::primary("v"),
    Binding::primary("x"),
    Binding::primary("a"),
];
/// Close tab or window, and quit: the desktop shell owns these.
const DESKTOP_CHORDS: &[Binding] = &[Binding::primary("q"), Binding::primary("w")];
/// Chords the message editor handles itself: undo, redo and line or
/// document jumps.
const EDITOR_CHORDS: &[Binding] = &[
    Binding::primary("z"),
    Binding::new("z", true, true, false),
    Binding::primary("y"),
    Binding::new("y", true, true, false),
    Binding::primary("Home"),
    Binding::new("Home", true, true, false),
    Binding::primary("End"),
    Binding::new("End", true, true, false),
];

fn includes(chords: &[Binding], binding: &Binding) -> bool {
    chords.iter().any(|chord| chord.same(binding))
}

/// A Settings row: a registered shortcut and what it answers to now.
#[derive(Clone, Debug)]
pub struct Row {
    pub key: &'static str,
    pub title: &'static str,
    /// Who registered it, as Settings names it.
    pub owner: &'static str,
    /// Close and quit defaults on desktop: shown, not rebindable.
    pub read_only: bool,
    pub defaults: &'static [Binding],
    pub override_: Option<Binding>,
    pub effective: Vec<Binding>,
}

/// Every host shortcut with its override applied, in Settings order.
pub fn rows(overrides: &Overrides, desktop: bool) -> Vec<Row> {
    let mut rows: Vec<(u32, Row)> = HOST
        .iter()
        .map(|shortcut| {
            let override_ = overrides.get(shortcut.id).cloned();
            let effective = match &override_ {
                Some(binding) => vec![binding.clone()],
                None => shortcut.defaults.to_vec(),
            };
            let read_only = desktop
                && shortcut
                    .defaults
                    .iter()
                    .any(|binding| includes(DESKTOP_CHORDS, binding));
            let row = Row {
                key: shortcut.id,
                title: shortcut.title,
                owner: "Buzz",
                read_only,
                defaults: shortcut.defaults,
                override_,
                effective,
            };
            (shortcut.order, row)
        })
        .collect();
    rows.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.key.cmp(y.key)).then(x.title.cmp(y.title)));
    rows.into_iter().map(|(_, row)| row).collect()
}

/// The other rows answering to one of `target`'s chords. A chord can
/// become shared after capture (a new default in a release); the keymap
/// then picks one, so each row names the others.
pub fn shared_with<'a>(rows: &'a [Row], target: &Row) -> Vec<&'a Row> {
    rows.iter()
        .filter(|other| {
            other.key != target.key
                && other
                    .effective
                    .iter()
                    .any(|binding| target.effective.iter().any(|own| own.same(binding)))
        })
        .collect()
}

/// A captured chord the page accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    /// The override to save; `None` when the chord is the default again.
    pub binding: Option<Binding>,
    /// Shown under the row after saving.
    pub warning: Option<String>,
}

/// Decides whether `target` may take a captured chord, with the refusal
/// to show under the row when not (`ShortcutSettings.capture`). Conflicts
/// are decided here rather than left to the keymap's precedence.
pub fn accept(
    target: &Row,
    captured: &Captured,
    rows: &[Row],
    apple: bool,
    desktop: bool,
) -> Result<Accepted, String> {
    let binding = &captured.binding;
    let chord = binding.text(apple);
    if binding.key == "Dead" || binding.key == "Unidentified" {
        return Err("That key can’t be used for a shortcut. Try another.".into());
    }
    if captured.other_primary {
        return Err(if apple {
            "Control isn’t used for shortcuts on this device. Try Command or Option."
        } else {
            "The Windows/Command key isn’t used for shortcuts on this device. Try Control or Alt."
        }
        .into());
    }
    if !binding.primary && !binding.alt {
        let keys = if apple {
            "Command or Option"
        } else {
            "Control or Alt"
        };
        return Err(format!("Include {keys} so ordinary typing keeps working."));
    }
    if includes(CLIPBOARD_CHORDS, binding) {
        return Err(format!(
            "{chord} is reserved for copy, cut, paste and select all. Try another."
        ));
    }
    if desktop && includes(DESKTOP_CHORDS, binding) {
        return Err(format!(
            "{chord} is reserved for closing tabs or the window and quitting Buzz. Try another."
        ));
    }
    if let Some(conflict) = rows.iter().find(|row| {
        row.key != target.key && row.effective.iter().any(|current| current.same(binding))
    }) {
        return Err(format!(
            "{chord} is already used by {} ({}).",
            conflict.title, conflict.owner
        ));
    }
    let is_default = target.defaults.len() == 1 && target.defaults[0].same(binding);
    // The editor handles Enter itself whatever modifiers are held.
    let warning = (binding.key == "Enter" || includes(EDITOR_CHORDS, binding)).then(|| {
        format!(
            "Saved. The message editor handles {chord} itself while you are typing, so it wins there."
        )
    });
    Ok(Accepted {
        binding: (!is_default).then(|| binding.clone()),
        warning,
    })
}

// ----- saved overrides -----

/// Rebinds by host id.
pub type Overrides = BTreeMap<String, Binding>;

/// Keeps every well-formed entry and drops only malformed ones
/// (`parseOverrides`).
pub fn parse_overrides(raw: &serde_json::Value) -> Overrides {
    let Some(entries) = raw.as_object() else {
        return Overrides::new();
    };
    entries
        .iter()
        .filter(|(key, _)| !key.is_empty())
        .filter_map(|(key, value)| {
            let binding = value.as_object()?;
            let flag = |name: &str| match binding.get(name) {
                None => Some(false),
                Some(serde_json::Value::Bool(on)) => Some(*on),
                Some(_) => None,
            };
            let name = binding.get("key")?.as_str().filter(|key| !key.is_empty())?;
            Some((
                key.clone(),
                Binding {
                    key: Cow::Owned(name.to_string()),
                    primary: flag("mod")?,
                    shift: flag("shift")?,
                    alt: flag("alt")?,
                },
            ))
        })
        .collect()
}

/// What a saved value restores to: the overrides, or none and the
/// restore error when it cannot be read.
fn restore<E>(saved: Result<Option<String>, E>) -> (Overrides, Option<&'static str>) {
    match saved {
        Ok(None) => (Overrides::new(), None),
        Ok(Some(json)) => match serde_json::from_str(&json) {
            Ok(value) => (parse_overrides(&value), None),
            Err(_) => (Overrides::new(), Some(RESTORE_ERROR)),
        },
        Err(_) => (Overrides::new(), Some(RESTORE_ERROR)),
    }
}

/// The person's rebinds on this device. A change applies even when it
/// cannot be saved; the page then offers a retry.
pub struct Shortcuts {
    overrides: Overrides,
    error: Option<&'static str>,
}

impl Global for Shortcuts {}

impl Shortcuts {
    pub fn overrides(cx: &App) -> &Overrides {
        &cx.global::<Self>().overrides
    }

    pub fn error(cx: &App) -> Option<&'static str> {
        cx.global::<Self>().error
    }

    /// Rebinds `id`, or with `None` puts its default back.
    pub fn set(id: &str, binding: Option<Binding>, cx: &mut App) {
        let mut overrides = Self::overrides(cx).clone();
        match binding {
            Some(binding) => overrides.insert(id.to_string(), binding),
            None => overrides.remove(id),
        };
        commit(overrides, cx);
    }

    /// Reset all shortcuts.
    pub fn reset(cx: &mut App) {
        commit(Overrides::new(), cx);
    }

    /// Tries again to save what is already active.
    pub fn retry(cx: &mut App) {
        commit(Self::overrides(cx).clone(), cx);
    }
}

/// The chord `id` answers to now, as text, for hints such as the search
/// button's tooltip.
pub fn text(id: &str, cx: &App) -> String {
    let binding = Shortcuts::overrides(cx).get(id).or_else(|| {
        HOST.iter()
            .find(|shortcut| shortcut.id == id)
            .and_then(|shortcut| shortcut.defaults.first())
    });
    binding
        .map(|binding| binding.text(APPLE))
        .unwrap_or_default()
}

fn commit(overrides: Overrides, cx: &mut App) {
    let json = serde_json::to_string(&overrides).unwrap_or_else(|_| "{}".into());
    let saved = cx
        .global::<AppState>()
        .store
        .set_device_pref(PREFERENCE, &json);
    if let Err(error) = &saved {
        tracing::warn!(%error, "could not save the shortcuts");
    }
    cx.set_global(Shortcuts {
        overrides,
        error: saved.is_err().then_some(SAVE_ERROR),
    });
    rebuild_keymap(cx);
}

/// Restores the saved rebinds, binds every host shortcut and handles the
/// actions this module owns. Call after the action owners' `init`.
pub fn init(cx: &mut App) {
    let saved = cx.global::<AppState>().store.device_pref(PREFERENCE);
    let (overrides, error) = restore(saved);
    cx.set_global(Shortcuts { overrides, error });
    rebuild_keymap(cx);
}

/// Replaces every app-wide binding of a host action with the current
/// chords. Bindings in a narrower context (a text field's own undo, say)
/// stay and still win where they apply.
fn rebuild_keymap(cx: &mut App) {
    let keymap = cx.key_bindings();
    let bindings = keymap_bindings(keymap.borrow().bindings(), Shortcuts::overrides(cx));
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

/// The keymap with the host shortcuts' app-wide bindings replaced.
fn keymap_bindings<'a>(
    existing: impl Iterator<Item = &'a KeyBinding>,
    overrides: &Overrides,
) -> Vec<KeyBinding> {
    let host: HashSet<TypeId> = HOST
        .iter()
        .map(|shortcut| (shortcut.action)().as_any().type_id())
        .collect();
    let mut bindings: Vec<KeyBinding> = existing
        .filter(|binding| {
            binding.predicate().is_some() || !host.contains(&binding.action().as_any().type_id())
        })
        .cloned()
        .collect();
    for row in rows(overrides, DESKTOP) {
        let action = HOST
            .iter()
            .find(|shortcut| shortcut.id == row.key)
            .map(|shortcut| shortcut.action);
        let Some(action) = action else { continue };
        for binding in &row.effective {
            match KeyBinding::load(
                &binding.keystroke(),
                action(),
                None,
                false,
                None,
                &DummyKeyboardMapper,
            ) {
                Ok(binding) => bindings.push(binding),
                Err(error) => tracing::warn!(%error, id = row.key, "skipping a shortcut"),
            }
        }
    }
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    // gpui_kit's glob brings its own `test` attribute.
    use core::prelude::v1::test;

    fn chord(key: &'static str, primary: bool, shift: bool, alt: bool) -> Captured {
        Captured {
            binding: Binding::new(key, primary, shift, alt),
            other_primary: false,
        }
    }

    fn row<'a>(rows: &'a [Row], key: &str) -> &'a Row {
        rows.iter().find(|row| row.key == key).unwrap()
    }

    #[test]
    fn formats_like_buzz_app() {
        let k = Binding::new("k", true, true, false);
        assert_eq!(k.text(false), "Ctrl+Shift+K");
        assert_eq!(k.text(true), "⇧⌘K");
        assert_eq!(
            Binding::new("k", true, true, true).text(false),
            "Ctrl+Alt+Shift+K"
        );
        assert_eq!(Binding::new("k", true, true, true).text(true), "⌥⇧⌘K");
        assert_eq!(
            Binding::new("ArrowRight", false, false, true).text(false),
            "Alt+→"
        );
        assert_eq!(Binding::primary(" ").text(true), "⌘Space");
        assert_eq!(Binding::primary("Enter").text(false), "Ctrl+Enter");
        assert_eq!(Binding::primary("=").text(false), "Ctrl+=");
        assert_eq!(Binding::new("+", true, true, false).text(true), "⇧⌘+");
        assert_eq!(Binding::primary("PageUp").text(true), "⌘PageUp");
    }

    #[test]
    fn rows_follow_the_registry_order() {
        let rows = rows(&Overrides::new(), true);
        let titles: Vec<_> = rows.iter().map(|row| row.title).collect();
        assert_eq!(
            titles,
            [
                "Home",
                "Go back",
                "Go forward",
                "Increase interface size",
                "Decrease interface size",
                "Reset interface size",
                "Search Buzz",
                "Search this conversation",
                "Open Settings",
                "Close tab or window",
            ]
        );
        let read_only: Vec<_> = rows.iter().filter(|r| r.read_only).map(|r| r.key).collect();
        assert_eq!(read_only, ["close-tab"]);
    }

    #[test]
    fn refuses_what_buzz_app_refuses() {
        let rows = rows(&Overrides::new(), true);
        let settings = row(&rows, "settings");
        let refuse = |captured: Captured| accept(settings, &captured, &rows, false, true);
        assert_eq!(
            refuse(chord("Dead", true, false, false)).unwrap_err(),
            "That key can’t be used for a shortcut. Try another."
        );
        let mut windows = chord("p", true, false, false);
        windows.other_primary = true;
        assert_eq!(
            refuse(windows).unwrap_err(),
            "The Windows/Command key isn’t used for shortcuts on this device. Try Control or Alt."
        );
        assert_eq!(
            refuse(chord("p", false, true, false)).unwrap_err(),
            "Include Control or Alt so ordinary typing keeps working."
        );
        assert_eq!(
            refuse(chord("c", true, false, false)).unwrap_err(),
            "Ctrl+C is reserved for copy, cut, paste and select all. Try another."
        );
        assert_eq!(
            refuse(chord("q", true, false, false)).unwrap_err(),
            "Ctrl+Q is reserved for closing tabs or the window and quitting Buzz. Try another."
        );
        assert_eq!(
            refuse(chord("k", true, false, false)).unwrap_err(),
            "Ctrl+K is already used by Search Buzz (Buzz)."
        );
        // An alias counts as used, too.
        assert_eq!(
            refuse(chord("+", true, false, false)).unwrap_err(),
            "Ctrl++ is already used by Increase interface size (Buzz)."
        );
        // Apple wording.
        assert_eq!(
            accept(
                settings,
                &chord("p", false, false, false),
                &rows,
                true,
                true
            )
            .unwrap_err(),
            "Include Command or Option so ordinary typing keeps working."
        );
    }

    #[test]
    fn accepts_rebinds_defaults_and_warns_for_editor_chords() {
        let rows = rows(&Overrides::new(), true);
        let settings = row(&rows, "settings");
        assert_eq!(
            accept(
                settings,
                &chord(";", true, false, false),
                &rows,
                false,
                true
            ),
            Ok(Accepted {
                binding: Some(Binding::primary(";")),
                warning: None
            })
        );
        // The default again clears the override.
        assert_eq!(
            accept(
                settings,
                &chord(",", true, false, false),
                &rows,
                false,
                true
            ),
            Ok(Accepted {
                binding: None,
                warning: None
            })
        );
        // Alt alone is enough, as is Enter with a modifier, with a warning.
        let enter = accept(
            settings,
            &chord("Enter", true, false, false),
            &rows,
            false,
            true,
        );
        assert_eq!(
            enter.unwrap().warning.as_deref(),
            Some(
                "Saved. The message editor handles Ctrl+Enter itself while you are typing, so it wins there."
            )
        );
        let undo = accept(
            settings,
            &chord("z", true, false, false),
            &rows,
            false,
            true,
        );
        assert!(undo.unwrap().warning.is_some());
        // A multi-alias default is overridden even with its first chord.
        let increase = row(&rows, "font-increase");
        assert_eq!(
            accept(
                increase,
                &chord("=", true, false, false),
                &rows,
                false,
                true
            )
            .unwrap()
            .binding,
            Some(Binding::primary("="))
        );
    }

    #[test]
    fn overrides_mark_rows_and_shared_chords() {
        let mut overrides = Overrides::new();
        overrides.insert("settings".into(), Binding::primary("k"));
        let rows = rows(&overrides, true);
        let settings = row(&rows, "settings");
        assert_eq!(settings.effective, [Binding::primary("k")]);
        assert!(settings.override_.is_some());
        let shared: Vec<_> = shared_with(&rows, settings)
            .iter()
            .map(|r| r.title)
            .collect();
        assert_eq!(shared, ["Search Buzz"]);
        assert!(shared_with(&rows, row(&rows, "home")).is_empty());
    }

    #[test]
    fn parses_well_formed_entries_only() {
        let value = |json: &str| serde_json::from_str::<serde_json::Value>(json).unwrap();
        let parsed = parse_overrides(&value(
            r#"{
                "example.counter/increment": { "key": "u", "mod": true, "shift": true },
                "settings": { "key": ";", "mod": true, "alt": false, "extra": "ignored" },
                "empty": { "key": "" },
                "text": "not a binding",
                "flags": { "key": "k", "mod": "yes" },
                "": { "key": "k", "mod": true }
            }"#,
        ));
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed["example.counter/increment"],
            Binding::new("u", true, true, false)
        );
        assert_eq!(parsed["settings"], Binding::primary(";"));
        for raw in ["null", "[]", "\"text\"", "42"] {
            assert!(parse_overrides(&value(raw)).is_empty());
        }
    }

    #[test]
    fn restores_and_reports_unreadable_values() {
        let ok: Result<_, ()> = Ok(Some(r#"{"settings":{"key":";","mod":true}}"#.to_string()));
        let (overrides, error) = restore(ok);
        assert_eq!(overrides["settings"], Binding::primary(";"));
        assert_eq!(error, None);
        let (overrides, error) = restore::<()>(Ok(Some("{not json".into())));
        assert!(overrides.is_empty());
        assert!(error.unwrap().contains("could not be restored"));
        let (_, error) = restore::<()>(Err(()));
        assert!(error.is_some());
        assert_eq!(restore::<()>(Ok(None)).1, None);
    }

    #[test]
    fn saves_in_buzz_app_shape() {
        let mut overrides = Overrides::new();
        overrides.insert("settings".into(), Binding::primary(";"));
        overrides.insert("home".into(), Binding::new("h", false, true, true));
        assert_eq!(
            serde_json::to_string(&overrides).unwrap(),
            r#"{"home":{"key":"h","shift":true,"alt":true},"settings":{"key":";","mod":true}}"#
        );
    }

    #[test]
    fn reads_captured_keystrokes() {
        let parse = |source: &str| press(&Keystroke::parse(source).unwrap(), false);
        assert_eq!(
            parse("ctrl-shift-a"),
            Press::Chord(chord("a", true, true, false))
        );
        assert_eq!(
            parse("alt-left"),
            Press::Chord(chord("ArrowLeft", false, false, true))
        );
        assert_eq!(
            parse("ctrl-space"),
            Press::Chord(chord(" ", true, false, false))
        );
        assert_eq!(
            parse("ctrl-f5"),
            Press::Chord(chord("F5", true, false, false))
        );
        assert_eq!(parse("tab"), Press::Leave);
        assert_eq!(parse("shift-escape"), Press::Cancel);
        assert_eq!(parse("control"), Press::Ignore);
        let Press::Chord(dead) = parse("dead_acute") else {
            panic!("a chord")
        };
        assert_eq!(dead.binding.key, "Dead");
        let Press::Chord(windows) = parse("super-k") else {
            panic!("a chord")
        };
        assert!(windows.other_primary);
        // Apple: Command is mod, Control is the other primary.
        let Press::Chord(apple) = press(&Keystroke::parse("cmd-k").unwrap(), true) else {
            panic!("a chord")
        };
        assert_eq!(apple.binding, Binding::primary("k"));
    }

    #[test]
    fn keymap_follows_overrides() {
        let typed = |source: &str| [Keystroke::parse(source).unwrap()];
        let other = KeyBinding::new("ctrl-q", buzz_ui::Cancel, None);
        let scoped = KeyBinding::new("ctrl-k", buzz_ui::Cancel, Some("Composer"));
        let stale = KeyBinding::new("ctrl-,", crate::ui::settings::OpenSettings, None);
        let defaults = Keymap::new(keymap_bindings(
            [&other, &scoped, &stale].into_iter(),
            &Overrides::new(),
        ));
        let action = |keymap: &Keymap, source: &str| {
            keymap
                .bindings_for_input(&typed(source), &[] as &[KeyContext])
                .0
                .first()
                .map(|binding| binding.action().name())
        };
        assert_eq!(action(&defaults, "ctrl-,"), Some("buzz::OpenSettings"));
        assert_eq!(
            action(&defaults, "ctrl-shift-a"),
            Some("buzz_navigation::GoHome")
        );
        assert_eq!(
            action(&defaults, "ctrl-+"),
            Some("buzz_ui::IncreaseInterfaceSize")
        );
        assert_eq!(action(&defaults, "ctrl-q"), Some("buzz_ui::Cancel"));
        // The stale app-wide binding goes; the others stay, beside 13 chords.
        assert_eq!(defaults.bindings().count(), 2 + 13);
        let mut overrides = Overrides::new();
        overrides.insert("settings".into(), Binding::primary(";"));
        let rebound = Keymap::new(keymap_bindings(defaults.bindings(), &overrides));
        assert_eq!(action(&rebound, "ctrl-;"), Some("buzz::OpenSettings"));
        assert_eq!(action(&rebound, "ctrl-,"), None);
        assert_eq!(rebound.bindings().count(), defaults.bindings().count());
    }
}
