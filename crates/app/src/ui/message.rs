//! One timeline row (ui.md §4.4–4.5, §4.7), after buzz-app's
//! `MessageRow.tsx`, `MessageActionBar.tsx` and `MessageReactions.tsx`.

use super::emoji_picker::{self, EmojiPicker, QuickReaction};
use super::identity::Names;
use super::keys::npub;
use super::markdown::{self, Directory};
use super::outbox_ops::{Ops, SharedOps};
use super::{media, membership, navigation, renderers, time};
use buzz_relay_client::fold::{CustomEmoji, Delivery, MessageRow, ReactionGroup};
use buzz_relay_client::{
    ChannelSummary, ChannelType, EventId, Manual, PresenceState, Profile, Profiles, ThreadUnread,
};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Icon, IconButton, IconButtonShape, IconButtonSize, IconName,
    MaterialStyled, Menu, MenuItem, Presence, Surface, Tooltip, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Nothing is said about a send for its first ten seconds (`delivery.ts`).
pub const DELIVERY_GRACE_MS: u64 = 10_000;
/// Rows this narrow drop the quick reactions; the picker keeps them all.
const QUICK_REACTIONS_MIN_WIDTH: f32 = 260.;

/// A message plus how it sits in the timeline.
#[derive(Clone)]
pub struct Item {
    pub row: Arc<MessageRow>,
    /// Starts a group: shows avatar and byline.
    pub head: bool,
    /// A day divider goes above this row.
    pub day: Option<SharedString>,
    /// A run of membership rows shown as one, oldest first.
    pub group: Option<Arc<[Arc<MessageRow>]>>,
}

/// Groups rows as buzz-app's `continuesMessageGroup` does: same author, at
/// most five minutes apart, same local day, and neither row a system,
/// workflow or sent-from-thread message. Nearby membership rows merge
/// first (`membershipRows`).
pub fn items(rows: &[Arc<MessageRow>]) -> Vec<Item> {
    let grouped = membership::group(rows);
    let mut items = Vec::with_capacity(grouped.len());
    let mut previous: Option<(&Arc<MessageRow>, u64)> = None;
    for entry in &grouped {
        let row = &entry.row;
        let at = entry.created_at;
        let new_day = previous.is_none_or(|(_, p)| !time::same_day(p, at));
        let continues = !new_day && previous.is_some_and(|(p, _)| continues(p, row));
        items.push(Item {
            row: row.clone(),
            head: !continues,
            day: new_day.then(|| time::day(at).into()),
            group: entry.group.clone(),
        });
        previous = Some((row, row.created_at));
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
    /// "Review report": shows the report dialog again.
    ReviewReport,
    CopyLink,
    CopyMessage,
    MarkRead,
    MarkUnread,
    SendToChannel,
    Edit,
    Delete,
    Follow(bool),
    Report,
    /// Reply in the thread: the thread root, with reply intent.
    Reply,
    /// Toggle the viewer's reaction, reusing a pill's custom emoji.
    React {
        content: String,
        emoji: Option<CustomEmoji>,
    },
    /// Retry a failed outbox operation (a reaction change).
    RetryOperation(EventId),
    /// A registered message action, by key.
    Action(String),
}

/// Which floating element of a row is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popup {
    /// The "More message actions" menu.
    More,
    /// The reaction picker, from the action bar or the reactions row.
    Picker { from_bar: bool },
    /// The workflow byline's details.
    WorkflowInfo,
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
    /// The keyboard highlight in the open more menu.
    pub highlight: Option<usize>,
    /// The reaction pill whose preview is open: `(message, index)`, and
    /// the pill it slid from.
    pub preview: Option<(EventId, usize, Option<usize>)>,
    /// The row holding keyboard focus, which shows its bar.
    pub focused: Option<EventId>,
}

impl Interaction {
    fn bar_visible(&self, id: EventId) -> bool {
        self.focused == Some(id)
            || self.hovered == Some(id)
            || self.bar_hovered == Some(id)
            || self
                .open
                .is_some_and(|(open, popup)| open == id && popup != Popup::WorkflowInfo)
    }
}

/// What the open more menu needs to know about its row, read when it
/// opens: whether the message is unread and its thread followed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuFacts {
    pub unread: bool,
    pub following: bool,
}

pub type RowHandler = Rc<dyn Fn(&Arc<MessageRow>, &mut Window, &mut App)>;
pub type HoverHandler = Rc<dyn Fn(EventId, Hover, bool, &mut Window, &mut App)>;
pub type PopupHandler = Rc<dyn Fn(Option<(EventId, Popup)>, &mut Window, &mut App)>;
pub type CommandHandler = Rc<dyn Fn(&Arc<MessageRow>, Command, &mut Window, &mut App)>;
pub type HighlightHandler = Rc<dyn Fn(Option<usize>, &mut Window, &mut App)>;
pub type PreviewHandler =
    Rc<dyn Fn(Option<(EventId, usize, Option<usize>)>, &mut Window, &mut App)>;
pub type WantProfiles = Rc<dyn Fn(Vec<String>, &mut App)>;

#[derive(Default, Clone)]
pub struct Actions {
    /// Opens the row's thread (the summary button).
    pub on_open_thread: Option<RowHandler>,
    pub on_retry: Option<RowHandler>,
    /// Without it rows show no action bar.
    pub on_hover: Option<HoverHandler>,
    pub on_popup: Option<PopupHandler>,
    pub on_command: Option<CommandHandler>,
    pub on_highlight: Option<HighlightHandler>,
    pub on_preview: Option<PreviewHandler>,
    pub on_want_profiles: Option<WantProfiles>,
}

/// What every row of a page shares.
pub struct Rows {
    pub viewer: String,
    pub directory: Directory,
    /// The page's conversation, for what the viewer may do in it.
    pub channel: Option<ChannelSummary>,
    pub actions: Actions,
    pub interaction: Interaction,
    /// The hover bar's three reactions (`quick-reactions.ts`).
    pub quick_reactions: Vec<QuickReaction>,
    /// The page's reaction picker; it reports picks to the page.
    pub picker: Option<Entity<EmojiPicker>>,
    pub ops: SharedOps,
    pub menu: MenuFacts,
    /// Observed presence, for agent avatars.
    pub presence: Arc<HashMap<String, PresenceState>>,
    /// Agents working (typing) in this conversation.
    pub working: Vec<String>,
    /// The rows sit in a thread panel.
    pub in_thread: bool,
    /// Focus for the open more menu's keyboard.
    pub menu_focus: Option<FocusHandle>,
    /// A report sent while its conversation was hidden: the message and
    /// the failure, `None` while it is still submitting.
    pub report: Option<(EventId, Option<SharedString>)>,
    /// A reaction error to show under a row.
    pub reaction_error: Option<(EventId, SharedString)>,
    /// Unread and agent activity of the rows shown.
    pub marks: Marks,
    /// Time chips and viewer opening (`media::RowMedia`).
    pub media: media::RowMedia,
}

/// Unread and agent activity the page follows for the rows it shows.
#[derive(Clone, Default)]
pub struct Marks {
    /// Thread roots' unread state, for their summaries' dot.
    pub threads: HashMap<EventId, ThreadUnread>,
    /// The viewer's own agents working in each thread, by root.
    pub working: HashMap<EventId, Vec<String>>,
}

impl Rows {
    /// Rows that only display: no actions, no picker.
    pub fn new(viewer: impl Into<String>, directory: Directory) -> Self {
        Self {
            viewer: viewer.into(),
            directory,
            channel: None,
            actions: Actions::default(),
            interaction: Interaction::default(),
            quick_reactions: Vec::new(),
            picker: None,
            ops: Arc::new(Ops::default()),
            menu: MenuFacts::default(),
            presence: Arc::default(),
            working: Vec::new(),
            in_thread: false,
            menu_focus: None,
            report: None,
            reaction_error: None,
            marks: Marks::default(),
            media: media::RowMedia::default(),
        }
    }

    fn names(&self) -> &Names {
        &self.directory.names
    }

    fn own(&self, row: &MessageRow) -> bool {
        row.author.to_hex() == self.viewer
    }

    fn popup(&self, value: Option<(EventId, Popup)>, window: &mut Window, cx: &mut App) {
        if let Some(on_popup) = &self.actions.on_popup {
            on_popup(value, window, cx);
        }
    }

    fn command(&self, row: &Arc<MessageRow>, command: Command, window: &mut Window, cx: &mut App) {
        if let Some(on_command) = &self.actions.on_command {
            on_command(row, command, window, cx);
        }
    }

    fn archived(&self) -> bool {
        self.channel.as_ref().is_some_and(|c| c.archived)
    }

    fn read_only(&self) -> bool {
        self.channel.as_ref().is_some_and(|c| c.read_only)
    }

    fn listed(&self) -> bool {
        self.channel.as_ref().is_some_and(|channel| {
            self.directory
                .channels
                .iter()
                .any(|listed| listed.id == channel.id)
        })
    }

    fn dm(&self) -> bool {
        self.channel
            .as_ref()
            .is_some_and(|c| c.channel_type == Some(ChannelType::Dm))
    }

    /// `canReact`: a listed, writable, unarchived conversation.
    fn can_react(&self) -> bool {
        self.actions.on_command.is_some() && self.listed() && !self.archived() && !self.read_only()
    }

    /// `useManagedMessage`: read state, edit and delete need a delivered,
    /// ordinary row in a writable channel.
    fn managed(&self, row: &MessageRow) -> bool {
        self.channel
            .as_ref()
            .is_some_and(|c| !c.read_only && self.listed())
            && row.membership.is_none()
            && row.diff.is_none()
            && delivered(row)
    }

    /// A delete authorized by the agent's attested owner, the viewer.
    pub fn owns_agent(&self, row: &MessageRow) -> bool {
        let author = row.author.to_hex();
        author != self.viewer
            && !self.archived()
            && self
                .directory
                .profiles
                .get(&author)
                .is_some_and(|p| p.is_agent && p.attested_owner.as_deref() == Some(&self.viewer))
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

fn presence_badge(state: Option<&PresenceState>) -> Option<Presence> {
    state.map(|state| match state {
        PresenceState::Online => Presence::Online,
        PresenceState::Away => Presence::Away,
        PresenceState::Offline => Presence::Offline,
    })
}

pub fn render_in(layout: Layout, item: &Item, rows: &Rc<Rows>, cx: &App) -> AnyElement {
    let c = *cx.colors();
    let (gutter, gap, avatar_size) = match layout {
        Layout::Timeline => (40., 12., AvatarSize::Large),
        Layout::Thread => (32., 8., AvatarSize::Default),
        Layout::Nested => (32., 8., AvatarSize::Small),
    };
    let row = &item.row;
    let names = rows.names();
    let profiles = &rows.directory.profiles;
    let author = row.author.to_hex();
    let profile = profiles.get(&author);
    let name: SharedString = names.display(&author).into();
    let agent = row.is_agent_envelope()
        || names.is_library_agent(&author)
        || profile.is_some_and(|p| p.is_agent);

    let mut container = div().flex().flex_col().w_full();
    if let Some(day) = &item.day {
        container = container.child(day_divider(day.clone(), cx));
    }

    if row.membership.is_some() {
        return container
            .child(membership::render(
                row,
                item.group.as_deref(),
                names,
                &rows.directory.media,
                cx,
            ))
            .into_any_element();
    }

    let id = row.id.to_hex();
    let bar_visible = rows.actions.on_hover.is_some() && rows.interaction.bar_visible(row.id);
    let left = if !item.head {
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
            .tooltip(time_tooltip(row.created_at))
            .child(time::compact(row.created_at))
            .into_any_element()
    } else if row.workflow_owner.is_some() {
        div()
            .flex_none()
            .w(px(gutter))
            .flex()
            .justify_center()
            .child(
                Avatar::new("Workflow")
                    .size(avatar_size)
                    .squircle()
                    .fallback_icon(IconName::Lightning, 20.),
            )
            .into_any_element()
    } else {
        let mut avatar = Avatar::new(name.clone()).size(avatar_size);
        if agent {
            avatar = avatar
                .squircle()
                .presence(presence_badge(rows.presence.get(&author)))
                .thinking(
                    SharedString::from(format!("thinking-{id}")),
                    rows.working.contains(&author),
                )
                .alt(name.clone());
        }
        if let Some(picture) = profile.and_then(|p| p.picture.as_deref()) {
            avatar = avatar.image(media::avatar(picture, &rows.directory.media));
        }
        let target = author.clone();
        div()
            .flex_none()
            .w(px(gutter))
            .flex()
            .justify_center()
            .child(
                div()
                    .id(SharedString::from(format!("avatar-{id}")))
                    .relative()
                    .rounded_full()
                    .cursor_pointer()
                    .child(avatar)
                    .on_click(move |event, window, cx| {
                        cx.stop_propagation();
                        navigation::show_profile(target.clone(), event.position(), window, cx)
                    }),
            )
            .into_any_element()
    };

    let mut body = div().flex().flex_col().flex_1().min_w_0();
    if item.head {
        body = body.child(byline(row, &name, rows, cx));
    }
    if let Some(sent) = &row.sent_from_thread {
        let label = match &sent.excerpt {
            Some(excerpt) => format!("Thread — {excerpt}"),
            None => "Thread".to_string(),
        };
        let url = sent_from_thread_link(&row.channel, &sent.root_id);
        body = body.child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .my(px(4.))
                .text_caption()
                .text_color(c.text_subtle)
                .child(div().flex_none().child("Sent from"))
                .child(div().flex_1().min_w_0().child(markdown::link_chip(
                    &url,
                    &label,
                    &rows.directory,
                    &format!("sent-{id}"),
                    cx,
                ))),
        );
    }
    // The body, after a time chip for timecoded video replies.
    if let Some(text) = media::text(row, rows, cx) {
        body = body.child(
            div()
                .id(SharedString::from(format!("md-{id}")))
                .when(item.head, |d| d.mt(px(4.)))
                .text_body_sm()
                .text_color(c.text_standard)
                .child(text),
        );
    }
    if let Some(notice) = delivery_notice(row, rows, cx) {
        body = body.child(notice);
    }
    if let Some((_, failure)) = rows.report.as_ref().filter(|(id, _)| *id == row.id) {
        body = body.child(report_state(row, failure.clone(), rows, cx));
    }
    if !row.attachments.is_empty() {
        body = body.child(media::attachments(row, rows, cx));
    }
    if let Some(reactions) = reactions(row, rows, cx) {
        body = body.child(reactions);
    }
    // Inside a thread the replies are already on screen. A root with no
    // replies yet still links to its thread while an agent works there.
    let working = rows.marks.working.get(&row.id).filter(|w| !w.is_empty());
    if layout == Layout::Timeline
        && rows.actions.on_open_thread.is_some()
        && (row.reply_count > 0 || (row.thread_root_id.is_none() && working.is_some()))
    {
        body = body.child(thread_summary(row, rows, cx));
    }

    let width = markdown::measured("row-width", &id);
    // The row's tab stop has no size, so a click on the row never takes
    // focus from where it was.
    let stop = div()
        .absolute()
        .size_0()
        .track_focus(&row_focus(rows, row.id, cx));
    let mut line = div()
        .id(SharedString::from(format!("message-{id}")))
        .when(rows.interaction.focused == Some(row.id), |d| {
            d.bg(c.affordance_subtle)
        })
        .child(stop)
        .relative()
        .flex()
        .gap(px(gap))
        .pt(if item.head { px(14.) } else { px(2.) })
        .pb(px(2.))
        .child(measure_width(width.clone()))
        .child(left)
        .child(body);
    if let Some(on_hover) = rows.actions.on_hover.clone() {
        let target = row.id;
        line = line.on_hover(move |hovered, window, cx| {
            on_hover(target, Hover::Row, *hovered, window, cx)
        });
    }
    if bar_visible {
        line = line.child(action_bar(item, rows, width.get(), cx));
    }
    container.child(line).into_any_element()
}

/// `buzz://message?channel&id=root&thread=root`.
fn sent_from_thread_link(channel: &str, root: &EventId) -> String {
    let mut url = url::Url::parse("buzz://message").expect("static URL");
    url.query_pairs_mut()
        .append_pair("channel", channel)
        .append_pair("id", &root.to_hex())
        .append_pair("thread", &root.to_hex());
    url.to_string()
}

/// A 500ms full date-and-time tooltip.
fn time_tooltip(seconds: u64) -> impl Fn(&mut Window, &mut App) -> AnyView {
    Tooltip::text(time::full(seconds))
}

/// The name (or workflow byline) and time over a group's first row.
fn byline(row: &Arc<MessageRow>, name: &SharedString, rows: &Rc<Rows>, cx: &App) -> AnyElement {
    let c = *cx.colors();
    let id = row.id.to_hex();
    let mut line = div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap(px(8.))
        .text_body_sm();
    match row.workflow_owner {
        Some(owner) => {
            let owner_hex = owner.to_hex();
            let fallback: String = owner_hex.chars().take(10).collect::<String>() + "…";
            let owner_name = rows.names().name(&owner_hex, &fallback);
            let target = owner_hex.clone();
            line = line
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c.text_standard)
                        .child("Workflow"),
                )
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(4.))
                        .min_w_0()
                        .text_color(c.text_subtle)
                        .child("· owned by")
                        .child(
                            div()
                                .id(SharedString::from(format!("workflow-owner-{id}")))
                                .text_color(c.text_link)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .child(owner_name.clone())
                                .on_click(move |event, window, cx| {
                                    navigation::show_profile(
                                        target.clone(),
                                        event.position(),
                                        window,
                                        cx,
                                    )
                                }),
                        ),
                )
                .child(workflow_info(row, &owner_hex, &owner_name, rows, cx));
        }
        None => {
            line = line.child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c.text_standard)
                    .min_w_0()
                    .child(name.clone()),
            );
        }
    }
    line.child(
        div()
            .id(SharedString::from(format!("byline-{id}")))
            .text_caption()
            .text_color(c.text_subtle)
            .tooltip(time_tooltip(row.created_at))
            .child(time::byline(row.created_at)),
    )
    .children(renderers::markers(row, cx))
    .into_any_element()
}

/// The workflow info button and its popover (`WorkflowByline.tsx`).
fn workflow_info(
    row: &Arc<MessageRow>,
    owner: &str,
    owner_name: &str,
    rows: &Rc<Rows>,
    cx: &App,
) -> AnyElement {
    let c = *cx.colors();
    let open = rows.interaction.open == Some((row.id, Popup::WorkflowInfo));
    let (toggle, target) = (rows.clone(), row.id);
    let button = IconButton::new(
        SharedString::from(format!("workflow-info-{}", row.id.to_hex())),
        IconName::Info,
    )
    .size(IconButtonSize::Xs)
    .selected(open)
    .tooltip("Workflow message details")
    .on_click(move |_, window, cx| {
        let next = (!open).then_some((target, Popup::WorkflowInfo));
        toggle.popup(next, window, cx)
    });
    let signer = row.signer.unwrap_or(row.author).to_hex();
    let detail = |term: &str, value: String| {
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .mt(px(8.))
                    .text_color(c.text_subtle)
                    .child(term.to_string()),
            )
            .child(div().child(value))
    };
    let close = rows.clone();
    div()
        .relative()
        .child(button)
        .when(open, |d| {
            d.child(
                div().absolute().left_0().bottom_0().child(deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .offset(point(px(0.), px(4.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .occlude()
                                .on_mouse_down_out(move |_, window, cx| {
                                    close.popup(None, window, cx)
                                })
                                .w(px(288.))
                                .p(px(12.))
                                .floating_surface(&c)
                                .rounded(px(16.))
                                .text_body_sm()
                                .text_color(c.text_standard)
                                .child(div().text_label_sm().child("Workflow message"))
                                .child(div().mt(px(4.)).text_color(c.text_subtle).child(
                                    "Sent automatically. This community’s relay attributes \
                                     the workflow to this owner; the owner did not sign this \
                                     message.",
                                ))
                                .child(
                                    div()
                                        .my(px(12.))
                                        .child(detail("Workflow owner", owner_name.to_string()))
                                        .child(detail("Owner public key", npub(owner)))
                                        .child(detail("Signed by the relay", npub(&signer))),
                                )
                                .child(div().text_color(c.text_subtle).child(
                                    "Workflow and run details are not included in this message.",
                                )),
                        ),
                )),
            )
        })
        .into_any_element()
}

/// Keyboard focus for a row and its action bar's buttons, kept by message
/// across frames: rows render from `&App`, so the page asks which row
/// holds focus (`keyboard_row`). Channel and thread rows are apart. Rows
/// are tab stops in the port so the keyboard can reach the bar, which
/// buzz-app reaches through each row's own controls.
#[derive(Default)]
struct RowFocus {
    row: Option<FocusHandle>,
    buttons: HashMap<String, FocusHandle>,
}

thread_local! {
    static ROW_FOCUS: std::cell::RefCell<HashMap<(bool, EventId), RowFocus>> =
        std::cell::RefCell::new(HashMap::new());
}

fn with_focus<T>(key: (bool, EventId), f: impl FnOnce(&mut RowFocus) -> T) -> T {
    ROW_FOCUS.with_borrow_mut(|rows| {
        if rows.len() > 2048 {
            rows.clear();
        }
        f(rows.entry(key).or_default())
    })
}

/// A row is a tab stop, so the keyboard can reach its action bar.
fn row_focus(rows: &Rows, id: EventId, cx: &App) -> FocusHandle {
    with_focus((rows.in_thread, id), |focus| {
        focus
            .row
            .get_or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    })
}

/// The bar button `name` of row `id`.
fn bar_focus(rows: &Rows, id: EventId, name: &str, cx: &App) -> FocusHandle {
    with_focus((rows.in_thread, id), |focus| {
        focus
            .buttons
            .entry(name.to_string())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    })
}

/// The row the keyboard is in, whose bar shows (`useFloatingActionBar`).
pub fn keyboard_row(in_thread: bool, window: &Window) -> Option<EventId> {
    if !window.last_input_was_keyboard() {
        return None;
    }
    ROW_FOCUS.with_borrow(|rows| {
        rows.iter()
            .filter(|((thread, _), _)| *thread == in_thread)
            .find(|(_, focus)| {
                focus
                    .row
                    .iter()
                    .chain(focus.buttons.values())
                    .any(|handle| handle.is_focused(window))
            })
            .map(|((_, id), _)| *id)
    })
}

/// Gives focus back to a row's bar button as its popup closes from the
/// keyboard (Base UI's `finalFocus`).
pub fn focus_bar_button(
    in_thread: bool,
    id: EventId,
    name: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = ROW_FOCUS.with_borrow(|rows| {
        rows.get(&(in_thread, id))
            .and_then(|focus| focus.buttons.get(name).cloned())
    });
    if let Some(handle) = handle {
        handle.focus(window, cx);
    }
}

/// The row's laid-out width, measured each frame; only crossing the quick
/// reactions' minimum redraws.
fn measure_width(width: Rc<Cell<f32>>) -> impl IntoElement {
    markdown::measure(
        width,
        |bounds| bounds.size.width.into(),
        |before, now| (before <= QUICK_REACTIONS_MIN_WIDTH) != (now <= QUICK_REACTIONS_MIN_WIDTH),
    )
}

/// Links, reactions and edits need the relay to have the message.
pub fn delivered(row: &MessageRow) -> bool {
    row.delivery
        .as_ref()
        .is_none_or(|d| matches!(d.status, Delivery::Accepted | Delivery::Seen))
}

/// The hover action bar (§4.5): quick reactions, add reaction, reply, copy
/// link and the more menu. Its bottom sits 4px under the byline, so a
/// continuation row's bar floats over the row above.
fn action_bar(item: &Item, rows: &Rc<Rows>, width: f32, cx: &App) -> impl IntoElement {
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
            .track_focus(&bar_focus(rows, row.id, name, cx))
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
    if rows.can_react() {
        let busy = !delivered || rows.ops.reaction_blocked(row);
        if width > QUICK_REACTIONS_MIN_WIDTH {
            for (index, quick) in rows.quick_reactions.iter().enumerate() {
                let content = quick.content.to_string();
                let custom = quick.image.as_ref().map(|url| CustomEmoji {
                    shortcode: content.trim_matches(':').to_lowercase(),
                    url: url.clone(),
                });
                let mine = row.reactions.iter().any(|group| {
                    group.content == content
                        && group.emoji.as_ref().map(|e| &e.url) == custom.as_ref().map(|e| &e.url)
                        && group
                            .events
                            .iter()
                            .any(|r| r.author.to_hex() == rows.viewer)
                });
                let glyph = match &quick.image {
                    Some(url) => img(media::image(url, &rows.directory.media))
                        .size_full()
                        .object_fit(ObjectFit::Contain)
                        .into_any_element(),
                    None => div().child(content.clone()).into_any_element(),
                };
                let (rows2, target) = (rows.clone(), row.clone());
                let group = SharedString::from(format!("quick-{index}-{id}"));
                let (hover, down, up) = (group.clone(), group.clone(), group.clone());
                bar = bar.child(
                    div()
                        .id(SharedString::from(format!("{group}-pointer")))
                        .on_hover(move |hovered, window, _| {
                            // Leaving ends a press as the pointer lets go.
                            let down = (!*hovered).then_some(false);
                            set_pose(&hover, Some(*hovered), down, window)
                        })
                        .on_mouse_down(MouseButton::Left, move |_, window, _| {
                            set_pose(&down, None, Some(true), window)
                        })
                        .on_mouse_up(MouseButton::Left, move |_, window, _| {
                            set_pose(&up, None, Some(false), window)
                        })
                        .child(
                            IconButton::new(group.clone(), IconName::Smiley)
                                .size(IconButtonSize::Sm)
                                .surface(Surface::Floating)
                                .track_focus(&bar_focus(
                                    rows,
                                    row.id,
                                    &format!("quick-{index}"),
                                    cx,
                                ))
                                .disabled(busy)
                                .selected(mine)
                                .child(lift(glyph, group, busy, cx))
                                .tooltip(format!(
                                    "{} {content}",
                                    if mine { "Remove" } else { "React with" }
                                ))
                                .on_click(move |_, window, cx| {
                                    rows2.command(
                                        &target,
                                        Command::React {
                                            content: content.clone(),
                                            emoji: custom.clone(),
                                        },
                                        window,
                                        cx,
                                    )
                                }),
                        ),
                );
            }
        }
        let (rows, target) = (rows.clone(), row.id);
        bar = bar.child(
            button("add-reaction", IconName::SmileyPlus)
                .shape(IconButtonShape::Round)
                .disabled(busy)
                .selected(open == Some(Popup::Picker { from_bar: true }))
                .tooltip("Add reaction")
                .on_click(move |_, window, cx| {
                    rows.popup(Some((target, Popup::Picker { from_bar: true })), window, cx)
                }),
        );
    }
    if rows.actions.on_command.is_some() {
        let unavailable = !delivered
            || rows.archived()
            || (rows.channel.is_some() && (!rows.listed() || rows.read_only()));
        let (rows2, target) = (rows.clone(), row.clone());
        bar = bar.child(
            button("reply", IconName::ChatCircle)
                .disabled(unavailable)
                .tooltip(if unavailable {
                    "Reply unavailable for this message"
                } else {
                    "Reply"
                })
                .on_click(move |_, window, cx| rows2.command(&target, Command::Reply, window, cx)),
        );
        let (rows2, target) = (rows.clone(), row.clone());
        bar = bar.child(
            button("copy-link", IconName::Link)
                .disabled(!delivered)
                .tooltip(if delivered {
                    "Copy link"
                } else {
                    "Message link unavailable"
                })
                .on_click(move |_, window, cx| {
                    rows2.command(&target, Command::CopyLink, window, cx)
                }),
        );
    }
    {
        let (rows2, target) = (rows.clone(), row.id);
        bar = bar.child(
            button("more", IconName::DotsThree)
                .selected(open == Some(Popup::More))
                .tooltip("More message actions")
                .on_click(move |_, window, cx| {
                    rows2.popup(Some((target, Popup::More)), window, cx)
                }),
        );
    }
    let popup = match open {
        Some(Popup::More) => Some(more_menu(row, rows, cx)),
        Some(Popup::Picker { from_bar: true }) => Some(picker(rows)),
        _ => None,
    };
    // Menus drop below the bar's end; the reaction picker opens above it,
    // 6px off, and below it when there is no room above.
    let top = bar_top(&id);
    let above = matches!(open, Some(Popup::Picker { .. })) && top.get() > PICKER_ROOM;
    bar = bar.child(measure_top(top));
    bar.children(popup.map(|popup| {
        let anchor = div().absolute().right_0();
        let anchor = if above {
            anchor.top_0()
        } else {
            anchor.bottom_0()
        };
        anchor.child(deferred(
            anchored()
                .anchor(if above {
                    Anchor::BottomRight
                } else {
                    Anchor::TopRight
                })
                .offset(point(px(0.), px(if above { -6. } else { 4. })))
                .snap_to_window_with_margin(px(8.))
                .child(popup),
        ))
    }))
}

/// Room the reaction picker needs above its trigger.
const PICKER_ROOM: f32 = 380.;

/// The window y of a row's action bar, measured each frame; only crossing
/// the picker's room redraws.
fn bar_top(id: &str) -> Rc<Cell<f32>> {
    markdown::measured("bar-top", id)
}

fn measure_top(top: Rc<Cell<f32>>) -> impl IntoElement {
    markdown::measure(
        top,
        |bounds| bounds.origin.y.into(),
        |before, now| (before > PICKER_ROOM) != (now > PICKER_ROOM),
    )
}

/// A quick reaction's pointer state, which its glyph eases toward.
#[derive(Clone, Copy, Default, PartialEq)]
enum Press {
    #[default]
    Rest,
    Hover,
    Down,
}

impl Press {
    /// `(scale, rise)`: lifted 1px and 12% larger under the pointer,
    /// 96% while pressed.
    fn pose(self) -> (f32, f32) {
        match self {
            Press::Rest => (1., 0.),
            Press::Hover => (1.12, 1.),
            Press::Down => (0.96, 0.),
        }
    }
}

/// Each quick reaction's pose: where its glyph eases from and to, and
/// which change it is, so a new change starts a new 120ms ease.
#[derive(Clone, Copy)]
struct Pose {
    from: (f32, f32),
    to: Press,
    hovered: bool,
    down: bool,
    change: u64,
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            from: Press::Rest.pose(),
            to: Press::Rest,
            hovered: false,
            down: false,
            change: 0,
        }
    }
}

thread_local! {
    static POSES: std::cell::RefCell<HashMap<SharedString, Pose>> =
        std::cell::RefCell::new(HashMap::new());
}

fn pose(group: &SharedString) -> Pose {
    POSES.with_borrow(|poses| poses.get(group).copied().unwrap_or_default())
}

/// Eases a glyph toward its new pointer state, from its last target.
fn set_pose(group: &SharedString, hovered: Option<bool>, down: Option<bool>, window: &mut Window) {
    POSES.with_borrow_mut(|poses| {
        if poses.len() > 256 {
            poses.clear();
        }
        let pose = poses.entry(group.clone()).or_default();
        if let Some(hovered) = hovered {
            pose.hovered = hovered;
        }
        if let Some(down) = down {
            pose.down = down;
        }
        let to = match (pose.hovered, pose.down) {
            (true, true) => Press::Down,
            (true, false) => Press::Hover,
            (false, _) => Press::Rest,
        };
        if to != pose.to {
            pose.from = pose.to.pose();
            pose.to = to;
            pose.change += 1;
            window.refresh();
        }
    });
}

/// A quick reaction's glyph: lifts 1px and grows 12% under the pointer,
/// presses to 96%, easing over 120ms. Reduced motion keeps it still.
fn lift(glyph: AnyElement, group: SharedString, disabled: bool, cx: &App) -> AnyElement {
    let base = div()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .size(px(16.))
        .text_size(px(14.))
        .child(glyph);
    if disabled || cx.reduce_motion() {
        return base.into_any_element();
    }
    let pose = pose(&group);
    let (from, to) = (pose.from, pose.to.pose());
    base.with_animation(
        ElementId::Name(format!("{group}-pose-{}", pose.change).into()),
        Animation::new(Duration::from_millis(120)).with_easing(|t| {
            buzz_ui::tokens::easing::cubic_bezier(buzz_ui::tokens::easing::ENTER, t)
        }),
        move |glyph, t| {
            let scale = from.0 + (to.0 - from.0) * t;
            let rise = from.1 + (to.1 - from.1) * t;
            glyph
                .top(px(-rise))
                .size(px(16. * scale))
                .text_size(px(14. * scale))
        },
    )
    .into_any_element()
}

/// One more-menu row: its command, label, icon and whether it is off.
struct Entry {
    name: String,
    label: String,
    icon: IconName,
    icon_size: f32,
    command: Command,
    disabled: bool,
}

/// `MessageActionBar`'s menu, in its order: read state, Copy message,
/// Send to channel, Edit, Delete, Follow, then Report.
fn menu_entries(row: &MessageRow, rows: &Rows) -> Vec<Entry> {
    let mut entries = Vec::new();
    let managed = rows.managed(row) && rows.actions.on_command.is_some();
    let delivered = delivered(row);
    if managed {
        let unread = rows.menu.unread;
        entries.push(Entry {
            name: "read-state".into(),
            label: if unread { "Mark read" } else { "Mark unread" }.into(),
            icon: if unread {
                IconName::EnvelopeOpen
            } else {
                IconName::Envelope
            },
            icon_size: 16.,
            command: if unread {
                Command::MarkRead
            } else {
                Command::MarkUnread
            },
            disabled: false,
        });
    }
    entries.push(Entry {
        name: "copy".into(),
        label: "Copy message".into(),
        icon: IconName::Copy,
        icon_size: 16.,
        command: Command::CopyMessage,
        disabled: false,
    });
    let writable = rows.listed() && !rows.archived() && !rows.read_only();
    if writable
        && row.thread_root_id.is_some()
        && rows.own(row)
        && !row.is_agent_envelope()
        && row.diff.is_none()
        && delivered
    {
        entries.push(Entry {
            name: "send-to-channel".into(),
            label: "Send to channel".into(),
            icon: IconName::HashArrowIn,
            icon_size: 16.,
            command: Command::SendToChannel,
            disabled: false,
        });
    }
    if managed {
        let own = rows.own(row) && !rows.archived();
        let busy = rows.ops.changing(row.id);
        if own && editable(row) {
            entries.push(Entry {
                name: "edit".into(),
                label: "Edit message".into(),
                icon: IconName::PencilSimple,
                icon_size: 14.,
                command: Command::Edit,
                disabled: busy || rows.ops.deleting(row.id),
            });
        }
        if own || rows.owns_agent(row) {
            entries.push(Entry {
                name: "delete".into(),
                label: "Delete message".into(),
                icon: IconName::Trash,
                icon_size: 14.,
                command: Command::Delete,
                disabled: busy || rows.ops.editing(row.id),
            });
        }
        if !rows.dm() {
            let following = rows.menu.following;
            entries.push(Entry {
                name: "follow".into(),
                label: if following {
                    "Unfollow thread".into()
                } else {
                    "Follow thread".into()
                },
                icon: if following {
                    IconName::BellSlash
                } else {
                    IconName::Bell
                },
                icon_size: 16.,
                command: Command::Follow(!following),
                disabled: false,
            });
        }
    }
    if rows.actions.on_command.is_some() {
        for action in renderers::actions(row) {
            entries.push(Entry {
                name: format!("action-{}", action.key()),
                label: action.title().to_string(),
                icon: action.icon().unwrap_or(IconName::DotsThree),
                icon_size: 16.,
                command: Command::Action(action.key().to_string()),
                disabled: false,
            });
        }
    }
    if row.membership.is_none() && delivered && rows.actions.on_command.is_some() {
        entries.push(Entry {
            name: "report".into(),
            label: "Report".into(),
            icon: IconName::Flag,
            icon_size: 16.,
            command: Command::Report,
            disabled: false,
        });
    }
    entries
}

/// `lastEditableMessage`'s row checks: an ordinary, delivered message.
pub fn editable(row: &MessageRow) -> bool {
    row.membership.is_none() && !row.is_agent_envelope() && row.diff.is_none() && delivered(row)
}

fn more_menu(row: &Arc<MessageRow>, rows: &Rc<Rows>, _cx: &App) -> AnyElement {
    let id = row.id.to_hex();
    let entries = Rc::new(menu_entries(row, rows));
    let highlight = rows.interaction.highlight;
    let mut menu = Menu::new();
    for (index, entry) in entries.iter().enumerate() {
        let (rows2, target, command) = (rows.clone(), row.clone(), entry.command.clone());
        menu = menu.child(
            MenuItem::new(
                SharedString::from(format!("{}-{id}", entry.name)),
                entry.label.clone(),
            )
            .icon(entry.icon)
            .icon_size(entry.icon_size)
            .disabled(entry.disabled)
            .highlighted(highlight == Some(index))
            .on_click(move |_, window, cx| {
                rows2.popup(None, window, cx);
                rows2.command(&target, command.clone(), window, cx);
            }),
        );
    }
    let close = rows.clone();
    let keys = rows.clone();
    let target = row.clone();
    div()
        .id(SharedString::from(format!("more-menu-{id}")))
        .occlude()
        .when_some(rows.menu_focus.as_ref(), |d, focus| d.track_focus(focus))
        .on_mouse_down_out(move |_, window, cx| close.popup(None, window, cx))
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            let count = entries.len();
            let enabled = |i: usize| entries.get(i).is_some_and(|e| !e.disabled);
            let step = |from: Option<usize>, forward: bool| -> Option<usize> {
                let mut index = from;
                for _ in 0..count {
                    let next = match (index, forward) {
                        (None, true) => 0,
                        (None, false) => count - 1,
                        (Some(i), true) => (i + 1) % count,
                        (Some(i), false) => (i + count - 1) % count,
                    };
                    if enabled(next) {
                        return Some(next);
                    }
                    index = Some(next);
                }
                None
            };
            let highlight = keys.interaction.highlight;
            let set = |value: Option<usize>, window: &mut Window, cx: &mut App| {
                if let Some(on_highlight) = &keys.actions.on_highlight {
                    on_highlight(value, window, cx);
                }
            };
            let (in_thread, more) = (keys.in_thread, target.id);
            let back = move |window: &mut Window, cx: &mut App| {
                focus_bar_button(in_thread, more, "more", window, cx)
            };
            match event.keystroke.key.as_str() {
                "escape" | "tab" => {
                    cx.stop_propagation();
                    keys.popup(None, window, cx);
                    back(window, cx);
                }
                "down" if count > 0 => {
                    cx.stop_propagation();
                    set(step(highlight, true), window, cx);
                }
                "up" if count > 0 => {
                    cx.stop_propagation();
                    set(step(highlight, false), window, cx);
                }
                "home" if count > 0 => set(step(None, true), window, cx),
                "end" if count > 0 => set(step(None, false), window, cx),
                "enter" | "space" => {
                    if let Some(entry) = highlight
                        .and_then(|i| entries.get(i))
                        .filter(|e| !e.disabled)
                    {
                        cx.stop_propagation();
                        let command = entry.command.clone();
                        keys.popup(None, window, cx);
                        back(window, cx);
                        keys.command(&target, command, window, cx);
                    }
                }
                _ => {}
            }
        })
        .child(menu)
        .into_any_element()
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

/// `reaction-name.ts`: an emoji's `:shortcode:`, else itself.
pub fn reaction_name(group: &ReactionGroup) -> String {
    if let Some(custom) = &group.emoji {
        return format!(":{}:", custom.shortcode);
    }
    emojis::get(&group.content)
        .and_then(|emoji| emoji.shortcode())
        .map(|code| format!(":{code}:"))
        .unwrap_or_else(|| group.content.clone())
}

/// "You" first, then the other reactors in event order; unknown names
/// are the first ten hex characters.
pub fn reactor_names(group: &ReactionGroup, viewer: &str, names: &Names) -> (Vec<String>, bool) {
    let mut authors: Vec<String> = Vec::new();
    for event in &group.events {
        let author = event.author.to_hex();
        if !authors.contains(&author) {
            authors.push(author);
        }
    }
    let mine = authors.iter().any(|a| a == viewer);
    let mut out: Vec<String> = Vec::new();
    if mine {
        out.push("You".into());
    }
    out.extend(
        authors
            .iter()
            .filter(|a| *a != viewer)
            .map(|a| names.display(a)),
    );
    (out, mine)
}

fn reaction_glyph(group: &ReactionGroup, size: f32, rows: &Rows, cx: &App) -> AnyElement {
    if let Some(custom) = &group.emoji
        && let Some(element) = renderers::inline(
            &format!(":{}:", custom.shortcode),
            std::slice::from_ref(custom),
            &rows.directory.media,
            cx,
        )
    {
        return element;
    }
    match &group.emoji {
        Some(custom) => {
            let fallback = format!(":{}:", custom.shortcode);
            img(media::image(&custom.url, &rows.directory.media))
                .size(px(size))
                .object_fit(ObjectFit::Contain)
                .with_fallback(move || {
                    div()
                        .max_w(px(96.))
                        .text_size(px(12.))
                        .line_height(px(12.))
                        .truncate()
                        .child(fallback.clone())
                        .into_any_element()
                })
                .into_any_element()
        }
        None => div()
            .text_size(px(14.))
            .line_height(px(18.))
            .min_w(px(18.))
            .max_w(px(96.))
            .truncate()
            .flex()
            .justify_center()
            .child(group.content.clone())
            .into_any_element(),
    }
}

/// The count, rolling up or down over 260ms when it changes
/// (`AnimatedReactionCount.tsx`).
fn reaction_count(key: SharedString, value: usize) -> AnyElement {
    thread_local! {
        static SHOWN: std::cell::RefCell<HashMap<SharedString, (usize, usize, u64)>> =
            std::cell::RefCell::new(HashMap::new());
    }
    let (from, version) = SHOWN.with_borrow_mut(|shown| {
        if shown.len() > 4096 {
            shown.clear();
        }
        let entry = shown.entry(key.clone()).or_insert((value, value, 0));
        if entry.1 != value {
            *entry = (entry.1, value, entry.2 + 1);
        }
        (entry.0, entry.2)
    });
    let text = |n: usize| {
        div()
            .h(px(14.))
            .text_size(px(12.))
            .line_height(px(14.))
            .child(n.to_string())
    };
    if from == value {
        return text(value).into_any_element();
    }
    let up = value > from;
    let (top, bottom) = if up { (from, value) } else { (value, from) };
    let settle = key.clone();
    div()
        .h(px(14.))
        .overflow_hidden()
        .child(
            div()
                .flex()
                .flex_col()
                .child(text(top))
                .child(text(bottom))
                .with_animation(
                    SharedString::from(format!("{key}-roll-{version}")),
                    Animation::new(Duration::from_millis(260)).with_easing(ease_out_quint),
                    move |column, delta| {
                        if delta >= 1. {
                            SHOWN.with_borrow_mut(|shown| {
                                if let Some(entry) = shown.get_mut(&settle) {
                                    entry.0 = entry.1;
                                }
                            });
                        }
                        let progress = if up { delta } else { 1. - delta };
                        column.mt(px(-14. * progress))
                    },
                ),
        )
        .into_any_element()
}

/// `cubic-bezier(0.23, 1, 0.32, 1)`, near enough.
fn ease_out_quint(t: f32) -> f32 {
    1. - (1. - t).powi(5)
}

fn reactions(row: &Arc<MessageRow>, rows: &Rc<Rows>, cx: &App) -> Option<AnyElement> {
    let c = *cx.colors();
    let id = row.id.to_hex();
    let failure = rows.ops.reaction_failure(row);
    let error = rows
        .reaction_error
        .as_ref()
        .filter(|(target, _)| *target == row.id)
        .map(|(_, error)| error.clone());
    if row.reactions.is_empty() && failure.is_none() && error.is_none() {
        return None;
    }
    let can = rows.can_react() && delivered(row);
    let unavailable = rows.ops.reaction_blocked(row);
    let mut pills = div()
        .id(SharedString::from(format!("reactions-{id}")))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.))
        .mt(px(6.));
    if let Some(on_preview) = rows.actions.on_preview.clone() {
        let target = row.id;
        let open = rows.interaction.preview.filter(|(id, _, _)| *id == target);
        pills = pills.on_hover(move |hovered, window, cx| {
            if !hovered && open.is_some() {
                on_preview(None, window, cx)
            }
        });
    }
    for (index, group) in row.reactions.iter().enumerate() {
        let (names, mine) = reactor_names(group, &rows.viewer, rows.names());
        let people = names.len();
        let glyph = reaction_glyph(group, 14., rows, cx);
        // By the group, not its place: a count stays with its emoji when
        // groups come and go.
        let emoji = group.emoji.as_ref().map_or("", |emoji| emoji.url.as_str());
        let key = SharedString::from(format!("reaction-{id}-{}-{emoji}", group.content));
        let mut pill = div()
            .id(key.clone())
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
            .child(glyph)
            .child(reaction_count(key.clone(), people));
        pill = if mine {
            pill.border_color(mix(c.affordance_prominent, c.border_standard, 0.4))
                .bg(mix(c.affordance_prominent, c.surface_panel, 0.12))
                .text_color(c.affordance_prominent)
        } else {
            pill.border_color(c.border_standard)
                .bg(c.affordance_subtle)
                .text_color(c.text_standard)
                .when(can, |p| {
                    p.hover(|style| style.bg(c.affordance_subtle_hover))
                })
        };
        if can {
            pill = pill.cursor_pointer();
            if !unavailable {
                let (rows2, target) = (rows.clone(), row.clone());
                let (content, emoji) = (group.content.clone(), group.emoji.clone());
                pill = pill.on_click(move |_, window, cx| {
                    rows2.command(
                        &target,
                        Command::React {
                            content: content.clone(),
                            emoji: emoji.clone(),
                        },
                        window,
                        cx,
                    )
                });
            }
        }
        // The preview card: 100ms to open, then instant along the row,
        // sliding in from the pill it left.
        let open = rows.interaction.preview;
        let shown = open.is_some_and(|(m, i, _)| m == row.id && i == index);
        if let Some(on_preview) = rows.actions.on_preview.clone() {
            let target = row.id;
            let want = rows.actions.on_want_profiles.clone();
            let authors: Vec<String> = group.events.iter().map(|e| e.author.to_hex()).collect();
            pill = pill.on_hover(move |hovered, window, cx| {
                if *hovered {
                    let from = open.filter(|(m, _, _)| *m == target).map(|(_, i, _)| i);
                    let delay = open.is_none_or(|(m, _, _)| m != target);
                    // Reactors' names load as the card opens.
                    if let Some(want) = &want {
                        want(authors.clone(), cx);
                    }
                    if delay {
                        let on_preview = on_preview.clone();
                        window
                            .spawn(cx, async move |cx| {
                                cx.background_executor()
                                    .timer(Duration::from_millis(100))
                                    .await;
                                let _ = cx.update(|window, cx| {
                                    on_preview(Some((target, index, None)), window, cx)
                                });
                            })
                            .detach();
                    } else {
                        on_preview(Some((target, index, from)), window, cx)
                    }
                }
            });
        }
        let preview = shown.then(|| {
            let slide = open.and_then(|(_, i, from)| from.map(|f| (i as f32 - f as f32).signum()));
            reaction_preview(group, names.join(", "), slide, rows, &c, cx)
        });
        pills = pills.child(div().relative().child(pill).children(preview.map(|card| {
            div().absolute().left_0().top_0().child(deferred(
                anchored()
                    .anchor(Anchor::BottomLeft)
                    .offset(point(px(0.), px(-6.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            ))
        })));
    }
    if can && !row.reactions.is_empty() {
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
            .map(|d| {
                if open {
                    d.border_color(c.border_standard)
                        .bg(c.affordance_subtle)
                        .text_color(c.text_standard)
                } else {
                    d.border_color(transparent_black())
                        .text_color(c.text_subtle)
                        .when(!unavailable, |d| {
                            d.hover(|s| {
                                s.border_color(c.border_standard)
                                    .bg(c.affordance_subtle)
                                    .text_color(c.text_standard)
                            })
                        })
                }
            })
            .when(!unavailable, |d| d.cursor_pointer())
            .tooltip(Tooltip::text("Add reaction"))
            .child(Icon::new(IconName::SmileyPlus).size(16.))
            .when(!unavailable, |d| {
                d.on_click(move |_, window, cx| {
                    popup_rows.popup(
                        Some((target, Popup::Picker { from_bar: false })),
                        window,
                        cx,
                    )
                })
            });
        let top = bar_top(&format!("react-more-{id}"));
        let above = top.get() > PICKER_ROOM;
        pills = pills.child(div().relative().child(measure_top(top)).child(button).when(
            open,
            |d| {
                let anchor = div().absolute().left_0();
                let anchor = if above {
                    anchor.top_0()
                } else {
                    anchor.bottom_0()
                };
                d.child(
                    anchor.child(deferred(
                        anchored()
                            .anchor(if above {
                                Anchor::BottomLeft
                            } else {
                                Anchor::TopLeft
                            })
                            .offset(point(px(0.), px(if above { -6. } else { 6. })))
                            .snap_to_window_with_margin(px(8.))
                            .child(picker(rows)),
                    )),
                )
            },
        ));
    }
    if let Some(error) = error {
        pills = pills.child(div().text_caption().text_color(c.text_danger).child(error));
    }
    if let Some(op) = failure {
        let (rows2, target, op_id) = (rows.clone(), row.clone(), op.id);
        pills = pills.child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_caption()
                .text_color(c.text_subtle)
                .child(if op.delivery == buzz_relay_client::Delivery::Failed {
                    "Couldn’t update reaction."
                } else {
                    "Reaction delivery not confirmed."
                })
                .child(
                    buzz_ui::Button::new(
                        SharedString::from(format!("retry-reaction-{id}")),
                        "Retry reaction",
                    )
                    .variant(buzz_ui::ButtonVariant::Ghost)
                    .size(buzz_ui::ButtonSize::Sm)
                    .on_click(move |_, window, cx| {
                        rows2.command(&target, Command::RetryOperation(op_id), window, cx)
                    }),
                ),
        );
    }
    Some(pills.into_any_element())
}

/// The reaction preview card: the glyph and its `:name:`, then who
/// reacted (`ReactionPill`'s `PreviewCard`).
fn reaction_preview(
    group: &ReactionGroup,
    names: String,
    slide: Option<f32>,
    rows: &Rows,
    c: &buzz_ui::Colors,
    cx: &App,
) -> AnyElement {
    let card = div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .px(px(12.))
        .py(px(8.))
        .floating_surface(c)
        .rounded(px(12.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(16.))
                .child(reaction_glyph(group, 14., rows, cx))
                .child(
                    div()
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child(reaction_name(group)),
                ),
        )
        .child(
            div()
                .max_w(px(224.))
                .text_label_sm()
                .text_color(c.text_standard)
                .child(names),
        );
    match slide {
        Some(direction) if direction != 0. => card
            .with_animation(
                SharedString::from(format!("reaction-preview-{direction}")),
                Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint),
                move |card, delta| {
                    // From 12px on the side it came from, at 98%.
                    card.ml(px(-12. * direction * (1. - delta)))
                        .opacity(0.6 + 0.4 * delta)
                },
            )
            .into_any_element(),
        _ => card.into_any_element(),
    }
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

/// `deliveryFeedback` at `now` (ms): nothing for a seen message or in the
/// first ten seconds.
pub fn delivery_feedback(row: &MessageRow, now: u64) -> Option<String> {
    let delivery = row.delivery.as_ref()?;
    if delivery.status == Delivery::Seen || now < row.created_at * 1000 + DELIVERY_GRACE_MS {
        return None;
    }
    Some(match (&delivery.status, &delivery.error) {
        (Delivery::Failed, _) => "Couldn’t send this message.".to_string(),
        (_, Some(error)) => format!("Delivery not yet confirmed. {error}"),
        (_, None) => "Delivery not yet confirmed.".to_string(),
    })
}

/// The delivery line under an unconfirmed send, once its grace period
/// ends. The row redraws itself when it does.
fn delivery_notice(row: &Arc<MessageRow>, rows: &Rc<Rows>, cx: &App) -> Option<AnyElement> {
    let delivery = row.delivery.as_ref()?;
    if delivery.status == Delivery::Seen {
        return None;
    }
    let now = crate::agent_activity::observer::now_ms();
    let due = row.created_at * 1000 + DELIVERY_GRACE_MS;
    let Some(text) = delivery_feedback(row, now) else {
        if now < due {
            schedule_refresh(row.id, Duration::from_millis(due - now), cx);
        }
        return None;
    };
    let c = cx.colors();
    let mut notice = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .mt(px(6.))
        .text_caption()
        .text_color(c.text_warning)
        .child(text);
    if matches!(delivery.status, Delivery::Failed | Delivery::Unknown)
        && let Some(retry) = rows.actions.on_retry.clone()
    {
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
    Some(notice.into_any_element())
}

/// A report sent while the conversation was hidden: "Submitting report…",
/// or its failure with "Review report" (`ReportMessageDialog`).
fn report_state(
    row: &Arc<MessageRow>,
    failure: Option<SharedString>,
    rows: &Rc<Rows>,
    cx: &App,
) -> AnyElement {
    let c = cx.colors();
    let line = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .mt(px(6.))
        .text_body_sm()
        .text_color(c.text_standard);
    let Some(failure) = failure else {
        return line.child("Submitting report…").into_any_element();
    };
    let (rows, target) = (rows.clone(), row.clone());
    line.child(failure)
        .child(
            buzz_ui::Button::new(
                SharedString::from(format!("review-report-{}", row.id.to_hex())),
                "Review report",
            )
            .size(buzz_ui::ButtonSize::Sm)
            .on_click(move |_, window, cx| {
                rows.command(&target, Command::ReviewReport, window, cx)
            }),
        )
        .into_any_element()
}

/// Redraws every window once `after` passes, once per row.
fn schedule_refresh(id: EventId, after: Duration, cx: &App) {
    thread_local! {
        static PENDING: std::cell::RefCell<std::collections::HashSet<EventId>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    if !PENDING.with_borrow_mut(|pending| pending.insert(id)) {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(after).await;
        PENDING.with_borrow_mut(|pending| pending.remove(&id));
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

fn thread_summary(row: &Arc<MessageRow>, rows: &Rc<Rows>, cx: &App) -> impl IntoElement {
    let c = *cx.colors();
    let profiles = &rows.directory.profiles;
    let names = rows.names();
    let mut avatars = div().flex();
    for (index, participant) in row.participants.iter().take(3).enumerate() {
        let hex = participant.to_hex();
        let profile = profiles.get(&hex);
        let name = names.display(&hex);
        let mut avatar = Avatar::new(name.clone()).size(AvatarSize::Small);
        if names.is_library_agent(&hex) {
            avatar = avatar.squircle();
        }
        if let Some(picture) = profile.and_then(|p| p.picture.as_deref()) {
            avatar = avatar.image(media::avatar(picture, &rows.directory.media));
        }
        avatars = avatars.child(
            div()
                .id(SharedString::from(format!(
                    "participant-{}-{index}",
                    row.id.to_hex()
                )))
                .when(index > 0, |d| d.ml(px(-4.)))
                .rounded_full()
                .p(px(1.5))
                .bg(c.surface_panel)
                .tooltip(Tooltip::text(name))
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
    let unread = rows
        .marks
        .threads
        .get(&row.id)
        .and_then(|thread| thread_unread_label(*thread));
    let working = rows
        .marks
        .working
        .get(&row.id)
        .is_some_and(|agents| !agents.is_empty());
    let replies = row.reply_count > 0;
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
        .when(replies, |d| d.child(avatars).child(label))
        .when_some(unread.filter(|_| replies), |d, unread| {
            d.child(
                div()
                    .id(SharedString::from(format!(
                        "thread-unread-{}",
                        row.id.to_hex()
                    )))
                    .flex_none()
                    .size(px(8.))
                    .rounded_full()
                    .bg(c.text_standard)
                    .tooltip(Tooltip::text(unread)),
            )
        })
        .when(working, |d| d.child(working_dots(&row.id.to_hex(), cx)))
        .when(working && !replies, |d| d.child("View thread"));
    if let Some(open) = rows.actions.on_open_thread.clone() {
        let target = row.clone();
        summary = summary.on_click(move |_, window, cx| open(&target, window, cx));
    }
    summary
}

/// The thread summary's dot (`MessageRow`'s `unreadLabel`): marked
/// unread, or observed unread replies.
pub fn thread_unread_label(thread: ThreadUnread) -> Option<String> {
    match thread.manual {
        Manual::LocalOnly => Some("Thread marked unread on this device only".into()),
        Manual::Remote => Some("Thread marked unread".into()),
        Manual::None if thread.observed.unwrap_or(0) > 0 => Some(format!(
            "Observed unread replies{}. Not an exact total.",
            if thread.stale {
                "; may be out of date"
            } else {
                ""
            }
        )),
        Manual::None => None,
    }
}

/// Three dots hopping in turn while an agent works in a thread
/// (`.threadWorking`: 1.2s, 80ms apart).
pub fn working_dots(id: &str, cx: &App) -> AnyElement {
    const PERIOD: f32 = 1.2;
    let c = *cx.colors();
    let dots = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(2.))
        .w(px(26.))
        .h(px(15.))
        .rounded_full()
        .bg(c.affordance_subtle_hover);
    let color = c.text_standard;
    let dot = move |opacity: f32, rise: f32| {
        div()
            .relative()
            .top(px(-rise))
            .size(px(3.))
            .rounded_full()
            .bg(color)
            .opacity(opacity)
    };
    if cx.reduce_motion() {
        return dots
            .children((0..3).map(|_| dot(1., 0.)))
            .into_any_element();
    }
    dots.with_animation(
        SharedString::from(format!("thread-working-{id}")),
        Animation::new(Duration::from_millis(1200)).repeat(),
        move |dots, delta| {
            dots.children((0..3).map(|n| {
                let phase = (delta - n as f32 * 0.08 / PERIOD).rem_euclid(1.);
                // Up at 20%, down by 40%, resting to 100%.
                let lift = if phase < 0.2 {
                    phase / 0.2
                } else if phase < 0.4 {
                    1. - (phase - 0.2) / 0.2
                } else {
                    0.
                };
                dot(0.45 + 0.55 * lift, 2. * lift)
            }))
        },
    )
    .into_any_element()
}

/// Icon for a channel in headers and the sidebar.
pub fn channel_icon(channel: &buzz_relay_client::ChannelSummary) -> IconName {
    match channel.channel_type {
        Some(ChannelType::Dm) => IconName::ChatCircle,
        _ if channel.private => IconName::Lock,
        _ => IconName::Hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_relay_client::fold::{LocalDelivery, Reaction};
    use core::prelude::v1::test;

    #[test]
    fn delivery_waits_out_its_grace_period() {
        let mut row = super::super::markdown::tests::row("hi");
        row.created_at = 100;
        row.delivery = Some(LocalDelivery {
            status: Delivery::Sending,
            error: None,
        });
        assert_eq!(delivery_feedback(&row, 100_000 + 9_999), None);
        assert_eq!(
            delivery_feedback(&row, 110_000).as_deref(),
            Some("Delivery not yet confirmed.")
        );
        row.delivery = Some(LocalDelivery {
            status: Delivery::Accepted,
            error: Some("Slow relay.".into()),
        });
        assert_eq!(
            delivery_feedback(&row, 120_000).as_deref(),
            Some("Delivery not yet confirmed. Slow relay.")
        );
        row.delivery = Some(LocalDelivery {
            status: Delivery::Failed,
            error: None,
        });
        assert_eq!(
            delivery_feedback(&row, 120_000).as_deref(),
            Some("Couldn’t send this message.")
        );
        row.delivery = Some(LocalDelivery {
            status: Delivery::Seen,
            error: None,
        });
        assert_eq!(delivery_feedback(&row, 120_000), None);
    }

    #[test]
    fn reactors_list_you_first_then_event_order() {
        let (viewer, zed, amy) = (
            buzz_identity::Identity::ephemeral().public_key(),
            buzz_identity::Identity::ephemeral().public_key(),
            buzz_identity::Identity::ephemeral().public_key(),
        );
        let profiles: Profiles = Arc::new(
            [(zed, "Zed"), (amy, "Amy")]
                .into_iter()
                .map(|(k, n)| {
                    (
                        k.to_hex(),
                        Arc::new(Profile {
                            name: n.into(),
                            ..Default::default()
                        }),
                    )
                })
                .collect(),
        );
        let names = super::super::identity::names(
            &profiles,
            &Default::default(),
            &viewer.to_hex(),
            None,
            None,
        );
        let event = |author| Reaction {
            id: EventId::from_byte_array([0; 32]),
            author,
        };
        let group = ReactionGroup {
            content: "👍".into(),
            emoji: None,
            events: vec![event(zed), event(viewer), event(amy), event(zed)],
        };
        let (list, mine) = reactor_names(&group, &viewer.to_hex(), &names);
        assert!(mine);
        assert_eq!(list, ["You", "Zed", "Amy"]);
        assert_eq!(reaction_name(&group), ":+1:");
    }

    #[test]
    fn sent_from_thread_links_open_the_root_in_its_thread() {
        let root = EventId::from_byte_array([3; 32]);
        let link = sent_from_thread_link("c-1", &root);
        let parsed = buzz_relay_client::parse_buzz_link(&link).unwrap();
        assert_eq!(parsed.message, Some(root));
        assert_eq!(parsed.thread, Some(root));
    }
}
