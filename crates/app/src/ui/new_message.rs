//! "New message" (`NewMessage.tsx`, `RecipientPicker.tsx`): pick one to
//! eight people, then open the DM (or group DM) with exactly them. The relay
//! reuses the conversation for the same people and unhides it.

use super::message::display_name;
use super::modal::{Dismissed, Leading, OpenConversation, error, group_label, note, row};
use crate::community::Community;
use buzz_relay_client::PeoplePage;
use buzz_relay_client::members::Person;
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, Dialog, Icon, IconName, SearchField, Surface,
    TypeStyled,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(150);
/// Other people a DM can hold (`direct-message-policy.ts`).
const MAX_RECIPIENTS: usize = 8;

pub struct NewMessageDialog {
    community: Entity<Community>,
    input: Entity<InputState>,
    selected: Vec<Person>,
    found: Option<(String, Result<PeoplePage, String>)>,
    searching: Option<Task<()>>,
    highlighted: usize,
    opening: bool,
    error: Option<SharedString>,
    on_open: OpenConversation,
    _subscription: Subscription,
}

impl EventEmitter<Dismissed> for NewMessageDialog {}

impl NewMessageDialog {
    pub fn new(
        community: Entity<Community>,
        on_open: OpenConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find a person…"));
        input.update(cx, |state, cx| state.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.highlighted = 0;
                    this.search(window, cx);
                }
            });
        let mut dialog = Self {
            community,
            input,
            selected: Vec::new(),
            found: None,
            searching: None,
            highlighted: 0,
            opening: false,
            error: None,
            on_open,
            _subscription: subscription,
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
        self.input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.set_placeholder("Add another person…", window, cx);
            state.focus(window, cx);
        });
        self.highlighted = 0;
        self.search(window, cx);
    }

    fn remove(&mut self, pubkey: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected.retain(|p| p.pubkey != pubkey);
        if self.selected.is_empty() {
            self.input.update(cx, |state, cx| {
                state.set_placeholder("Find a person…", window, cx);
            });
        }
        cx.notify();
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() || self.opening {
            return;
        }
        let pubkeys: Vec<String> = self.selected.iter().map(|p| p.pubkey.clone()).collect();
        let session = self.community.read(cx).session.clone();
        self.opening = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = session.open_dm(pubkeys).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.opening = false;
                match result {
                    Ok(id) => {
                        (this.on_open)(id, window, cx);
                        cx.emit(Dismissed);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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
                                _ => this.open(window, cx),
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
                            .h(px(300.))
                            .overflow_y_scroll()
                            .child(list),
                    )
                    .children(self.error.clone().map(|message| error(message, cx))),
            )
            .footer(
                Button::new("new-message-cancel", "Cancel")
                    .surface(Surface::Floating)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
            )
            .footer(
                Button::new("new-message-open", "Open conversation")
                    .prominent()
                    .loading(self.opening)
                    .disabled(self.selected.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.open(window, cx))),
            )
    }
}
