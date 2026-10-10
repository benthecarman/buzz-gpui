//! "New message" (`NewMessage.tsx`, `RecipientPicker.tsx`): pick one to
//! eight people, then either open the DM (or group DM) with exactly them,
//! or write the first message here and send it there. The relay reuses the
//! conversation for the same people and unhides it.

use super::message::display_name;
use super::modal::{Dismissed, Leading, OpenConversation, error, group_label, note, row};
use crate::community::Community;
use buzz_relay_client::members::Person;
use buzz_relay_client::{Delivery, EventId, PeoplePage, kinds};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, Dialog, Icon, IconName, SearchField, Surface,
    TextArea, TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(150);
/// Other people a DM can hold (`direct-message-policy.ts`).
const MAX_RECIPIENTS: usize = 8;

pub struct NewMessageDialog {
    community: Entity<Community>,
    input: Entity<InputState>,
    /// The first message, sent once the conversation is open.
    message: Entity<TextareaState>,
    selected: Vec<Person>,
    found: Option<(String, Result<PeoplePage, String>)>,
    searching: Option<Task<()>>,
    highlighted: usize,
    opening: bool,
    /// The first message is being confirmed ("Sending message…").
    sending: bool,
    error: Option<SharedString>,
    /// A first message sent but not yet confirmed, kept for Retry send.
    pending: Option<Pending>,
    on_open: OpenConversation,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Dismissed> for NewMessageDialog {}

/// A first message waiting for its delivery to be confirmed.
#[derive(Clone)]
struct Pending {
    id: EventId,
    channel: String,
    recipients: Vec<String>,
    draft: String,
}

impl NewMessageDialog {
    pub fn new(
        community: Entity<Community>,
        on_open: OpenConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find a person…"));
        input.update(cx, |state, cx| state.focus(window, cx));
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .auto_grow(2, 6)
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.highlighted = 0;
                    this.search(window, cx);
                }
            }),
            cx.subscribe_in(
                &message,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.open(true, window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
        ];
        let mut dialog = Self {
            community,
            input,
            message,
            selected: Vec::new(),
            found: None,
            searching: None,
            highlighted: 0,
            opening: false,
            sending: false,
            error: None,
            pending: None,
            on_open,
            _subscriptions: subscriptions,
        };
        dialog.search(window, cx);
        dialog
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let session = self.community.read(cx).session.clone();
        self.searching = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let page = session.people(&query, 1).await;
            let _ = this.update(cx, |this, cx| {
                this.found = Some((query, page));
                this.searching = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn candidates(&self, cx: &App) -> Vec<Person> {
        match &self.found {
            Some((query, Ok(page))) if *query == self.query(cx) => page
                .people
                .iter()
                .filter(|p| !self.selected.iter().any(|s| s.pubkey == p.pubkey))
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn select(&mut self, person: Person, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.len() >= MAX_RECIPIENTS {
            return;
        }
        self.selected.push(person);
        let placeholder = if self.selected.len() >= MAX_RECIPIENTS {
            format!("{MAX_RECIPIENTS} people selected")
        } else {
            "Add another person…".to_owned()
        };
        self.input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.set_placeholder(placeholder, window, cx);
            state.focus(window, cx);
        });
        self.highlighted = 0;
        self.update_message_placeholder(window, cx);
        self.search(window, cx);
    }

    fn remove(&mut self, pubkey: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected.retain(|p| p.pubkey != pubkey);
        let placeholder = if self.selected.is_empty() {
            "Find a person…"
        } else {
            "Add another person…"
        };
        self.input.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx);
        });
        self.update_message_placeholder(window, cx);
        cx.notify();
    }

    /// The composer's label (`NewMessage.tsx`): `Message <names>`, or
    /// nothing until someone is picked.
    fn update_message_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = self
            .selected
            .iter()
            .map(|p| p.profile.name.clone())
            .collect();
        let placeholder = if names.is_empty() {
            String::new()
        } else {
            format!("Message {}", names.join(", "))
        };
        self.message.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx);
        });
    }

    fn draft(&self, cx: &App) -> String {
        self.message.read(cx).value().trim().to_string()
    }

    /// Opens the conversation with the people picked; with `send`, posts
    /// the typed message there first and waits until its delivery is
    /// confirmed (`directMessages.delivered`). A send that cannot be
    /// confirmed stays pending for "Retry send"; changing the message or
    /// the people first needs it confirmed or failed.
    fn open(&mut self, send: bool, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        if self.selected.is_empty() || self.opening || (send && draft.is_empty()) {
            return;
        }
        let pubkeys: Vec<String> = self.selected.iter().map(|p| p.pubkey.clone()).collect();
        let session = self.community.read(cx).session.clone();
        let earlier = self.pending.clone().filter(|_| send);
        self.opening = true;
        self.sending = send;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let mut pending = earlier;
                if let Some(earlier) = &pending
                    && (earlier.recipients != pubkeys || earlier.draft != draft)
                {
                    let failed = match session.outbox().await {
                        Some(outbox) => outbox
                            .get(&earlier.id)
                            .is_some_and(|op| op.delivery == Delivery::Failed),
                        None => false,
                    };
                    if !failed {
                        return Err((
                            "Retry the earlier message to confirm its delivery before editing."
                                .to_string(),
                            pending,
                        ));
                    }
                    session
                        .dismiss(earlier.id)
                        .await
                        .map_err(|error| (error, pending.clone()))?;
                    pending = None;
                }
                let pending = match pending {
                    Some(pending) => pending,
                    None => {
                        let id = session
                            .open_dm(pubkeys.clone())
                            .await
                            .map_err(|error| (error, None))?;
                        if !send {
                            return Ok(id);
                        }
                        session
                            .send(&id, &draft, Default::default(), Vec::new(), None)
                            .await
                            .map_err(|error| {
                                (
                                    format!(
                                        "The conversation is open, but the message was not \
                                         sent. {error}"
                                    ),
                                    None,
                                )
                            })?;
                        let sent = queued(&session, &id, &draft).await.ok_or_else(|| {
                            ("The message could not be confirmed.".to_string(), None)
                        })?;
                        Pending {
                            id: sent,
                            channel: id,
                            recipients: pubkeys.clone(),
                            draft: draft.clone(),
                        }
                    }
                };
                session
                    .dm_delivered(pending.id, &pending.channel)
                    .await
                    .map_err(|error| (error, Some(pending.clone())))?;
                Ok(pending.channel)
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.opening = false;
                this.sending = false;
                match result {
                    Ok(id) => {
                        this.pending = None;
                        (this.on_open)(id, window, cx);
                        cx.emit(Dismissed);
                    }
                    Err((error, pending)) => {
                        this.error = Some(error.into());
                        this.pending = pending;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// "Retry send": confirms the pending first message again.
    fn retry_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.clone() else {
            return;
        };
        if self.draft(cx) != pending.draft {
            self.message.update(cx, |state, cx| {
                state.set_value(pending.draft.clone(), window, cx)
            });
        }
        self.open(true, window, cx);
    }

    fn avatar(&self, person: &Person, size: f32, cx: &App) -> Avatar {
        let mut avatar = Avatar::new(person.profile.name.clone()).size(AvatarSize::Px(size));
        if person.profile.is_agent {
            avatar = avatar.squircle();
        }
        if let Some(picture) = &person.profile.picture {
            let media = self.community.read(cx).session.media().clone();
            avatar = avatar.image(super::media::avatar(picture, &media));
        }
        avatar
    }
}

impl Render for NewMessageDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let candidates = self.candidates(cx);
        if !candidates.is_empty() && self.highlighted >= candidates.len() {
            self.highlighted = candidates.len() - 1;
        }
        let profiles = self.community.read(cx).snapshot.profiles.clone();
        let full = self.selected.len() >= MAX_RECIPIENTS;

        let chips = div()
            .flex()
            .flex_wrap()
            .gap(rems(0.5))
            .children(self.selected.iter().map(|person| {
                let key = person.pubkey.clone();
                div()
                    .id(SharedString::from(format!("recipient-{}", person.pubkey)))
                    .flex()
                    .items_center()
                    .gap(rems(0.375))
                    .pl(rems(0.25))
                    .pr(rems(0.5))
                    .py(rems(0.125))
                    .rounded_full()
                    .bg(c.affordance_subtle)
                    .text_body_sm()
                    .cursor_pointer()
                    .hover(|s| s.bg(c.affordance_subtle_hover))
                    .child(self.avatar(person, 20., cx))
                    .child(display_name(&profiles, &person.pubkey))
                    .child(Icon::new(IconName::X).size(12.))
                    .on_click(cx.listener(move |this, _, window, cx| this.remove(&key, window, cx)))
            }));

        let mut list = div().flex().flex_col();
        list = list.child(group_label("People", cx));
        if full {
            list = list.child(note("You can message up to 8 people at once.", cx));
        } else {
            match &self.found {
                Some((query, Err(_))) if *query == self.query(cx) => {
                    list = list.child(note("Could not load people.", cx));
                }
                Some((query, Ok(_))) if *query == self.query(cx) && candidates.is_empty() => {
                    list = list.child(note("No matching people.", cx));
                }
                Some((query, Ok(_))) if *query == self.query(cx) => {}
                _ => list = list.child(note("Loading people…", cx)),
            }
            for (index, person) in candidates.iter().enumerate() {
                let chosen = person.clone();
                let detail = person.profile.is_agent.then(|| SharedString::from("Agent"));
                list = list.child(
                    row(
                        SharedString::from(format!("person-{}", person.pubkey)),
                        Leading::Element(self.avatar(person, 24., cx).into_any_element()),
                        person.profile.name.clone(),
                        detail,
                        index == self.highlighted,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(chosen.clone(), window, cx)
                    })),
                );
            }
        }

        Dialog::new("new-message", "New message")
            .compact_header_gap()
            .on_close(cx.listener(|_, _, _, cx| cx.emit(Dismissed)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(rems(0.75))
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        // The message box keeps its keys; Enter sends it.
                        if this.message.focus_handle(cx).contains_focused(window, cx) {
                            return;
                        }
                        let count = this.candidates(cx).len();
                        match event.keystroke.key.as_str() {
                            "down" if count > 0 => {
                                this.highlighted = (this.highlighted + 1) % count;
                            }
                            "up" if count > 0 => {
                                this.highlighted = (this.highlighted + count - 1) % count;
                            }
                            "enter" => match this.candidates(cx).get(this.highlighted).cloned() {
                                Some(person) if !this.query(cx).is_empty() => {
                                    this.select(person, window, cx)
                                }
                                _ => this.open(false, window, cx),
                            },
                            "backspace" if this.query(cx).is_empty() => {
                                if let Some(last) = this.selected.last().map(|p| p.pubkey.clone()) {
                                    this.remove(&last, window, cx);
                                }
                                return;
                            }
                            "escape" => cx.emit(Dismissed),
                            _ => return,
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(rems(0.5))
                            .text_label_sm()
                            .child("To:")
                            .when(!self.selected.is_empty(), |this| this.child(chips)),
                    )
                    .child(SearchField::new(&self.input).surface(Surface::Floating))
                    .child(
                        div()
                            .id("people")
                            .h(px(240.))
                            .overflow_y_scroll()
                            .child(list),
                    )
                    .child(
                        TextArea::new(&self.message)
                            .surface(Surface::Floating)
                            .disabled(self.selected.is_empty() || self.opening),
                    )
                    .when(self.opening, |this| {
                        this.child(note(
                            if self.sending {
                                "Sending message…"
                            } else {
                                "Opening conversation…"
                            },
                            cx,
                        ))
                    })
                    .children(self.error.clone().map(|message| error(message, cx)))
                    .when(self.pending.is_some() && !self.opening, |this| {
                        this.child(
                            div().child(
                                Button::new("new-message-retry", "Retry send")
                                    .surface(Surface::Floating)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.retry_send(window, cx)
                                    })),
                            ),
                        )
                    }),
            )
            // No Cancel: the dialog closes with its X or Escape, and three
            // buttons do not fit.
            .footer(
                Button::new("new-message-open", "Open conversation")
                    .surface(Surface::Floating)
                    .disabled(self.selected.is_empty() || self.opening)
                    .on_click(cx.listener(|this, _, window, cx| this.open(false, window, cx))),
            )
            .footer(
                Button::new("new-message-send", "Send message")
                    .prominent()
                    .loading(self.opening)
                    .disabled(self.selected.is_empty() || self.draft(cx).is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.open(true, window, cx))),
            )
    }
}

/// The outbox's newest message with this text in `channel`, which
/// `Session::send` just queued (still pending, or kept once confirmed).
async fn queued(
    session: &buzz_relay_client::Session,
    channel: &str,
    draft: &str,
) -> Option<EventId> {
    let outbox = session.outbox().await?;
    let viewer = session.viewer().to_string();
    outbox
        .visible()
        .iter()
        .filter(|op| {
            op.kind() == kinds::CHAT
                && op.channel() == Some(channel)
                && op.event.content == draft
                && op.event.pubkey.to_hex() == viewer
        })
        .max_by_key(|op| op.created_at())
        .map(|op| op.id())
}
