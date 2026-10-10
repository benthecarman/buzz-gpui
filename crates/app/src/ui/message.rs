//! One timeline row (ui.md §4.4–4.5, §4.7).

use super::emoji_picker::{self, EmojiPicker, QuickReaction};
use super::markdown::{self, Directory};
use super::{media, time};
use buzz_relay_client::fold::{Attachment, AttachmentKind, Delivery, MessageRow};
use buzz_relay_client::{EventId, Profile, Profiles};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Icon, IconButton, IconButtonShape, IconButtonSize, IconName,
    MaterialStyled, Menu, MenuItem, Surface, Tooltip, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

/// A message plus how it sits in the timeline.
#[derive(Clone)]
pub struct Item {
    pub row: Arc<MessageRow>,
    /// Starts a group: shows avatar and byline.
    pub head: bool,
    /// A day divider goes above this row.
    pub day: Option<SharedString>,
}

/// Groups rows as buzz-app's `continuesMessageGroup` does: same author, at
/// most five minutes apart, same local day, and neither row a system,
/// workflow or sent-from-thread message.
pub fn items(rows: &[Arc<MessageRow>]) -> Vec<Item> {
    let mut items = Vec::with_capacity(rows.len());
    let mut previous: Option<&Arc<MessageRow>> = None;
    for row in rows {
        let new_day = previous.is_none_or(|p| !time::same_day(p.created_at, row.created_at));
        let continues = !new_day && previous.is_some_and(|p| continues(p, row));
        items.push(Item {
            row: row.clone(),
            head: !continues,
            day: new_day.then(|| time::day(row.created_at).into()),
        });
        previous = Some(row);
    }
    items
}

/// Whether `row` continues the group `previous` started (same day checked
/// separately).
pub fn continues(previous: &MessageRow, row: &MessageRow) -> bool {
    previous.author == row.author
        && row.created_at >= previous.created_at
        && row.created_at - previous.created_at <= 300
        && previous.membership.is_none()
        && row.membership.is_none()
        && previous.workflow_owner.is_none()
        && row.workflow_owner.is_none()
        && previous.sent_from_thread.is_none()
        && row.sent_from_thread.is_none()
}

/// The day divider (§4.4): a label between two hairlines.
pub fn day_divider(day: SharedString, cx: &App) -> Div {
    let c = *cx.colors();
    div()
        .flex()
        .items_center()
        .my(px(12.))
        .child(div().flex_1().h(px(1.)).bg(c.border_standard))
        .child(
            div()
                .px(px(12.))
                .py(px(6.))
                .text_caption()
                .text_color(c.text_standard)
                .child(day),
        )
        .child(div().flex_1().h(px(1.)).bg(c.border_standard))
}

pub fn display_name(profiles: &Profiles, pubkey: &str) -> SharedString {
    profiles
        .get(pubkey)
        .map(|profile| profile.name.clone())
        .unwrap_or_else(|| Profile::fallback(pubkey).name)
        .into()
}

/// Something a row asks its page to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    CopyLink,
    CopyMessage,
    MarkUnread,
    Edit,
    Delete,
}

/// Which floating element of a row is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popup {
    /// The "More message actions" menu.
    More,
    /// The reaction picker, from the action bar or the reactions row.
    Picker { from_bar: bool },
}

/// What the pointer is over, for the hover action bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hover {
    Row,
    Bar,
}

/// Pointer and popup state the page keeps for its rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Interaction {
    pub hovered: Option<EventId>,
    pub bar_hovered: Option<EventId>,
    pub open: Option<(EventId, Popup)>,
}

impl Interaction {
    fn bar_visible(&self, id: EventId) -> bool {
        self.hovered == Some(id)
            || self.bar_hovered == Some(id)
            || self.open.is_some_and(|(open, _)| open == id)
    }
}

pub type RowHandler = Rc<dyn Fn(&Arc<MessageRow>, &mut Window, &mut App)>;
pub type ReactHandler = Rc<dyn Fn(&Arc<MessageRow>, &str, &mut Window, &mut App)>;
pub type HoverHandler = Rc<dyn Fn(EventId, Hover, bool, &mut Window, &mut App)>;
pub type PopupHandler = Rc<dyn Fn(Option<(EventId, Popup)>, &mut Window, &mut App)>;
pub type CommandHandler = Rc<dyn Fn(&Arc<MessageRow>, Command, &mut Window, &mut App)>;

#[derive(Default)]
pub struct Actions {
    pub on_open_thread: Option<RowHandler>,
    pub on_react: Option<ReactHandler>,
    pub on_retry: Option<RowHandler>,
    /// Without it rows show no action bar.
    pub on_hover: Option<HoverHandler>,
    pub on_popup: Option<PopupHandler>,
    pub on_command: Option<CommandHandler>,
}

/// What every row of a page shares.
pub struct Rows {
    pub viewer: String,
    pub directory: Directory,
    pub actions: Actions,
    pub interaction: Interaction,
    /// The hover bar's three reactions (`quick-reactions.ts`).
    pub quick_reactions: Vec<QuickReaction>,
    /// The page's reaction picker; it reports picks to the page.
    pub picker: Option<Entity<EmojiPicker>>,
}

impl Rows {
    fn own(&self, row: &MessageRow) -> bool {
        row.author.to_hex() == self.viewer || row.signer.is_some_and(|s| s.to_hex() == self.viewer)
    }

    fn popup(&self, value: Option<(EventId, Popup)>, window: &mut Window, cx: &mut App) {
        if let Some(on_popup) = &self.actions.on_popup {
            on_popup(value, window, cx);
        }
    }
}

/// Where a row is drawn. Thread panels use a 32px gutter (ui.md §4.11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Timeline,
    /// A thread root or top-level reply: 32px avatar.
    Thread,
    /// A nested reply: 24px avatar centred in the 32px gutter.
    Nested,
}

pub fn render(item: &Item, rows: &Rc<Rows>, cx: &App) -> AnyElement {
    render_in(Layout::Timeline, item, rows, cx)
}

pub fn render_in(layout: Layout, item: &Item, rows: &Rc<Rows>, cx: &App) -> AnyElement {
    let c = *cx.colors();
    let (gutter, gap, avatar_size) = match layout {
        Layout::Timeline => (40., 12., AvatarSize::Large),
        Layout::Thread => (32., 8., AvatarSize::Default),
        Layout::Nested => (32., 8., AvatarSize::Small),
    };
    let row = &item.row;
    let profiles = &rows.directory.profiles;
    let author = row.author.to_hex();
    let profile = profiles.get(&author);
    let name = display_name(profiles, &author);
    let agent = row.is_agent_envelope() || profile.is_some_and(|p| p.is_agent);

    let mut container = div().flex().flex_col().w_full();
    if let Some(day) = &item.day {
        container = container.child(day_divider(day.clone(), cx));
    }

    if let Some(change) = &row.membership {
        let subject = display_name(profiles, &change.target.to_hex());
        let text = format!("{subject} {}", membership_text(change, profiles));
        return container
            .child(
                div()
                    .flex()
                    .justify_center()
                    .gap(px(12.))
                    .pt(px(8.))
                    .pb(px(16.))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(text),
            )
            .into_any_element();
    }

    let id = row.id.to_hex();
    let bar_visible = rows.actions.on_hover.is_some() && rows.interaction.bar_visible(row.id);
    let left = if item.head {
        let mut avatar = Avatar::new(name.clone()).size(avatar_size);
        if agent {
            avatar = avatar.squircle();
        }
        if let Some(picture) = profile.and_then(|p| p.picture.as_deref()) {
            avatar = avatar.image(media::avatar(picture, &rows.directory.media));
        }
        div()
            .flex_none()
            .w(px(gutter))
            .flex()
            .justify_center()
            .child(avatar)
            .into_any_element()
    } else {
        div()
            .id(SharedString::from(format!("time-{id}")))
            .flex_none()
            .w(px(gutter))
            .h(px(20.))
            .flex()
            .justify_center()
            .text_caption()
            .line_height(px(20.))
            .text_color(c.text_subtle)
            .when(!bar_visible, |d| d.invisible())
            .tooltip(Tooltip::text(time::full(row.created_at)))
            .child(time::compact(row.created_at))
            .into_any_element()
    };

    let mut body = div().flex().flex_col().flex_1().min_w_0();
    if item.head {
        body = body.child(
            div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap(px(8.))
                .text_body_sm()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c.text_standard)
                        .child(name),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("byline-{id}")))
                        .text_caption()
                        .text_color(c.text_subtle)
                        .tooltip(Tooltip::text(time::full(row.created_at)))
                        .child(time::byline(row.created_at)),
                ),
        );
    }
    if let Some(sent) = &row.sent_from_thread {
        let link = match &sent.excerpt {
            Some(excerpt) => format!("Thread — {excerpt}"),
            None => "Thread".to_string(),
        };
        body = body.child(
            div()
                .flex()
                .my(px(4.))
                .text_caption()
                .text_color(c.text_subtle)
                .child("Sent from ")
                .child(div().text_color(c.text_link).child(link)),
        );
    }
    if let Some(card) = super::diff::card(row) {
        body = body.child(div().when(item.head, |d| d.mt(px(4.))).child(card));
    } else if !row.content.trim().is_empty() {
        body = body.child(
            div()
                .id(SharedString::from(format!("md-{id}")))
                .when(item.head, |d| d.mt(px(4.)))
                .text_body_sm()
                .text_color(c.text_standard)
                .child(markdown::body(row, &rows.directory, cx)),
        );
    }
    if !row.attachments.is_empty() {
        body = body.child(attachments(row, rows, cx));
    }
    if !row.reactions.is_empty() {
        body = body.child(reactions(row, rows, cx));
    }
    // Inside a thread the replies are already on screen.
    if row.reply_count > 0 && layout == Layout::Timeline {
        body = body.child(thread_summary(row, rows, cx));
    }
    if let Some(delivery) = &row.delivery
        && matches!(delivery.status, Delivery::Failed | Delivery::Unknown)
    {
        body = body.child(delivery_notice(row, delivery, rows, cx));
    }

    let mut line = div()
        .id(SharedString::from(format!("message-{id}")))
        .relative()
        .flex()
        .gap(px(gap))
        .pt(if item.head { px(14.) } else { px(2.) })
        .pb(px(2.))
        .child(left)
        .child(body);
    if let Some(on_hover) = rows.actions.on_hover.clone() {
        let target = row.id;
        line = line.on_hover(move |hovered, window, cx| {
            on_hover(target, Hover::Row, *hovered, window, cx)
        });
    }
    if bar_visible {
        line = line.child(action_bar(item, rows, cx));
    }
    container.child(line).into_any_element()
}

/// The hover action bar (§4.5): quick reactions, add reaction, reply, copy
/// link and the more menu. Its bottom sits 4px under the byline, so a
/// continuation row's bar floats over the row above.
fn action_bar(item: &Item, rows: &Rc<Rows>, cx: &App) -> impl IntoElement {
    let c = *cx.colors();
    let row = &item.row;
    let id = row.id.to_hex();
    let open = rows
        .interaction
        .open
        .filter(|(open, _)| *open == row.id)
        .map(|(_, popup)| popup);
    let delivered = delivered(row);
    let button = |name: &str, icon: IconName| {
        IconButton::new(SharedString::from(format!("{name}-{id}")), icon)
            .size(IconButtonSize::Sm)
            .surface(Surface::Floating)
    };

    let mut bar = div()
        .id(SharedString::from(format!("bar-{id}")))
        .absolute()
        .right_0()
        .top(if item.head { px(0.) } else { px(-36.) })
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .floating_surface(&c)
        .rounded(px(12.))
        .occlude();
    if let Some(on_hover) = rows.actions.on_hover.clone() {
        let target = row.id;
        bar = bar.on_hover(move |hovered, window, cx| {
            on_hover(target, Hover::Bar, *hovered, window, cx)
        });
    }
    if delivered && let Some(react) = rows.actions.on_react.clone() {
        for (index, quick) in rows.quick_reactions.iter().enumerate() {
            let (react, target) = (react.clone(), row.clone());
            let content = quick.content.clone();
            let glyph = match &quick.image {
                Some(url) => img(media::image(url, &rows.directory.media))
                    .size(px(16.))
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => div()
                    .text_size(px(14.))
                    .child(content.clone())
                    .into_any_element(),
            };
            bar = bar.child(
                IconButton::new(
                    SharedString::from(format!("quick-{index}-{id}")),
                    IconName::Smiley,
                )
                .size(IconButtonSize::Sm)
                .surface(Surface::Floating)
                .child(glyph)
                .tooltip(format!("React with {content}"))
                .on_click(move |_, window, cx| react(&target, &content, window, cx)),
            );
        }
        let (rows, target) = (rows.clone(), row.id);
        bar = bar.child(
            button("add-reaction", IconName::SmileyPlus)
                .shape(IconButtonShape::Round)
                .selected(open == Some(Popup::Picker { from_bar: true }))
                .tooltip("Add reaction")
                .on_click(move |_, window, cx| {
                    rows.popup(Some((target, Popup::Picker { from_bar: true })), window, cx)
                }),
        );
    }
    if row.thread_root_id.is_none()
        && let Some(open_thread) = rows.actions.on_open_thread.clone()
    {
        let target = row.clone();
        bar = bar.child(
            button("reply", IconName::ChatCircle)
                .tooltip("Reply")
                .on_click(move |_, window, cx| open_thread(&target, window, cx)),
        );
    }
    if delivered && let Some(command) = rows.actions.on_command.clone() {
        let target = row.clone();
        bar = bar.child(
            button("copy-link", IconName::Link)
                .tooltip("Copy link")
                .on_click(move |_, window, cx| command(&target, Command::CopyLink, window, cx)),
        );
    }
    {
        let (rows, target) = (rows.clone(), row.id);
        bar = bar.child(
            button("more", IconName::DotsThree)
                .tooltip("More message actions")
                .on_click(move |_, window, cx| rows.popup(Some((target, Popup::More)), window, cx)),
        );
    }
    let popup = match open {
        Some(Popup::More) => Some(more_menu(row, rows).into_any_element()),
        Some(Popup::Picker { from_bar: true }) => Some(picker(rows)),
        _ => None,
    };
    bar.children(popup.map(|popup| {
        div().absolute().right_0().bottom_0().child(deferred(
            anchored()
                .anchor(Anchor::TopRight)
                .offset(point(px(0.), px(4.)))
                .snap_to_window_with_margin(px(8.))
                .child(popup),
        ))
    }))
}

/// Links, reactions and edits need the relay to have the message.
fn delivered(row: &MessageRow) -> bool {
    row.delivery
        .as_ref()
        .is_none_or(|d| matches!(d.status, Delivery::Accepted | Delivery::Seen))
}

fn more_menu(row: &Arc<MessageRow>, rows: &Rc<Rows>) -> impl IntoElement {
    let id = row.id.to_hex();
    let own = rows.own(row);
    let delivered = delivered(row);
    let item = |name: &str, label: &str, icon: IconName, command: Command| {
        let (rows, target) = (rows.clone(), row.clone());
        MenuItem::new(
            SharedString::from(format!("{name}-{id}")),
            label.to_string(),
        )
        .icon(icon)
        .on_click(move |_, window, cx| {
            rows.popup(None, window, cx);
            if let Some(on_command) = &rows.actions.on_command {
                on_command(&target, command.clone(), window, cx);
            }
        })
    };
    let close = rows.clone();
    let menu = Menu::new()
        .when(delivered, |menu| {
            menu.child(item(
                "unread",
                "Mark unread",
                IconName::Envelope,
                Command::MarkUnread,
            ))
        })
        .child(item(
            "copy",
            "Copy message",
            IconName::Copy,
            Command::CopyMessage,
        ))
        .when(own && delivered, |menu| {
            menu.child(item(
                "edit",
                "Edit message",
                IconName::PencilSimple,
                Command::Edit,
            ))
        })
        .when(own, |menu| {
            menu.child(item("delete", "Delete message", IconName::Trash, Command::Delete).danger())
        });
    div()
        .occlude()
        .on_mouse_down_out(move |_, window, cx| close.popup(None, window, cx))
        .child(menu)
}

/// The page's emoji picker as the reaction picker. The page reacts with
/// what it picks to the row whose picker is open.
fn picker(rows: &Rc<Rows>) -> AnyElement {
    let Some(picker) = &rows.picker else {
        return div().into_any_element();
    };
    let close = rows.clone();
    emoji_picker::emoji_picker(picker, move |_, window, cx| close.popup(None, window, cx))
        .into_any_element()
}

fn reactions(row: &Arc<MessageRow>, rows: &Rc<Rows>, cx: &App) -> impl IntoElement {
    let c = *cx.colors();
    let id = row.id.to_hex();
    let mut pills = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.))
        .mt(px(6.));
    for (index, group) in row.reactions.iter().enumerate() {
        let mut people: Vec<String> = group.events.iter().map(|r| r.author.to_hex()).collect();
        people.sort();
        people.dedup();
        let mine = people.iter().any(|p| *p == rows.viewer);
        // "You" first, then everyone else by name.
        let mut names: Vec<String> = people
            .iter()
            .filter(|p| **p != rows.viewer)
            .map(|p| display_name(&rows.directory.profiles, p).to_string())
            .collect();
        names.sort();
        if mine {
            names.insert(0, "You".into());
        }
        let glyph = match &group.emoji {
            Some(custom) => {
                let fallback = group.content.clone();
                img(media::image(&custom.url, &rows.directory.media))
                    .size(px(14.))
                    .object_fit(ObjectFit::Contain)
                    .with_fallback(move || {
                        div()
                            .text_size(px(12.))
                            .child(fallback.clone())
                            .into_any_element()
                    })
                    .into_any_element()
            }
            None => div()
                .text_size(px(14.))
                .line_height(px(18.))
                .min_w(px(18.))
                .flex()
                .justify_center()
                .child(group.content.clone())
                .into_any_element(),
        };
        let mut pill = div()
            .id(SharedString::from(format!("reaction-{id}-{index}")))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .min_w(px(48.))
            .max_w(px(160.))
            .min_h(px(28.))
            .px(px(8.))
            .rounded_full()
            .border_1()
            .cursor_pointer()
            .tooltip(Tooltip::text(format!(
                "{} {}",
                group.content,
                names.join(", ")
            )))
            .child(glyph)
            .child(
                div()
                    .text_size(px(12.))
                    .line_height(px(14.))
                    .child(people.len().to_string()),
            );
        pill = if mine {
            pill.border_color(mix(c.affordance_prominent, c.border_standard, 0.4))
                .bg(mix(c.affordance_prominent, c.surface_panel, 0.12))
                .text_color(c.affordance_prominent)
        } else {
            pill.border_color(c.border_standard)
                .bg(c.affordance_subtle)
                .text_color(c.text_standard)
                .hover(|style| style.bg(c.affordance_subtle_hover))
        };
        if let Some(react) = rows.actions.on_react.clone() {
            let target = row.clone();
            let content = group.content.clone();
            pill = pill.on_click(move |_, window, cx| react(&target, &content, window, cx));
        }
        pills = pills.child(pill);
    }
    if rows.actions.on_react.is_some() {
        let open = rows.interaction.open == Some((row.id, Popup::Picker { from_bar: false }));
        let (popup_rows, target) = (rows.clone(), row.id);
        let button = div()
            .id(SharedString::from(format!("react-more-{id}")))
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .cursor_pointer()
            .map(|d| {
                if open {
                    d.border_color(c.border_standard)
                        .bg(c.affordance_subtle)
                        .text_color(c.text_standard)
                } else {
                    d.border_color(transparent_black())
                        .text_color(c.text_subtle)
                        .hover(|s| {
                            s.border_color(c.border_standard)
                                .bg(c.affordance_subtle)
                                .text_color(c.text_standard)
                        })
                }
            })
            .tooltip(Tooltip::text("Add reaction"))
            .child(Icon::new(IconName::SmileyPlus).size(16.))
            .on_click(move |_, window, cx| {
                popup_rows.popup(
                    Some((target, Popup::Picker { from_bar: false })),
                    window,
                    cx,
                )
            });
        pills = pills.child(div().relative().child(button).when(open, |d| {
            d.child(
                div().absolute().left_0().bottom_0().child(deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .offset(point(px(0.), px(4.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(picker(rows)),
                )),
            )
        }));
    }
    pills
}

/// CSS `color-mix(in srgb, a share, b)`.
fn mix(a: Hsla, b: Hsla, share: f32) -> Hsla {
    let (a, b) = (Rgba::from(a), Rgba::from(b));
    let lerp = |x: f32, y: f32| x * share + y * (1. - share);
    Hsla::from(Rgba {
        r: lerp(a.r, b.r),
        g: lerp(a.g, b.g),
        b: lerp(a.b, b.b),
        a: lerp(a.a, b.a),
    })
}

/// Attachments in send order; adjacent images share a strip and adjacent
/// files a group (§4.7). Videos and audio stand alone.
fn attachments(row: &Arc<MessageRow>, rows: &Rc<Rows>, cx: &App) -> impl IntoElement {
    let mut groups: Vec<(AttachmentKind, Vec<(usize, &Attachment)>)> = Vec::new();
    for (index, attachment) in row.attachments.iter().enumerate() {
        match groups.last_mut() {
            Some((kind, members)) if *kind == attachment.kind => members.push((index, attachment)),
            _ => groups.push((attachment.kind, vec![(index, attachment)])),
        }
    }
    let c = *cx.colors();
    let media = &rows.directory.media;
    div()
        .flex()
        .flex_col()
        .children(groups.into_iter().map(|(kind, members)| {
            if kind == AttachmentKind::Video {
                div()
                    .flex()
                    .flex_col()
                    .children(members.into_iter().map(|(index, attachment)| {
                        let (target, url, media) =
                            (row.clone(), attachment.url.clone(), media.clone());
                        media::video_preview(
                            SharedString::from(format!("video-{}-{index}", row.id.to_hex())),
                            attachment,
                            &rows.directory.media,
                            Rc::new(move |window, cx| {
                                media::open_review(&target, &url, &media, window, cx)
                            }),
                        )
                    }))
                    .into_any_element()
            } else if kind == AttachmentKind::Audio {
                // Only relay media plays inline; other links stay file cards.
                div()
                    .flex()
                    .flex_col()
                    .children(members.into_iter().map(|(index, attachment)| {
                        let id = SharedString::from(format!("audio-{}-{index}", row.id.to_hex()));
                        if !media.is_relay_media(&attachment.url) {
                            file_tile(row, index, attachment, rows, &c)
                        } else if attachment.voice_note {
                            media::voice_note(id, attachment, media, cx)
                        } else {
                            media::audio_attachment(id, attachment, media, cx)
                        }
                    }))
                    .into_any_element()
            } else if kind == AttachmentKind::Image {
                let count = members.len();
                div()
                    .mt(px(8.))
                    .flex()
                    .flex_col()
                    .child(div().flex().flex_wrap().gap(px(8.)).p(px(4.)).children(
                        members.into_iter().map(|(index, attachment)| {
                            image_tile(row, index, attachment, rows, &c)
                        }),
                    ))
                    .when(count > 1, |d| {
                        d.child(
                            div()
                                .mt(px(4.))
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child(format!("{count} images")),
                        )
                    })
                    .into_any_element()
            } else {
                div()
                    .mt(px(8.))
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .children(
                        members
                            .into_iter()
                            .map(|(index, attachment)| file_tile(row, index, attachment, rows, &c)),
                    )
                    .into_any_element()
            }
        }))
}

fn image_tile(
    row: &Arc<MessageRow>,
    index: usize,
    attachment: &Attachment,
    rows: &Rc<Rows>,
    c: &buzz_ui::Colors,
) -> AnyElement {
    let media = rows.directory.media.clone();
    let (target, url) = (row.clone(), attachment.url.clone());
    div()
        .id(SharedString::from(format!(
            "image-{}-{index}",
            row.id.to_hex()
        )))
        .relative()
        .size(px(72.))
        .flex_none()
        .rounded(px(16.))
        .overflow_hidden()
        .bg(c.surface_inset)
        .cursor_pointer()
        .child(media::thumbnail(
            &attachment.url,
            attachment.blurhash.as_deref(),
            &media,
        ))
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(px(16.))
                .border_1()
                .border_color(c.border_image),
        )
        .on_click(move |_, window, cx| media::open_review(&target, &url, &media, window, cx))
        .into_any_element()
}

/// Files, and audio that does not play inline: a card that downloads
/// relay media to the Downloads folder or opens other links. Plain-HTTP
/// media cannot be fetched, so its card only says so.
fn file_tile(
    row: &Arc<MessageRow>,
    index: usize,
    attachment: &Attachment,
    rows: &Rc<Rows>,
    c: &buzz_ui::Colors,
) -> AnyElement {
    let media = rows.directory.media.clone();
    let insecure = attachment.insecure;
    let relay = !insecure && media.is_relay_media(&attachment.url);
    let name = attachment
        .name
        .clone()
        .or_else(|| mime_label(attachment.mime.as_deref()))
        .unwrap_or_else(|| "File".into());
    let meta = match attachment.size {
        _ if insecure => "File unavailable".into(),
        Some(size) => file_size(size),
        None if relay => "Download file".into(),
        None => "Open file".into(),
    };
    let icon = match attachment.kind {
        AttachmentKind::Video => IconName::VideoCamera,
        AttachmentKind::Audio => IconName::SpeakerHigh,
        _ => IconName::FileText,
    };
    let (url, download_name) = (attachment.url.clone(), attachment.name.clone());
    div()
        .id(SharedString::from(format!(
            "file-{}-{index}",
            row.id.to_hex()
        )))
        .flex()
        .items_center()
        .gap(px(8.))
        .max_w(px(384.))
        .min_w_0()
        .px(px(12.))
        .py(px(8.))
        .border_1()
        .border_color(c.border_standard)
        .rounded(px(12.))
        .bg(c.affordance_subtle)
        .when(!insecure, |d| {
            d.hover(|s| s.bg(c.affordance_subtle_hover))
                .cursor_pointer()
        })
        .tooltip(Tooltip::text(name.clone()))
        .child(Icon::new(icon).size(24.))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(name),
                )
                .child(div().text_caption().text_color(c.text_subtle).child(meta)),
        )
        .when(relay, |d| d.child(Icon::new(IconName::Download).size(20.)))
        .on_click(move |_, _, cx| {
            if insecure {
                return;
            }
            if relay {
                media::download(url.clone(), download_name.clone(), media.clone(), cx);
            } else {
                cx.open_url(&url);
            }
        })
        .into_any_element()
}

/// `formatFileSize`.
fn file_size(size: u64) -> String {
    const K: u64 = 1024;
    let round = |unit: u64| (size as f64 / unit as f64).round() as u64;
    match size {
        s if s < K => format!("{s} B"),
        s if s < K * K => format!("{} KB", round(K)),
        s if s < K * K * K => format!("{} MB", round(K * K)),
        _ => format!("{} GB", round(K * K * K)),
    }
}

fn mime_label(mime: Option<&str>) -> Option<String> {
    let (kind, subtype) = mime?.split_once('/')?;
    if kind.is_empty() || subtype.is_empty() {
        return None;
    }
    if subtype == "pdf" {
        return Some("PDF file".into());
    }
    subtype
        .bytes()
        .all(|b| b.is_ascii_alphanumeric())
        .then(|| format!("{} file", subtype.to_uppercase()))
}

fn delivery_notice(
    row: &Arc<MessageRow>,
    delivery: &buzz_relay_client::fold::LocalDelivery,
    rows: &Rc<Rows>,
    cx: &App,
) -> impl IntoElement {
    let c = cx.colors();
    let text = if delivery.status == Delivery::Failed {
        "Couldn’t send this message.".to_string()
    } else {
        match &delivery.error {
            Some(error) => format!("Delivery not yet confirmed. {error}"),
            None => "Delivery not yet confirmed.".to_string(),
        }
    };
    let mut notice = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .mt(px(6.))
        .text_caption()
        .text_color(c.text_warning)
        .child(text);
    if let Some(retry) = rows.actions.on_retry.clone() {
        let target = row.clone();
        notice = notice.child(
            buzz_ui::Button::new(
                SharedString::from(format!("retry-{}", row.id.to_hex())),
                "Retry",
            )
            .variant(buzz_ui::ButtonVariant::Ghost)
            .size(buzz_ui::ButtonSize::Sm)
            .on_click(move |_, window, cx| retry(&target, window, cx)),
        );
    }
    notice
}

fn thread_summary(row: &Arc<MessageRow>, rows: &Rc<Rows>, cx: &App) -> impl IntoElement {
    let c = *cx.colors();
    let profiles = &rows.directory.profiles;
    let mut avatars = div().flex();
    for (index, participant) in row.participants.iter().take(3).enumerate() {
        let hex = participant.to_hex();
        let profile = profiles.get(&hex);
        let mut avatar = Avatar::new(display_name(profiles, &hex)).size(AvatarSize::Small);
        if profile.is_some_and(|p| p.is_agent) {
            avatar = avatar.squircle();
        }
        if let Some(picture) = profile.and_then(|p| p.picture.as_deref()) {
            avatar = avatar.image(media::avatar(picture, &rows.directory.media));
        }
        avatars = avatars.child(
            div()
                .when(index > 0, |d| d.ml(px(-4.)))
                .rounded_full()
                .p(px(1.5))
                .bg(c.surface_panel)
                .child(avatar),
        );
    }
    if row.participants.len() > 3 {
        avatars = avatars.child(
            div()
                .ml(px(-4.))
                .size(px(24.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(c.affordance_selected)
                .text_caption()
                .child(format!("+{}", row.participants.len() - 3)),
        );
    }
    let label = if row.reply_count == 1 {
        "1 reply".to_string()
    } else {
        format!("{} replies", row.reply_count)
    };
    let mut summary = div()
        .id(SharedString::from(format!("thread-{}", row.id.to_hex())))
        .flex()
        .items_center()
        .self_start()
        .gap(px(8.))
        .ml(px(-4.))
        .mt(px(4.))
        .p(px(4.))
        .pr(px(6.))
        .min_h(px(32.))
        .rounded(px(10.))
        .cursor_pointer()
        .text_caption()
        .text_color(c.text_standard)
        .hover(|style| style.bg(c.affordance_subtle_hover))
        .child(avatars)
        .child(label);
    if let Some(open) = rows.actions.on_open_thread.clone() {
        let target = row.clone();
        summary = summary.on_click(move |_, window, cx| open(&target, window, cx));
    }
    summary
}

fn membership_text(
    change: &buzz_relay_client::fold::MembershipChange,
    profiles: &Profiles,
) -> String {
    use buzz_relay_client::fold::MembershipKind;
    match change.kind {
        MembershipKind::Joined => "joined".into(),
        MembershipKind::Left => "left the channel".into(),
        MembershipKind::Removed if change.actor != change.target => {
            format!(
                "was removed by {}",
                display_name(profiles, &change.actor.to_hex())
            )
        }
        MembershipKind::Removed => "was removed".into(),
    }
}

/// Icon for a channel in headers and the sidebar.
pub fn channel_icon(channel: &buzz_relay_client::ChannelSummary) -> IconName {
    use buzz_relay_client::ChannelType;
    match channel.channel_type {
        Some(ChannelType::Dm) => IconName::ChatCircle,
        _ if channel.private => IconName::Lock,
        _ => IconName::Hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn formats_file_sizes_like_the_reference() {
        assert_eq!(file_size(900), "900 B");
        assert_eq!(file_size(13 * 1024 + 100), "13 KB");
        assert_eq!(file_size(5 * 1024 * 1024), "5 MB");
        assert_eq!(
            mime_label(Some("application/pdf")).as_deref(),
            Some("PDF file")
        );
        assert_eq!(
            mime_label(Some("text/plain")).as_deref(),
            Some("PLAIN file")
        );
        assert_eq!(mime_label(Some("application/vnd.x+y")), None);
    }
}
