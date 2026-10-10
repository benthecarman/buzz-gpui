//! "Remind me" for one message (`RemindDialog.tsx`): a preset or custom
//! time, plus an optional note.

use super::model::{
    TIME_PRESETS, format_due, parse_custom_date_time, shifted_from, today_date_string,
};
use crate::community::Community;
use crate::ui::modal::Dismissed;
use buzz_relay_client::ReminderTarget;
use buzz_relay_client::fold::MessageRow;
use buzz_ui::{ActiveTheme, Button, Dialog, Surface, TextArea, TextField, TypeStyled};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::*;

const NOTE_LIMIT: usize = 4096;

pub struct RemindDialog {
    community: Entity<Community>,
    target: ReminderTarget,
    note: Entity<TextareaState>,
    date: Entity<InputState>,
    time: Entity<InputState>,
    pending: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for RemindDialog {}

impl RemindDialog {
    pub fn new(
        community: Entity<Community>,
        row: &MessageRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let note = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Add a note (optional)")
                .auto_grow(2, 6)
        });
        let date = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("YYYY-MM-DD")
                .default_value(today_date_string())
        });
        let time = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("HH:MM")
                .default_value("09:00")
        });
        let _subscriptions = vec![
            cx.subscribe(&date, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&time, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&note, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        Self {
            community,
            target: ReminderTarget {
                event_id: row.id.to_hex(),
                channel_id: row.channel.to_string(),
                preview: preview(&row.content),
                author_pubkey: row.author.to_hex(),
            },
            note,
            date,
            time,
            pending: false,
            error: None,
            _subscriptions,
        }
    }

    fn custom(&self, cx: &App) -> Option<u64> {
        parse_custom_date_time(&self.date.read(cx).value(), &self.time.read(cx).value())
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.pending {
            cx.emit(Dismissed);
        }
    }

    fn submit(&mut self, not_before: u64, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.pending = true;
        self.error = None;
        let session = self.community.read(cx).session.clone();
        let target = self.target.clone();
        let note: String = self
            .note
            .read(cx)
            .value()
            .chars()
            .take(NOTE_LIMIT)
            .collect();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = session
                .create_reminder(target, not_before, Some(note))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pending = false;
                match saved {
                    Ok(()) => cx.emit(Dismissed),
                    Err(error) => {
                        tracing::warn!(%error, "could not save a reminder");
                        this.error = Some("The reminder could not be saved. Try again.".into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// `messagePreview`: the message as plain text, or "New message".
fn preview(content: &str) -> String {
    let text = crate::notifications::plain_text(content);
    if text.is_empty() {
        "New message".into()
    } else {
        text
    }
}

impl Render for RemindDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let pending = self.pending;
        let custom = self.custom(cx);
        let time = self.time.read(cx).value().to_string();
        let presets = TIME_PRESETS.iter().enumerate().map(|(index, preset)| {
            let at = preset.at;
            Button::new(("remind-preset", index), preset.label)
                .surface(Surface::Floating)
                .full_width()
                .disabled(pending)
                .on_click(cx.listener(move |this, _, _, cx| this.submit(at(), cx)))
                .into_any_element()
        });
        let shifted = custom.filter(|at| shifted_from(&time, *at)).map(|at| {
            div().text_body().text_color(c.text_subtle).child(format!(
                "That time is skipped by daylight saving. The reminder will be set for {}.",
                format_due(at)
            ))
        });
        Dialog::new("remind-me", "Remind me")
            .description("Choose when to be reminded about this message.")
            .on_close(cx.listener(|this, _, _, cx| this.close(cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.5))
                    .children(presets)
                    .child(
                        div()
                            .flex()
                            .gap(rems(0.5))
                            .child(
                                div().flex_1().child(
                                    TextField::new(&self.date)
                                        .surface(Surface::Floating)
                                        .disabled(pending),
                                ),
                            )
                            .child(
                                div().flex_1().child(
                                    TextField::new(&self.time)
                                        .surface(Surface::Floating)
                                        .disabled(pending),
                                ),
                            ),
                    )
                    .children(shifted)
                    .child(
                        TextArea::new(&self.note)
                            .rows(2)
                            .surface(Surface::Floating)
                            .disabled(pending),
                    )
                    .children(
                        self.error
                            .clone()
                            .map(|error| div().text_body().text_color(c.text_subtle).child(error)),
                    ),
            )
            .footer(
                Button::new("remind-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .disabled(pending)
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            )
            .footer(
                Button::new("remind-set", "Set reminder")
                    .prominent()
                    .disabled(pending || custom.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(at) = custom {
                            this.submit(at, cx);
                        }
                    })),
            )
    }
}
