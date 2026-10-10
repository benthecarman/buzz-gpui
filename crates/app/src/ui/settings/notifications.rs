//! Settings → Notifications (§5b.5 item 3, buzz-app's
//! `NotificationSettings`): the selected community's notification
//! preferences and alert sounds. Banners are always silent; the app plays
//! the chosen sound itself.

use super::{SettingsPage, header, inline_header, note, preference_row, rows_group, section};
use crate::community::AppState;
use crate::notifications::Preferences;
use crate::sounds::{self, Sound};
use crate::ui::overlay::{self, Align};
use buzz_relay_client::{Category, Partition, Store};
use buzz_ui::{ActiveTheme, Button, ButtonShape, ButtonSize, IconName, Menu, MenuItem, Switch};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

/// Saves the preferences and redraws.
type Save = Rc<dyn Fn(Preferences, &mut Window)>;

/// Sound rows in the reference order: direct messages, mentions, threads.
const SOUND_ROWS: [Category; 3] = [Category::Direct, Category::Mention, Category::Thread];

fn sound_label(category: Category) -> &'static str {
    match category {
        Category::Direct => "Direct messages",
        Category::Mention => "@Mentions",
        Category::Thread => "Thread replies",
    }
}

fn option_label(sound: Sound, category: Category) -> String {
    if sound == Sound::Silent {
        "Silent".into()
    } else if sound == Sound::recommended(category) {
        format!("{} rec.", sound.name())
    } else {
        sound.name().into()
    }
}

impl SettingsPage {
    pub(super) fn notifications_section(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(community) = AppState::current(cx) else {
            return section(vec![
                header("Notifications", None, cx).into_any_element(),
                note("Add a community to choose its notifications.", cx).into_any_element(),
            ]);
        };
        let store = cx.global::<AppState>().store.clone();
        let (name, partition) = {
            let community = community.read(cx);
            (community.name(), community.partition())
        };
        let preferences = Preferences::load(&store, &partition);
        let save: Save = {
            let (store, partition) = (store.clone(), partition.clone());
            Rc::new(move |next: Preferences, window: &mut Window| {
                next.save(&store, &partition);
                window.refresh();
            })
        };
        let toggle = |id: &'static str,
                      title: &'static str,
                      description: Option<&'static str>,
                      checked: bool,
                      enabled: bool,
                      apply: fn(&mut Preferences, bool),
                      cx: &App| {
            let save = save.clone();
            let current = preferences.clone();
            preference_row(
                None,
                title,
                description.map(IntoElement::into_any_element),
                vec![
                    Switch::new(id, checked)
                        .disabled(!enabled)
                        .on_change(move |value, window, _| {
                            let mut next = current.clone();
                            apply(&mut next, value);
                            if !next.enabled || !next.sound {
                                sounds::stop_preview();
                            }
                            save(next, window);
                        })
                        .into_any_element(),
                ],
                cx,
            )
            .into_any_element()
        };
        let enabled = preferences.enabled;
        let mut delivery = vec![toggle(
            "notify-desktop",
            "Desktop alerts",
            Some("Show a silent banner through your desktop’s notification service."),
            enabled,
            true,
            |p, v| p.enabled = v,
            cx,
        )];
        if enabled {
            delivery.push(toggle(
                "notify-sound",
                "Sound",
                Some("Choose a sound to preview it."),
                preferences.sound,
                true,
                |p, v| p.sound = v,
                cx,
            ));
            if preferences.sound {
                for category in SOUND_ROWS {
                    delivery.push(
                        self.sound_row(category, &preferences, &store, &partition, window, cx)
                            .into_any_element(),
                    );
                }
            }
        }
        section(vec![
            header(
                "Notifications",
                Some(format!("For {name}. Only the selected community shows alerts.").into()),
                cx,
            )
            .into_any_element(),
            div()
                .child(inline_header("Notify me about", None, Vec::new(), cx))
                .child(rows_group(
                    vec![
                        toggle(
                            "notify-mentions",
                            "Mentions",
                            None,
                            preferences.mention,
                            enabled,
                            |p, v| p.mention = v,
                            cx,
                        ),
                        toggle(
                            "notify-direct",
                            "Direct messages",
                            None,
                            preferences.direct,
                            enabled,
                            |p, v| p.direct = v,
                            cx,
                        ),
                        toggle(
                            "notify-thread",
                            "Thread replies",
                            None,
                            preferences.thread,
                            enabled,
                            |p, v| p.thread = v,
                            cx,
                        ),
                    ],
                    cx,
                ))
                .into_any_element(),
            div()
                .child(inline_header("Delivery", None, Vec::new(), cx))
                .child(rows_group(delivery, cx))
                .into_any_element(),
        ])
    }

    /// `AlertSoundRow`: the category and a compact select of sounds.
    /// Choosing one saves it and previews it.
    fn sound_row(
        &self,
        category: Category,
        preferences: &Preferences,
        store: &Store,
        partition: &Partition,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let allowed = match category {
            Category::Mention => preferences.mention,
            Category::Direct => preferences.direct,
            Category::Thread => preferences.thread,
        };
        let current = preferences.sounds.get(category);
        let open = self.sound_menu == Some(category) && allowed;
        let id = |name: &str| SharedString::from(format!("sound-{name}-{category:?}"));
        let trigger = Button::new(
            id("select"),
            if current == Sound::Silent {
                "Silent"
            } else {
                current.name()
            },
        )
        .outline()
        .size(ButtonSize::Sm)
        .shape(ButtonShape::Control)
        .icon_end(IconName::CaretDown)
        .selected(open)
        .disabled(!allowed)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.sound_menu = (this.sound_menu != Some(category)).then_some(category);
            cx.notify();
        }));
        let mut control = div().relative().child(trigger);
        if open {
            let items = Sound::choices(category).into_iter().map(|sound| {
                let (store, partition) = (store.clone(), partition.clone());
                let base = preferences.clone();
                MenuItem::new(id(sound.name()), option_label(sound, category))
                    .checked(sound == current)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.sound_menu = None;
                        if sound != base.sounds.get(category) {
                            let mut next = base.clone();
                            next.sounds.set(category, sound);
                            next.save(&store, &partition);
                            if sound == Sound::Silent {
                                sounds::stop_preview();
                            } else {
                                sounds::preview(sound);
                            }
                            window.refresh();
                        }
                        cx.notify();
                    }))
            });
            let page = cx.entity().downgrade();
            control = control.child(overlay::dropdown(
                window,
                Align::End,
                Menu::new().child(
                    div()
                        .id(id("menu"))
                        .max_h(rems(18.))
                        .overflow_y_scroll()
                        .children(items),
                ),
                Rc::new(move |_, cx| {
                    let _ = page.update(cx, |this, cx| {
                        this.sound_menu = None;
                        cx.notify();
                    });
                }),
            ));
        }
        preference_row(
            None,
            sound_label(category),
            None,
            vec![control.into_any_element()],
            cx,
        )
        .when(!allowed, |this| this.text_color(c.text_unavailable))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn options_mark_the_recommended_sound() {
        assert_eq!(option_label(Sound::Silent, Category::Direct), "Silent");
        assert_eq!(option_label(Sound::Unison, Category::Direct), "unison rec.");
        assert_eq!(option_label(Sound::Unison, Category::Mention), "unison");
    }

    #[test]
    fn saved_preferences_without_sounds_get_the_defaults() {
        let old = r#"{"enabled":true,"mention":false,"direct":true,"thread":true}"#;
        let preferences: Preferences = serde_json::from_str(old).unwrap();
        assert!(!preferences.mention && preferences.sound);
        assert_eq!(preferences.sounds.get(Category::Thread), Sound::Flutter);
        assert_eq!(
            preferences.sound_for(Category::Direct),
            Some(Sound::Flutter)
        );
        assert_eq!(preferences.sound_for(Category::Mention), None);
        let silent = Preferences {
            sound: false,
            ..preferences.clone()
        };
        assert_eq!(silent.sound_for(Category::Direct), None);
        let mut chosen = preferences;
        chosen.sounds.set(Category::Direct, Sound::Silent);
        assert_eq!(chosen.sound_for(Category::Direct), None);
        let json = serde_json::to_string(&chosen).unwrap();
        assert!(
            json.contains(r#""sounds":{"mention":"flutter","direct":"silent","thread":"flutter"}"#)
        );
    }
}
