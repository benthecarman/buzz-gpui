//! The "Set a status" dialog (`StatusEditor.tsx`, relay.md §5j): text, an
//! emoji, and how long the status lasts.

use crate::community::Community;
use crate::ui::emoji_picker::{self, EmojiPicker, Picked};
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_relay_client::{Session, StatusInput, UserStatus};
use buzz_ui::{
    ActiveTheme, Button, Dialog, IconButton, IconButtonSize, IconName, Menu, MenuItem, Select,
    Surface, TextField, TypeStyled, tokens::radius,
};
use chrono::{DateTime, Datelike, Duration as Days, Local, NaiveTime, TimeZone, Timelike};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

/// `status-duration.ts` presets. The preset is not saved; editing infers
/// it from the saved expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duration {
    Hour,
    EightHours,
    Today,
    Week,
    Custom,
    Never,
}

const DURATIONS: [Duration; 6] = [
    Duration::Hour,
    Duration::EightHours,
    Duration::Today,
    Duration::Week,
    Duration::Custom,
    Duration::Never,
];

impl Duration {
    fn label(self) -> &'static str {
        match self {
            Self::Hour => "1 hour",
            Self::EightHours => "8 hours",
            Self::Today => "Today",
            Self::Week => "This week",
            Self::Custom => "Custom",
            Self::Never => "Don’t clear",
        }
    }
}

/// When a preset ends, counted from `now`: local midnight tonight, or the
/// next local Monday at midnight for "This week".
pub fn deadline(duration: Duration, now: DateTime<Local>) -> Option<i64> {
    let midnight = |days: i64| {
        let date = now.date_naive() + Days::days(days);
        Local
            .from_local_datetime(&date.and_time(NaiveTime::MIN))
            .earliest()
            .map(|at| at.timestamp())
    };
    match duration {
        Duration::Hour => Some(now.timestamp() + 3600),
        Duration::EightHours => Some(now.timestamp() + 28_800),
        Duration::Today => midnight(1),
        Duration::Week => {
            // Days until next Monday; a full week on Mondays.
            let weekday = i64::from(now.weekday().num_days_from_sunday());
            let days = match (8 - weekday) % 7 {
                0 => 7,
                days => days,
            };
            midnight(days)
        }
        Duration::Custom | Duration::Never => None,
    }
}

/// The preset whose deadline, counted from `updated_at`, lands within two
/// minutes of `expires_at`; otherwise Custom.
pub fn infer(expires_at: Option<u64>, updated_at: u64) -> Duration {
    let Some(expires_at) = expires_at else {
        return Duration::Never;
    };
    let Some(from) = Local.timestamp_opt(updated_at as i64, 0).single() else {
        return Duration::Custom;
    };
    DURATIONS
        .into_iter()
        .filter_map(|duration| {
            deadline(duration, from).map(|at| (duration, (at - expires_at as i64).abs()))
        })
        .filter(|(_, distance)| *distance < 121)
        .min_by_key(|(_, distance)| *distance)
        .map_or(Duration::Custom, |(duration, _)| duration)
}

const QUICK: [(&str, &str); 5] = [
    ("🗣️", "In a meeting"),
    ("🚌", "Commuting"),
    ("🤒", "Out sick"),
    ("🏖️", "Vacationing"),
    ("🏠", "Working remotely"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Emoji,
    Duration,
    Date,
    Time,
}

/// The dialog finished: `Some` with what was saved (`None` inside when
/// cleared), or `None` when cancelled.
pub struct Closed(pub Option<Option<UserStatus>>);

pub struct StatusEditor {
    session: Arc<Session>,
    /// The composer's emoji picker, custom emoji included.
    emoji_picker: Entity<EmojiPicker>,
    current: Option<UserStatus>,
    text: Entity<InputState>,
    emoji: String,
    duration: Duration,
    duration_changed: bool,
    custom: DateTime<Local>,
    picker: Option<Picker>,
    busy: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Closed> for StatusEditor {}

impl StatusEditor {
    pub fn new(
        community: Entity<Community>,
        current: Option<UserStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("What’s your status?");
            if let Some(current) = &current {
                state = state.default_value(current.text.clone());
            }
            state
        });
        text.update(cx, |state, cx| state.focus(window, cx));
        let session = community.read(cx).session.clone();
        let emoji_picker = cx.new(|cx| EmojiPicker::new(community, window, cx));
        let _subscriptions = vec![
            cx.subscribe(&text, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.submit(false, cx),
                _ => {}
            }),
            cx.subscribe(&emoji_picker, |this, _, Picked(pick), cx| {
                this.emoji = pick.text();
                this.set_picker(None, cx);
            }),
            cx.subscribe(&emoji_picker, |this, _, _: &emoji_picker::Dismissed, cx| {
                this.set_picker(None, cx)
            }),
        ];
        let now = Local::now();
        let duration = current.as_ref().map_or(Duration::Today, |status| {
            infer(status.expires_at, status.updated_at)
        });
        let custom = current
            .as_ref()
            .and_then(|status| status.expires_at)
            .and_then(|at| Local.timestamp_opt(at as i64, 0).single())
            .or_else(|| {
                deadline(Duration::Today, now).and_then(|at| Local.timestamp_opt(at, 0).single())
            })
            .unwrap_or(now);
        Self {
            session,
            emoji_picker,
            emoji: current
                .as_ref()
                .map(|status| status.emoji.clone())
                .unwrap_or_default(),
            current,
            text,
            duration,
            duration_changed: false,
            custom,
            picker: None,
            busy: false,
            error: None,
            _subscriptions,
        }
    }

    fn text(&self, cx: &App) -> String {
        self.text.read(cx).value().trim().to_string()
    }

    /// The chosen emoji, or 💬 for a text-only status.
    fn effective_emoji(&self, cx: &App) -> String {
        if !self.emoji.is_empty() {
            self.emoji.clone()
        } else if !self.text(cx).is_empty() {
            "💬".into()
        } else {
            String::new()
        }
    }

    fn set_picker(&mut self, picker: Option<Picker>, cx: &mut Context<Self>) {
        self.picker = picker;
        cx.notify();
    }

    fn set_duration(&mut self, duration: Duration, cx: &mut Context<Self>) {
        self.duration = duration;
        self.duration_changed = true;
        self.picker = None;
        self.error = None;
        cx.notify();
    }

    fn set_custom(&mut self, at: DateTime<Local>, cx: &mut Context<Self>) {
        self.custom = at;
        self.duration_changed = true;
        self.picker = None;
        self.error = None;
        cx.notify();
    }

    fn submit(&mut self, clear: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let text = self.text(cx);
        let emoji = self.effective_emoji(cx);
        if !clear && text.is_empty() && self.emoji.is_empty() {
            return;
        }
        let now = Local::now().timestamp();
        let current_expiry = self.current.as_ref().and_then(|status| {
            let live = status.expires_at.is_none_or(|at| at as i64 > now);
            (!self.duration_changed && live).then_some(status.expires_at)
        });
        let expires_at = if clear || self.duration == Duration::Never {
            None
        } else if let Some(expiry) = current_expiry {
            expiry.map(|at| at as i64)
        } else if self.duration == Duration::Custom {
            Some(self.custom.timestamp())
        } else {
            deadline(self.duration, Local::now())
        };
        if !clear && expires_at.is_some_and(|at| at <= now) {
            self.error = Some("Choose a date and time in the future.".into());
            cx.notify();
            return;
        }
        let input = if clear {
            StatusInput::default()
        } else {
            StatusInput {
                text,
                emoji,
                expires_at: expires_at.map(|at| at as u64),
            }
        };
        self.busy = true;
        self.error = None;
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
            let result = session.set_status(input.clone()).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        let saved =
                            (!input.text.is_empty() || !input.emoji.is_empty()).then(|| {
                                UserStatus {
                                    text: input.text,
                                    emoji: input.emoji,
                                    updated_at: Local::now().timestamp() as u64,
                                    expires_at: input.expires_at,
                                }
                            });
                        cx.emit(Closed(Some(saved)));
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dismiss(cx: &mut Context<Self>) -> Dismiss {
        let editor = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = editor.update(cx, |this, cx| this.set_picker(None, cx));
        })
    }

    fn label(text: &'static str, cx: &App) -> Div {
        div()
            .text_label_sm()
            .text_color(cx.colors().text_subtle)
            .child(text)
    }

    /// A `FieldButton` that shows `menu` below it while `picker` is open.
    fn field_button(
        &self,
        id: &'static str,
        value: String,
        picker: Picker,
        menu: fn(&Self, &mut Context<Self>) -> AnyElement,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.picker == Some(picker);
        let mut wrapper = div().relative().w_full().child(
            Select::new(id)
                .value(value)
                .open(open)
                .surface(Surface::Floating)
                .disabled(self.busy)
                .on_click(cx.listener(move |this, _, _, cx| this.set_picker(Some(picker), cx))),
        );
        if open {
            wrapper = wrapper.child(overlay::dropdown(
                window,
                Align::End,
                menu(self, cx),
                Self::dismiss(cx),
            ));
        }
        wrapper
    }

    fn duration_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let keep_never = self
            .current
            .as_ref()
            .is_some_and(|status| status.expires_at.is_none());
        Menu::new()
            .children(
                DURATIONS
                    .into_iter()
                    .filter(|duration| *duration != Duration::Never || keep_never)
                    .map(|duration| {
                        MenuItem::new(SharedString::from(duration.label()), duration.label())
                            .checked(self.duration == duration)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_duration(duration, cx)),
                            )
                    }),
            )
            .into_any_element()
    }

    fn date_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let today = Local::now().date_naive();
        let chosen = self.custom.date_naive();
        let time = self.custom.time();
        let items = (0..31).map(|offset| {
            let date = today + Days::days(offset);
            let at = Local.from_local_datetime(&date.and_time(time)).earliest();
            MenuItem::new(
                SharedString::from(format!("date-{offset}")),
                date.format("%b %-d, %Y").to_string(),
            )
            .checked(date == chosen)
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(at) = at {
                    this.set_custom(at, cx);
                }
            }))
        });
        Menu::new()
            .child(
                div()
                    .id("status-dates")
                    .max_h(rems(18.))
                    .overflow_y_scroll()
                    .children(items),
            )
            .into_any_element()
    }

    fn time_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let date = self.custom.date_naive();
        let chosen = (self.custom.hour(), self.custom.minute());
        let items = (0..48).map(|slot| {
            let (hour, minute) = (slot / 2, (slot % 2) * 30);
            let at = NaiveTime::from_hms_opt(hour, minute, 0)
                .and_then(|time| Local.from_local_datetime(&date.and_time(time)).earliest());
            MenuItem::new(
                SharedString::from(format!("time-{slot}")),
                time_label(hour, minute),
            )
            .checked(chosen == (hour, minute))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(at) = at {
                    this.set_custom(at, cx);
                }
            }))
        });
        Menu::new()
            .child(
                div()
                    .id("status-times")
                    .max_h(rems(18.))
                    .overflow_y_scroll()
                    .children(items),
            )
            .into_any_element()
    }
}

fn time_label(hour: u32, minute: u32) -> String {
    let twelve = match hour % 12 {
        0 => 12,
        hour => hour,
    };
    let half = if hour < 12 { "AM" } else { "PM" };
    format!("{twelve}:{minute:02} {half}")
}

impl Render for StatusEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let effective = self.effective_emoji(cx);
        let emoji_open = self.picker == Some(Picker::Emoji);
        let emoji_button = div()
            .relative()
            .child(
                IconButton::new("status-emoji", IconName::Smiley)
                    .size(IconButtonSize::Sm)
                    .surface(Surface::Floating)
                    .selected(emoji_open)
                    .disabled(self.busy)
                    .icon_size(22.)
                    .tooltip("Choose a status emoji")
                    .when(!effective.is_empty(), |this| {
                        this.child(div().text_size(rems(1.25)).child(effective.clone()))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if !emoji_open {
                            this.emoji_picker.update(cx, |picker, cx| {
                                picker.reset(window, cx);
                                picker.focus(window, cx);
                            });
                        }
                        this.set_picker((!emoji_open).then_some(Picker::Emoji), cx)
                    })),
            )
            .when(emoji_open, |this| {
                this.child(overlay::dropdown(
                    window,
                    Align::Start,
                    emoji_picker::emoji_picker(&self.emoji_picker, {
                        let dismiss = Self::dismiss(cx);
                        move |_, window, cx| dismiss(window, cx)
                    }),
                    Self::dismiss(cx),
                ))
            });
        let message = TextField::new(&self.text)
            .surface(Surface::Floating)
            .disabled(self.busy)
            .leading(emoji_button);

        let duration_label = if self.duration == Duration::Custom {
            "Custom date and time".to_string()
        } else {
            self.duration.label().to_string()
        };
        let mut duration = div()
            .flex()
            .flex_col()
            .gap(rems(0.5))
            .child(Self::label("Duration", cx))
            .child(self.field_button(
                "status-duration",
                duration_label,
                Picker::Duration,
                Self::duration_menu,
                window,
                cx,
            ));
        if self.duration == Duration::Custom {
            let date = self.custom.format("%b %-d, %Y").to_string();
            let time = time_label(self.custom.hour(), self.custom.minute());
            duration = duration.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .child(Self::label("Until", cx))
                    .child(
                        div()
                            .flex()
                            .gap(rems(0.5))
                            .child(div().flex_1().min_w_0().child(self.field_button(
                                "status-date",
                                date,
                                Picker::Date,
                                Self::date_menu,
                                window,
                                cx,
                            )))
                            .child(div().w(rems(10.)).child(self.field_button(
                                "status-time",
                                time,
                                Picker::Time,
                                Self::time_menu,
                                window,
                                cx,
                            ))),
                    ),
            );
        }

        let quick = self.current.is_none().then(|| {
            div()
                .flex()
                .flex_col()
                .gap(rems(0.5))
                .child(Self::label("Quick statuses", cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .py(rems(0.5))
                        .border_1()
                        .border_color(c.border_standard)
                        .rounded(radius::CONTROL)
                        .overflow_hidden()
                        .children(QUICK.into_iter().map(|(emoji, label)| {
                            div()
                                .id(SharedString::from(format!("quick-{label}")))
                                .flex()
                                .items_center()
                                .gap(rems(0.75))
                                .min_h(rems(2.5))
                                .px(rems(1.5))
                                .text_label_sm()
                                .cursor_pointer()
                                .hover(move |s| s.bg(c.affordance_floating_hover))
                                .child(div().text_body_lg().child(emoji))
                                .child(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.emoji = emoji.to_string();
                                    this.text.update(cx, |state, cx| {
                                        state.set_value(label, window, cx);
                                        state.focus(window, cx);
                                    });
                                    cx.notify();
                                }))
                        })),
                )
        });

        let can_save = !self.text(cx).is_empty() || !self.emoji.is_empty();
        let mut dialog =
            Dialog::new("status-editor", "Set a status")
                .on_close(cx.listener(|this, _, _, cx| {
                    if !this.busy {
                        cx.emit(Closed(None));
                    }
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(rems(1.25))
                        .child(message)
                        .child(duration)
                        .children(quick)
                        .children(self.error.clone().map(|error| {
                            div().text_body_sm().text_color(c.text_danger).child(error)
                        })),
                )
                .footer(
                    Button::new("status-save", "Save status")
                        .prominent()
                        .loading(self.busy)
                        .disabled(self.busy || !can_save)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx))),
                );
        if self.current.is_some() {
            // A ghost button with danger text.
            let busy = self.busy;
            let hover = c.interaction(Surface::Floating).hover;
            dialog = dialog.footer_leading(
                div()
                    .id("status-clear")
                    .flex()
                    .items_center()
                    .h(rems(2.5))
                    .px(rems(1.5))
                    .rounded(radius::CAPSULE)
                    .text_label_sm()
                    .text_color(if busy {
                        c.text_unavailable
                    } else {
                        c.text_danger
                    })
                    .when(!busy, |this| {
                        this.cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(|this, _, _, cx| this.submit(true, cx)))
                    })
                    .child("Clear status"),
            );
        }
        dialog
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn deadlines_follow_the_reference() {
        // Friday 2026-10-09 15:00.
        let now = at(2026, 10, 9, 15, 0);
        assert_eq!(deadline(Duration::Hour, now), Some(now.timestamp() + 3600));
        assert_eq!(
            deadline(Duration::Today, now),
            Some(at(2026, 10, 10, 0, 0).timestamp())
        );
        assert_eq!(
            deadline(Duration::Week, now),
            Some(at(2026, 10, 12, 0, 0).timestamp())
        );
        // On a Monday, "This week" runs to the next Monday.
        let monday = at(2026, 10, 12, 9, 0);
        assert_eq!(
            deadline(Duration::Week, monday),
            Some(at(2026, 10, 19, 0, 0).timestamp())
        );
        // On a Sunday, it ends at midnight.
        let sunday = at(2026, 10, 11, 9, 0);
        assert_eq!(
            deadline(Duration::Week, sunday),
            Some(at(2026, 10, 12, 0, 0).timestamp())
        );
        assert_eq!(deadline(Duration::Never, now), None);
    }

    #[test]
    fn infers_the_preset_from_the_expiry() {
        let updated = at(2026, 10, 9, 10, 0);
        let t = updated.timestamp() as u64;
        assert_eq!(infer(None, t), Duration::Never);
        assert_eq!(infer(Some(t + 3600), t), Duration::Hour);
        assert_eq!(infer(Some(t + 3600 + 100), t), Duration::Hour);
        assert_eq!(
            infer(Some(at(2026, 10, 10, 0, 0).timestamp() as u64), t),
            Duration::Today
        );
        assert_eq!(infer(Some(t + 5000), t), Duration::Custom);
        assert_eq!(time_label(0, 30), "12:30 AM");
        assert_eq!(time_label(13, 0), "1:00 PM");
    }
}
