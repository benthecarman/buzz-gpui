//! The Reminders page (`RemindersPage.tsx`): Overdue, Today and Upcoming,
//! then a collapsed Done list, with Open, Snooze, Done and Cancel.

use super::model::{TIME_PRESETS, end_of_today, format_due, group_reminders};
use super::{Hub, hub};
use crate::ui::navigation::{Place, Request, Target, with_workspace};
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_relay_client::{EventId, Reminder, ReminderStatus, RemindersStatus, SessionStatus};
use buzz_ui::{
    ActiveTheme, Button, Icon, IconName, Menu, MenuItem, PanelHeader, TypeStyled, tokens::space,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

const UPDATE_FAILED: &str = "The reminder could not be updated. Try again.";

pub struct RemindersPage {
    hub: Option<Entity<Hub>>,
    error: Option<SharedString>,
    /// A Retry read is pending.
    retrying: bool,
    /// The reminder whose Snooze menu is open.
    snooze: Option<String>,
    done_open: bool,
    _hub: Option<Subscription>,
}

impl RemindersPage {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let hub = hub(cx);
        let _hub = hub.as_ref().map(|hub| {
            cx.observe(hub, |this, hub, cx| {
                // A finished read ends Retry's wait.
                if this.retrying && hub.read(cx).state().status != RemindersStatus::Loading {
                    this.retrying = false;
                }
                cx.notify();
            })
        });
        Self {
            hub,
            error: None,
            retrying: false,
            snooze: None,
            done_open: false,
            _hub,
        }
    }

    fn run(&mut self, id: String, edit: Edit, cx: &mut Context<Self>) {
        let Some(community) = self
            .hub
            .as_ref()
            .and_then(|hub| hub.read(cx).community().cloned())
        else {
            return;
        };
        let session = community.read(cx).session.clone();
        self.error = None;
        self.snooze = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = match edit {
                Edit::Snooze(at) => session.snooze_reminder(&id, at).await,
                Edit::Complete => session.complete_reminder(&id).await,
                Edit::Cancel => session.cancel_reminder(&id).await,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "could not update a reminder");
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(UPDATE_FAILED.into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        let Some(community) = self
            .hub
            .as_ref()
            .and_then(|hub| hub.read(cx).community().cloned())
        else {
            return;
        };
        self.retrying = true;
        community.read(cx).session.refresh_reminders();
        cx.notify();
    }

    fn open(&self, reminder: &Reminder, cx: &mut Context<Self>) {
        let Some(target) = reminder.navigable_target() else {
            return;
        };
        let Some(community) = self
            .hub
            .as_ref()
            .and_then(|hub| hub.read(cx).community().cloned())
        else {
            return;
        };
        let target = Target {
            community: Some(community.read(cx).origin.to_string()),
            place: Place::Conversation {
                channel: target.channel_id.clone(),
                message: EventId::from_hex(&target.event_id).ok(),
                thread: None,
                members: false,
            },
        };
        with_workspace(cx, move |workspace, window, cx| {
            workspace.handle(Request::Open(target), window, cx)
        });
    }

    fn row(&self, reminder: &Reminder, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let id = reminder.id.clone();
        let text = reminder
            .target
            .as_ref()
            .map(|target| target.preview.clone())
            .filter(|preview| !preview.is_empty())
            .or_else(|| reminder.note.clone())
            .unwrap_or_default();
        let note = reminder.target.as_ref().and(reminder.note.clone());
        let mut actions: Vec<AnyElement> = Vec::new();
        if reminder.navigable_target().is_some() {
            let reminder = reminder.clone();
            actions.push(
                Button::new(SharedString::from(format!("open-{id}")), "Open")
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&reminder, cx)))
                    .into_any_element(),
            );
        }
        if reminder.status == ReminderStatus::Pending {
            let open = self.snooze.as_deref() == Some(id.as_str());
            let menu_id = id.clone();
            let page = cx.entity().downgrade();
            let dismiss: Dismiss = Rc::new(move |_, cx| {
                let _ = page.update(cx, |this, cx| {
                    this.snooze = None;
                    cx.notify();
                });
            });
            let menu =
                Menu::new().children(TIME_PRESETS.iter().enumerate().map(|(index, preset)| {
                    let (id, at) = (id.clone(), preset.at);
                    MenuItem::new(("snooze-preset", index), preset.label).on_click(cx.listener(
                        move |this, _, _, cx| this.run(id.clone(), Edit::Snooze(at()), cx),
                    ))
                }));
            actions.push(
                div()
                    .relative()
                    .child(
                        Button::new(SharedString::from(format!("snooze-{id}")), "Snooze")
                            .selected(open)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.snooze = Some(menu_id.clone());
                                cx.notify();
                            })),
                    )
                    .when(open, |this| {
                        this.child(overlay::dropdown(window, Align::Start, menu, dismiss))
                    })
                    .into_any_element(),
            );
            let done = id.clone();
            actions.push(
                Button::new(SharedString::from(format!("done-{id}")), "Done")
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.run(done.clone(), Edit::Complete, cx)
                        }),
                    )
                    .into_any_element(),
            );
            let cancel = id.clone();
            actions.push(
                Button::new(SharedString::from(format!("cancel-{id}")), "Cancel")
                    .ghost()
                    .on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.run(cancel.clone(), Edit::Cancel, cx)
                        }),
                    )
                    .into_any_element(),
            );
        }
        // `.row`: its border names an undefined token in buzz-app, so
        // none is drawn.
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(space::S3)
            .py(space::S2)
            .px(space::S3)
            .child(
                div()
                    .flex_1()
                    .min_w(rems(16.))
                    .flex()
                    .flex_col()
                    .child(div().text_body().child(text))
                    .children(
                        note.map(|note| div().text_body().text_color(c.text_subtle).child(note)),
                    )
                    .children(reminder.not_before.map(|at| {
                        div()
                            .text_label()
                            .text_color(c.text_subtle)
                            .child(format_due(at))
                    })),
            )
            .child(div().flex().flex_wrap().gap(space::S2).children(actions))
            .into_any_element()
    }

    fn list(&self, items: &[Reminder], window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(space::S2)
            .mt(space::S2)
            .mb(space::S4)
            .children(items.iter().map(|reminder| self.row(reminder, window, cx)))
    }
}

#[derive(Clone, Copy)]
enum Edit {
    Snooze(u64),
    Complete,
    Cancel,
}

fn notice(child: impl IntoElement) -> Div {
    div().px(space::PANEL_INSET).child(child)
}

impl Render for RemindersPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let (community, state, now) = match &self.hub {
            Some(hub) => {
                let hub = hub.read(cx);
                (hub.community().cloned(), hub.state().clone(), hub.now())
            }
            None => (None, Arc::default(), 0),
        };
        let session_status = community
            .as_ref()
            .map(|community| community.read(cx).snapshot.status.clone());
        let ready = session_status == Some(SessionStatus::Ready);
        let status: Option<String> = match &session_status {
            Some(SessionStatus::Ready) => (state.status == RemindersStatus::Loading
                && !state.hydrated)
                .then(|| "Loading reminders…".into()),
            Some(SessionStatus::Failed(error)) => Some(error.clone()),
            _ => Some("Connect to a community to see your reminders.".into()),
        };
        // Any failed read keeps Retry up beside the list until one succeeds.
        let failed = ready && state.error.is_some() && state.status != RemindersStatus::Loading;
        let show_retry = ready && (failed || self.retrying);
        let groups = group_reminders(&state.reminders, now, end_of_today());
        let (done, active): (Vec<_>, Vec<_>) =
            groups.into_iter().partition(|group| group.label == "Done");
        let done = done.into_iter().next();

        let mut body: Vec<AnyElement> = Vec::new();
        if let Some(error) = self.error.clone() {
            body.push(notice(div().text_body().child(error)).into_any_element());
        }
        if show_retry {
            body.push(
                notice(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(space::S2)
                        .child(
                            div()
                                .text_body()
                                .child("Some reminders could not be loaded."),
                        )
                        .child(
                            Button::new("retry-reminders", "Retry reminders")
                                .loading(self.retrying)
                                .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                        ),
                )
                .into_any_element(),
            );
        }
        if let Some(status) = status {
            body.push(notice(div().text_body().child(status)).into_any_element());
        } else if !(failed && !state.hydrated && state.reminders.is_empty()) {
            let section = |label: &'static str| {
                div()
                    .px(space::PANEL_INSET)
                    .child(div().text_label().text_color(c.text_subtle).child(label))
            };
            if active.is_empty() {
                body.push(
                    section("Active")
                        .child(
                            div()
                                .text_body()
                                .text_color(c.text_subtle)
                                .child("No active reminders"),
                        )
                        .when(done.is_none(), |this| {
                            this.child(
                                div().text_body().text_color(c.text_subtle).child(
                                    "Use Remind me in a message's menu to get reminded later.",
                                ),
                            )
                        })
                        .into_any_element(),
                );
            } else {
                for group in &active {
                    body.push(
                        section(group.label)
                            .child(self.list(&group.reminders, window, cx))
                            .into_any_element(),
                    );
                }
            }
            if let Some(done) = done {
                let open = self.done_open;
                body.push(
                    div()
                        .px(space::PANEL_INSET)
                        .child(
                            div()
                                .id("reminders-done")
                                .flex()
                                .items_center()
                                .gap(space::S1)
                                .cursor_pointer()
                                .text_label()
                                .text_color(c.text_subtle)
                                .child(
                                    Icon::new(if open {
                                        IconName::CaretDown
                                    } else {
                                        IconName::CaretRight
                                    })
                                    .size(12.),
                                )
                                .child(format!("Done · {}", done.reminders.len()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.done_open = !this.done_open;
                                    cx.notify();
                                })),
                        )
                        .when(open, |this| {
                            this.child(self.list(&done.reminders, window, cx))
                        })
                        .into_any_element(),
                );
            }
        }
        div()
            .id("reminders-page")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .text_color(c.text_standard)
            .child(
                PanelHeader::new()
                    .joined(true)
                    .label(Some(IconName::Alarm), "Reminders"),
            )
            .children(body)
    }
}
