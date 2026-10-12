//! The Discussion tab (`GitHubConversation.tsx`): the description, then
//! the merge, comments and reviews as a timeline. Messages show one line
//! until expanded; older stretches fold into "earlier messages".

use super::body::{self, Block, Body};
use super::body_view;
use super::client::GitHub;
use super::conversation::{self, Entry, Event, EventKind, Item, Source, SourceState};
use super::data::Details;
use super::preview::{self, Preview};
use super::{context_menu, copy_link, inline_link, local_time, relative_time};
use crate::ui::media;
use crate::ui::overlay::Dismiss;
use buzz_relay_client::Media;
use buzz_ui::{
    ActiveTheme, Avatar, Button, ButtonSize, ButtonVariant, Colors, Icon, IconButton,
    IconButtonSize, IconName, Menu, MenuItem, Tooltip, TypeStyled, tokens::font,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Where a message's actions menu is open: from its button, or at the
/// pointer from a right-click.
#[derive(Clone, PartialEq)]
struct OpenMenu {
    key: String,
    at: Option<Point<Pixels>>,
}

/// The conversation's events, grouped for the timeline.
struct Timeline {
    items: Vec<Item>,
    has_events: bool,
}

/// A message's body, read once.
struct Prepared {
    source: String,
    preview: Preview,
    text_body: bool,
    body: Rc<Body>,
    /// The body as one line of text, when it is a single short block.
    line: Option<String>,
}

pub struct ConversationView {
    details: Rc<Details>,
    url: String,
    id: String,
    media: Media,
    discussion: SourceState,
    reviews: SourceState,
    tasks: HashMap<Source, Task<()>>,
    /// Expanded messages.
    open: HashSet<String>,
    /// Expanded history runs.
    history: HashSet<String>,
    menu: Option<OpenMenu>,
    prepared: HashMap<String, Rc<Prepared>>,
    /// The grouped timeline, built once per page that arrives.
    timeline: Option<Rc<Timeline>>,
    /// The timeline's width as last drawn.
    width: Rc<Cell<f32>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Description,
    Review,
    Comment,
    Merge,
}

fn review_label(state: Option<&str>) -> &'static str {
    match state {
        Some("APPROVED") => "Approved",
        Some("CHANGES_REQUESTED") => "Changes requested",
        Some("COMMENTED") => "Review comment",
        Some("DISMISSED") => "Review dismissed",
        _ => "Reviewed",
    }
}

impl ConversationView {
    pub fn new(
        details: Rc<Details>,
        url: String,
        id: String,
        media: Media,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            details,
            url,
            id,
            media,
            discussion: SourceState::default(),
            reviews: SourceState::default(),
            tasks: HashMap::new(),
            open: HashSet::new(),
            history: HashSet::new(),
            menu: None,
            prepared: HashMap::new(),
            timeline: None,
            width: Rc::new(Cell::new(0.)),
        };
        view.fetch(Source::Discussion, 1, cx);
        view.fetch(Source::Reviews, 1, cx);
        view
    }

    fn state(&mut self, source: Source) -> &mut SourceState {
        match source {
            Source::Discussion => &mut self.discussion,
            Source::Reviews => &mut self.reviews,
        }
    }

    fn fetch(&mut self, source: Source, page: u32, cx: &mut Context<Self>) {
        let url = self.url.clone();
        let load = GitHub::global(cx)
            .run(move |fetch| Box::pin(conversation::load_page(fetch, url, source, page)));
        let task = cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                this.state(source).finish(result);
                this.timeline = None;
                cx.notify();
            });
        });
        self.tasks.insert(source, task);
        cx.notify();
    }

    fn more(&mut self, source: Source, cx: &mut Context<Self>) {
        let state = self.state(source);
        let page = if state.error.is_some() {
            state.retry()
        } else {
            state.load_more()
        };
        if let Some(page) = page {
            self.fetch(source, page, cx);
        }
    }

    fn prepare(&mut self, key: &str, entry: &Entry) -> Rc<Prepared> {
        if let Some(prepared) = self.prepared.get(key)
            && prepared.source == entry.body
        {
            return prepared.clone();
        }
        let body = Rc::new(body::prepare(
            &entry.body,
            entry.body_html.as_deref(),
            &self.url,
        ));
        let line = match &*body {
            Body::Rich { blocks, .. } => match blocks.as_slice() {
                [Block::Paragraph(inlines)] => Some(body::plain_text(inlines)),
                _ => None,
            },
            Body::Plain(_) => None,
        }
        .filter(|line| !line.contains('\n'));
        let prepared = Rc::new(Prepared {
            source: entry.body.clone(),
            preview: preview::preview(&entry.body, entry.body_html.as_deref(), &self.url),
            text_body: preview::is_text_body(&entry.body, entry.body_html.as_deref(), &self.url),
            body,
            line,
        });
        self.prepared.insert(key.to_string(), prepared.clone());
        prepared
    }

    fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.open.remove(key) {
            self.open.insert(key.to_string());
        }
        cx.notify();
    }

    fn body(&self, key: &str, prepared: &Prepared, fallback: &str, cx: &App) -> AnyElement {
        if prepared.source.is_empty() {
            return div().child(fallback.to_string()).into_any_element();
        }
        // The first block's top margin is dropped (`> :first-child`).
        let drawn = body_view::draw(
            &prepared.body,
            &body_view::Context {
                id: format!("{}-{key}", self.id).into(),
                media: self.media.clone(),
            },
            cx,
        );
        div()
            .pb(px(drawn.bottom))
            .child(drawn.element)
            .into_any_element()
    }

    /// Whether `text` fits the header's width on one line.
    fn fits(&self, text: &str, window: &Window) -> bool {
        let available = self.width.get() - 32. - 12. - 24.;
        if available <= 0. {
            return false;
        }
        let size = rems(0.875).to_pixels(window.rem_size());
        let mut font = gpui_kit::font(font::SANS);
        font.fallbacks = Some(buzz_ui::fonts::fallbacks());
        let run = TextRun {
            len: text.len(),
            font,
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(text.to_string().into(), size, &[run], None);
        f32::from(line.width) <= available
    }

    #[allow(clippy::too_many_arguments)]
    fn message(
        &mut self,
        key: &str,
        entry: &Entry,
        label: &'static str,
        kind: Kind,
        fallback: &'static str,
        comment_url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let prepared = self.prepare(key, entry);
        let has_body = !entry.body.trim().is_empty() || !fallback.is_empty();
        let can_fit = prepared.text_body && prepared.preview.images.is_empty();
        let fits = can_fit
            && has_body
            && match (entry.body.is_empty(), &prepared.line) {
                (true, _) => self.fits(fallback, window),
                (false, Some(line)) => self.fits(line, window),
                (false, None) => false,
            };
        let expandable = !can_fit || (has_body && !fits);
        let open = expandable && self.open.contains(key);
        let group: SharedString = format!("message-{}-{key}", self.id).into();
        let marker = match kind {
            Kind::Description => {
                let mut avatar = Avatar::new(if entry.author.is_empty() {
                    "?".to_string()
                } else {
                    entry.author.clone()
                });
                if let Some(url) = &entry.author_avatar {
                    avatar = avatar.image(media::image(url, &self.media));
                }
                avatar.into_any_element()
            }
            _ => event_icon(kind, entry.state.as_deref(), &c).into_any_element(),
        };
        let marker = div()
            .id(SharedString::from(format!("{key}-marker")))
            .flex()
            .flex_none()
            .justify_center()
            .w(px(32.))
            .when(expandable, |d| {
                let key = key.to_string();
                d.cursor_pointer()
                    .tooltip(Tooltip::text(format!("Toggle {label}")))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&key, cx)))
            })
            .child(marker);
        let author = {
            let name = if entry.author.is_empty() {
                "Unknown author".to_string()
            } else {
                entry.author.clone()
            };
            let element = match &entry.author_url {
                Some(url) => inline_link(
                    SharedString::from(format!("{key}-author")),
                    name,
                    url.clone(),
                    cx,
                )
                .into_any_element(),
                None => div().child(name).into_any_element(),
            };
            div()
                .text_body_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(element)
        };
        let time = match entry.created_at.as_deref() {
            Some(at) => div()
                .id(SharedString::from(format!("{key}-time")))
                .child(relative_time(at).unwrap_or_default())
                .tooltip(Tooltip::text(local_time(at).unwrap_or_default()))
                .into_any_element(),
            None => div().child("Time unavailable").into_any_element(),
        };
        let metadata = div()
            .flex()
            .flex_wrap()
            .items_baseline()
            .gap_x(px(8.))
            .gap_y(px(4.))
            .pr(px(24.))
            .text_caption()
            .child(author)
            .child(
                div()
                    .id(SharedString::from(format!("{key}-label")))
                    .text_color(c.text_subtle)
                    .child(label)
                    .when(kind == Kind::Review, |d| {
                        d.tooltip(Tooltip::text(
                            "Submitted review event, not the PR’s current approval status",
                        ))
                    }),
            )
            .child(div().ml_auto().text_color(c.text_subtle).child(time));
        let header = div()
            .id(SharedString::from(format!("{key}-header")))
            .relative()
            .min_w_0()
            .child(metadata)
            .when(expandable && !open, |d| {
                let text = if prepared.preview.text.is_empty() {
                    fallback.to_string()
                } else {
                    prepared.preview.text.clone()
                };
                d.child(
                    div()
                        .pt(px(4.))
                        .pr(px(24.))
                        .text_body_sm()
                        .truncate()
                        .child(text),
                )
            })
            .when(!expandable && has_body, |d| {
                d.child(
                    div()
                        .pt(px(4.))
                        .pr(px(24.))
                        .text_body_sm()
                        .child(self.body(key, &prepared, fallback, cx)),
                )
            })
            .when(expandable, |d| {
                let key = key.to_string();
                d.cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(&key, cx)))
                    .child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .right(px(3.))
                            .text_color(c.text_subtle)
                            .child(
                                Icon::new(if open {
                                    IconName::CaretUp
                                } else {
                                    IconName::CaretDown
                                })
                                .size(14.),
                            ),
                    )
            });
        let thumbnails = (!open && !prepared.preview.images.is_empty())
            .then(|| self.thumbnails(key, &prepared.preview.images, &c));
        let panel = open.then(|| {
            div()
                .pt(px(4.))
                .pr(px(24.))
                .text_body_sm()
                .child(self.body(key, &prepared, fallback, cx))
        });
        let show_copy = has_body && expandable && open;
        let menu = self.menu.clone().filter(|m| m.key == key);
        let copy = comment_url.clone().filter(|_| show_copy).map(|url| {
            div()
                .mt(px(8.))
                .child(
                    Button::new(SharedString::from(format!("{key}-copy")), "Copy link")
                        .variant(ButtonVariant::Subtle)
                        .size(ButtonSize::Xs)
                        .icon(IconName::Link)
                        .on_click(move |_, _, cx| copy_link(&url, "Comment link copied", cx)),
                )
                .into_any_element()
        });
        // The actions button sits over the header's bottom right corner
        // (top right on one line), shown on hover or while its menu is open.
        let dots = comment_url.clone().filter(|_| !show_copy).map(|url| {
            let toggle_key = key.to_string();
            let dropdown = menu
                .as_ref()
                .filter(|m| m.at.is_none())
                .map(|_| self.actions_menu(url, window, cx));
            div()
                .absolute()
                .right_0()
                .map(|d| {
                    if fits || !has_body {
                        d.top_0()
                    } else {
                        d.bottom_0()
                    }
                })
                .when(menu.is_none(), |d| {
                    d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                })
                .child(
                    IconButton::new(
                        SharedString::from(format!("{key}-actions")),
                        IconName::DotsThree,
                    )
                    .size(IconButtonSize::Xs)
                    .tooltip("Comment actions")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.menu = match &this.menu {
                            Some(menu) if menu.key == toggle_key && menu.at.is_none() => None,
                            _ => Some(OpenMenu {
                                key: toggle_key.clone(),
                                at: None,
                            }),
                        };
                        cx.notify();
                    })),
                )
                .children(dropdown)
        });
        let header = header.children(dots);
        let pointer_menu = menu
            .filter(|m| m.at.is_some())
            .and(comment_url.clone())
            .map(|url| self.actions_menu(url, window, cx));
        let context_key = key.to_string();
        let content = div()
            .id(SharedString::from(format!("{key}-content")))
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .when(comment_url.is_some(), |d| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.menu = Some(OpenMenu {
                            key: context_key.clone(),
                            at: Some(event.position),
                        });
                        cx.notify();
                    }),
                )
            })
            .child(header)
            .children(thumbnails)
            .children(panel)
            .children(copy)
            .children(pointer_menu);
        div()
            .id(SharedString::from(format!("{key}-message")))
            .group(group)
            .flex()
            .gap(px(12.))
            .py(px(8.))
            .map(|d| {
                if has_body {
                    d.items_start()
                } else {
                    d.items_center()
                }
            })
            .child(marker)
            .child(content)
            .into_any_element()
    }

    fn actions_menu(&self, url: String, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.menu = None;
                cx.notify();
            });
        });
        let close = dismiss.clone();
        let menu = Menu::new().child(
            MenuItem::new("copy-link", "Copy link")
                .icon(IconName::Link)
                .on_click(move |_, window, cx| {
                    copy_link(&url, "Comment link copied", cx);
                    close(window, cx);
                }),
        );
        match self.menu.as_ref().and_then(|m| m.at) {
            Some(at) => context_menu(window, at, menu, dismiss).into_any_element(),
            None => {
                crate::ui::overlay::dropdown(window, crate::ui::overlay::Align::End, menu, dismiss)
                    .into_any_element()
            }
        }
    }

    /// Image thumbnails of a collapsed message: as many 40px tiles as fit,
    /// the last counting the rest.
    fn thumbnails(&self, key: &str, images: &[String], c: &Colors) -> AnyElement {
        let available = (self.width.get() - 32. - 12.).max(40.);
        let capacity = (((available + 4.) / 44.).floor() as usize).max(1);
        let visible = images.len().min(capacity);
        let hidden = images.len() - visible;
        div()
            .mt(px(8.))
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .children(images.iter().take(visible).enumerate().map(|(index, src)| {
                let attachment = buzz_relay_client::fold::Attachment {
                    url: src.clone(),
                    kind: buzz_relay_client::fold::AttachmentKind::Image,
                    mime: None,
                    size: None,
                    name: None,
                    duration: None,
                    waveform: None,
                    voice_note: false,
                    dimensions: None,
                    blurhash: None,
                    preview_url: None,
                    insecure: false,
                };
                let media = self.media.clone();
                div()
                    .id(SharedString::from(format!("{key}-thumb-{index}")))
                    .relative()
                    .flex_none()
                    .size(px(40.))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .bg(c.surface_inset)
                    .cursor(CursorStyle::PointingHand)
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        media::open_media(attachment.clone(), &media, window, cx)
                    })
                    .child(media::thumbnail(src, None, &self.media))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(px(12.))
                            .border_1()
                            .border_color(c.border_standard),
                    )
                    .when(index + 1 == visible && hidden > 0, |d| {
                        d.child(
                            div()
                                .absolute()
                                .right_0()
                                .bottom_0()
                                .px(px(4.))
                                .py(px(2.))
                                .rounded_tl(px(12.))
                                .bg(c.surface_panel)
                                .text_caption()
                                .text_color(c.text_standard)
                                .child(format!("+{hidden}")),
                        )
                    })
            }))
            .into_any_element()
    }

    fn event(&mut self, event: &Event, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (label, kind, fallback) = match event.kind {
            EventKind::Merge => ("Merged", Kind::Merge, ""),
            EventKind::Review => (
                review_label(event.message.state.as_deref()),
                Kind::Review,
                "",
            ),
            EventKind::Discussion => ("Comment", Kind::Comment, "No message provided"),
        };
        let comment_url = (event.kind != EventKind::Merge).then(|| {
            let base = self.url.split('#').next().unwrap_or_default();
            let anchor = if event.kind == EventKind::Review {
                "pullrequestreview"
            } else {
                "issuecomment"
            };
            format!("{base}#{anchor}-{}", event.message.id)
        });
        self.message(
            &event.key,
            &event.message,
            label,
            kind,
            fallback,
            comment_url,
            window,
            cx,
        )
    }

    fn history_run(
        &mut self,
        key: &str,
        events: &[Event],
        after_merge: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let open = self.history.contains(key);
        let count = events.len();
        let noun = if count == 1 { "message" } else { "messages" };
        let summary = if after_merge {
            format!("{count} {noun} after merge")
        } else {
            format!("{count} earlier {noun}")
        };
        let mut authors: Vec<String> = Vec::new();
        for event in events {
            let author = if event.message.author.is_empty() {
                "Unknown author".to_string()
            } else {
                event.message.author.clone()
            };
            if !authors.contains(&author) {
                authors.push(author);
            }
        }
        let toggle_key = key.to_string();
        let trigger = div()
            .id(SharedString::from(format!("history-{key}")))
            .flex()
            .items_center()
            .gap(px(12.))
            .py(px(8.))
            .w_full()
            .cursor_pointer()
            .text_body_sm()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.history.remove(&toggle_key) {
                    this.history.insert(toggle_key.clone());
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded_full()
                    .bg(c.surface_panel)
                    .text_color(c.text_subtle)
                    .child(
                        Icon::new(if open {
                            IconName::CaretUp
                        } else {
                            IconName::DotsThree
                        })
                        .size(20.),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .map(|d| {
                        if open {
                            d.child(if after_merge {
                                "Hide messages after merge"
                            } else {
                                "Hide earlier messages"
                            })
                        } else {
                            d.child(format!("{summary}\u{a0}")).child(
                                div()
                                    .min_w_0()
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .child(format!("· {}", authors.join(", "))),
                            )
                        }
                    }),
            );
        let events = open.then(|| {
            let rows: Vec<AnyElement> = events
                .iter()
                .map(|event| self.event(event, window, cx))
                .collect();
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .pt(px(12.))
                .children(rows)
        });
        div()
            .flex()
            .flex_col()
            .child(trigger)
            .children(events)
            .into_any_element()
    }

    fn source_status(&self, source: Source, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let state = match source {
            Source::Discussion => &self.discussion,
            Source::Reviews => &self.reviews,
        };
        if !state.incomplete() {
            return None;
        }
        let label = source.label();
        let lower = label.to_lowercase();
        let status = if state.loading {
            Some(format!("Loading {lower}…"))
        } else {
            state.error.clone()
        };
        let button = (state.error.is_some() || state.next.is_some()).then(|| {
            let verb = if state.error.is_some() {
                "Retry"
            } else {
                "Load more"
            };
            Button::new(
                SharedString::from(format!("source-{lower}")),
                format!("{verb} {lower}"),
            )
            .size(ButtonSize::Xs)
            .loading(state.loading)
            .on_click(cx.listener(move |this, _, _, cx| this.more(source, cx)))
        });
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .text_caption()
                .text_color(c.text_subtle)
                .child(format!("{label} · {} loaded", state.entries.len()))
                .children(status)
                .when_some(button, |d, button| d.child(div().ml_auto().child(button)))
                .into_any_element(),
        )
    }
}

fn event_icon(kind: Kind, state: Option<&str>, c: &Colors) -> impl IntoElement {
    let (icon, color) = match kind {
        Kind::Merge => (IconName::GitMerge, c.text_categorical_purple),
        Kind::Review => match state {
            Some("APPROVED") => (IconName::CheckCircle, c.text_success),
            Some("CHANGES_REQUESTED") => (IconName::XCircle, c.text_danger),
            Some("DISMISSED") => (IconName::X, c.text_subtle),
            _ => (IconName::ChatCircle, c.text_standard),
        },
        _ => (IconName::ChatCircle, c.text_subtle),
    };
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded_full()
        .bg(c.surface_panel)
        .text_color(color)
        .child(Icon::new(icon).size(20.).color(color))
}

impl Render for ConversationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let timeline = match &self.timeline {
            Some(timeline) => timeline.clone(),
            None => {
                let events = conversation::events(
                    &self.discussion.entries,
                    &self.reviews.entries,
                    Some(&self.details),
                );
                let timeline = Rc::new(Timeline {
                    items: conversation::group(&events),
                    has_events: !events.is_empty(),
                });
                self.timeline = Some(timeline.clone());
                timeline
            }
        };
        let description = Entry::description(&self.details);
        let mut rows = vec![self.message(
            "description",
            &description,
            "Description",
            Kind::Description,
            "No description provided",
            None,
            window,
            cx,
        )];
        for item in &timeline.items {
            rows.push(match item {
                Item::Event(event) => self.event(event, window, cx),
                Item::History {
                    key,
                    events,
                    after_merge,
                } => self.history_run(key, events, *after_merge, window, cx),
            });
        }
        let incomplete = self.discussion.incomplete() || self.reviews.incomplete();
        let width = self.width.clone();
        let measure = canvas(
            move |bounds, window, _| {
                let measured = f32::from(bounds.size.width);
                if (width.get() - measured).abs() > 0.5 {
                    width.set(measured);
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let has_events = timeline.has_events;
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(measure)
                    .when(has_events, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(16.))
                                .top(px(20.))
                                .bottom(px(20.))
                                .w(px(1.))
                                .bg(c.border_standard),
                        )
                    })
                    .children(rows),
            )
            .when(incomplete, |d| {
                let discussion = self.source_status(Source::Discussion, cx);
                let reviews = self.source_status(Source::Reviews, cx);
                d.child(
                    div()
                        .mt(px(16.))
                        .pt(px(8.))
                        .border_t_1()
                        .border_color(c.border_standard)
                        .child(
                            div()
                                .my(px(12.))
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child("Loaded conversation only · some sources are incomplete."),
                        )
                        .children(discussion)
                        .children(reviews),
                )
            })
    }
}
