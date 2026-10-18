//! What a plugin adds to the app's registries besides pages and panels:
//! settings cards, notification categories, message actions and markers,
//! link, message and attachment renderers, composer tools, page badges and
//! shortcuts (`docs/plugins.md`).
//!
//! The app draws renderers while it paints, without waiting. A plugin's
//! renderer is asked once per target in the background; until it answers,
//! the app draws what it would have anyway, and the answer is kept so the
//! next paint uses it. The answers are cleared when the plugin restarts.

use super::draw::{self, Draw};
use super::process::Requester;
use super::protocol::{self, Node};
use super::{ExternalPlugins, capabilities, intern};
use crate::community::AppState;
use crate::runtime::Runtime;
use crate::ui::composer::Composer;
use crate::ui::composer_completions::{self, Answer as Completed, Item, Place};
use crate::ui::composer_tools::ToolContext;
use crate::ui::markdown::Directory;
use crate::ui::renderers::{
    AttachmentRenderer, InMessage, LinkRenderer, MessageAction, MessageRenderer,
};
use buzz_relay_client::Media;
use buzz_relay_client::fold::{Attachment, MessageRow};
use buzz_ui::{Badge, BadgeTone, Icon, IconButton, IconButtonSize, IconName};
use gpui_kit::*;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

/// Answers kept at once; the oldest go first.
const MAX_ANSWERS: usize = 2_000;

enum Answer {
    Pending,
    Ready(Option<Rc<Node>>),
}

#[derive(Default)]
struct Answers {
    entries: HashMap<(String, String), Answer>,
    order: VecDeque<(String, String)>,
}

/// A renderer's answer for a target, on its way to the UI thread.
type Delivered = ((String, String), Option<Node>);

thread_local! {
    static ANSWERS: RefCell<Answers> = RefCell::default();
    static INBOX: RefCell<Option<futures::channel::mpsc::UnboundedSender<Delivered>>> =
        RefCell::default();
    /// Message ids each action marks, by action key.
    static MARKS: RefCell<HashMap<String, HashSet<String>>> = RefCell::default();
    /// A page's badge text and tone, by page key.
    static BADGES: RefCell<HashMap<String, (String, BadgeTone)>> = RefCell::default();
    static COMPOSERS: RefCell<HashMap<u64, WeakEntity<Composer>>> = RefCell::default();
    /// Settings cards a plugin hid or marked pending, by key.
    static CARD_STATES: RefCell<HashMap<String, crate::ui::settings::cards::CardVisibility>> =
        RefCell::default();
    /// Text renderer keys: their answers change how prose is prepared.
    static TEXT_KEYS: RefCell<HashSet<String>> = RefCell::default();
    /// Completion answers by completion key, channel and query; `None`
    /// while one is on its way.
    static SUGGESTIONS: RefCell<Suggestions> = RefCell::default();
    static SUGGESTION_INBOX: RefCell<Option<futures::channel::mpsc::UnboundedSender<Suggested>>> =
        RefCell::default();
    static NEXT_COMPOSER: Cell<u64> = const { Cell::new(0) };
}

type SuggestionKey = (String, String, String);
/// A completion's answer, on its way to the UI thread.
type Suggested = (SuggestionKey, Completed);

#[derive(Default)]
struct Suggestions {
    entries: HashMap<SuggestionKey, Option<Completed>>,
    order: VecDeque<SuggestionKey>,
}

/// A completion's suggestions for `query` typed in `channel`, asking the
/// plugin the first time.
fn suggest(
    plugin: &str,
    key: &str,
    id: &str,
    query: &str,
    place: Place,
    cx: &App,
) -> Option<Completed> {
    let Place {
        channel,
        thread,
        invite_agents,
    } = place;
    let entry = (
        key.to_owned(),
        format!("{channel}/{thread:?}/{invite_agents}"),
        query.to_owned(),
    );
    if let Some(known) =
        SUGGESTIONS.with_borrow(|suggestions| suggestions.entries.get(&entry).cloned())
    {
        return known;
    }
    let connection = ExternalPlugins::running_connection(plugin, cx)?;
    let inbox = SUGGESTION_INBOX.with_borrow(Clone::clone)?;
    let request = connection.request(
        "completion/query",
        json!({
            "completion": id,
            "query": query,
            "channel": channel,
            "thread": thread,
            "inviteAgents": invite_agents,
        }),
    );
    SUGGESTIONS.with_borrow_mut(|suggestions| {
        suggestions.entries.insert(entry.clone(), None);
        suggestions.order.push_back(entry.clone());
        while suggestions.order.len() > MAX_ANSWERS {
            if let Some(old) = suggestions.order.pop_front() {
                suggestions.entries.remove(&old);
            }
        }
    });
    Runtime::handle(cx).spawn(async move {
        let items = match request.await {
            Ok(Ok(value)) => answer(&value),
            _ => Completed {
                status: Some("Suggestions are not available.".into()),
                retry: true,
                ..Completed::default()
            },
        };
        let _ = inbox.unbounded_send((entry, items));
    });
    None
}

/// Forgets completion `key`'s answers.
fn forget_suggestions(key: &str) {
    SUGGESTIONS.with_borrow_mut(|suggestions| {
        suggestions.entries.retain(|(entry, _, _), _| entry != key);
        suggestions.order.retain(|(entry, _, _)| entry != key);
    });
    composer_completions::changed();
}

/// `completion/query`'s answer: `{"items", "status", "retry"}`.
fn answer(value: &Value) -> Completed {
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .map(|status| status.chars().take(200).collect::<String>().into());
    Completed {
        items: items(value),
        status,
        retry: value.get("retry").and_then(Value::as_bool) == Some(true),
    }
}

/// `completion/query`'s items: `[{"label", "detail", "text", "mentions",
/// "disabled", "preview", "exact"}]`.
fn items(value: &Value) -> Vec<Item> {
    let text = |item: &Value, key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .map(|text| text.chars().take(2_000).collect::<String>())
    };
    value
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .take(50)
                .filter_map(|item| {
                    let label = text(item, "label").filter(|label| !label.is_empty())?;
                    let mentions: Vec<String> = item
                        .get("mentions")
                        .and_then(Value::as_array)
                        .map(|keys| {
                            keys.iter()
                                .filter_map(Value::as_str)
                                .filter(|key| {
                                    key.len() == 64
                                        && key.bytes().all(|byte| byte.is_ascii_hexdigit())
                                })
                                .map(str::to_ascii_lowercase)
                                .take(32)
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(Item {
                        text: text(item, "text").unwrap_or_else(|| label.clone()),
                        detail: text(item, "detail").map(Into::into),
                        label: label.into(),
                        mentions,
                        disabled: text(item, "disabled").map(Into::into),
                        preview: text(item, "preview").map(Into::into),
                        exact: item.get("exact").and_then(Value::as_bool) == Some(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Starts taking renderer answers.
pub fn init(cx: &mut App) {
    super::naming::init(cx);
    let (sender, mut suggestions) = futures::channel::mpsc::unbounded();
    SUGGESTION_INBOX.with_borrow_mut(|inbox| *inbox = Some(sender));
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some((key, items)) = suggestions.next().await {
            SUGGESTIONS.with_borrow_mut(|suggestions| {
                if let Some(entry) = suggestions.entries.get_mut(&key) {
                    *entry = Some(items);
                }
            });
            composer_completions::changed();
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
    let (sender, mut answers) = futures::channel::mpsc::unbounded();
    INBOX.with_borrow_mut(|inbox| *inbox = Some(sender));
    cx.spawn(async move |cx| {
        use futures::StreamExt;
        while let Some((key, tree)) = answers.next().await {
            let drawn = tree.is_some();
            ANSWERS.with_borrow_mut(|answers| {
                if let Some(answer) = answers.entries.get_mut(&key) {
                    *answer = Answer::Ready(tree.map(Rc::new));
                }
            });
            if drawn && TEXT_KEYS.with_borrow(|keys| keys.contains(&key.0)) {
                crate::ui::renderers::text_changed();
            }
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

/// Forgets what `plugin` answered and marked, when it restarts or stops.
pub fn forget(plugin: &str) {
    let prefix = format!("{plugin}/");
    ANSWERS.with_borrow_mut(|answers| {
        answers
            .entries
            .retain(|(key, _), _| !key.starts_with(&prefix));
        answers.order.retain(|(key, _)| !key.starts_with(&prefix));
    });
    MARKS.with_borrow_mut(|marks| marks.retain(|key, _| !key.starts_with(&prefix)));
    CARD_STATES.with_borrow_mut(|states| states.retain(|key, _| !key.starts_with(&prefix)));
    super::naming::forget(plugin);
    // Prose drew this plugin's old answers.
    crate::ui::renderers::text_changed();
    SUGGESTIONS.with_borrow_mut(|suggestions| {
        suggestions
            .entries
            .retain(|(key, _, _), _| !key.starts_with(&prefix));
        suggestions
            .order
            .retain(|(key, _, _)| !key.starts_with(&prefix));
    });
    BADGES.with_borrow_mut(|badges| badges.retain(|key, _| !key.starts_with(&prefix)));
}

/// The plugin's answer for `target` of renderer `key`, asking for it the
/// first time.
fn ask(
    plugin: &str,
    key: &str,
    target: &str,
    method: &str,
    params: impl FnOnce() -> Value,
    cx: &App,
) -> Option<Rc<Node>> {
    let entry = (key.to_owned(), target.to_owned());
    let known = ANSWERS.with_borrow(|answers| match answers.entries.get(&entry) {
        Some(Answer::Ready(tree)) => Some(tree.clone()),
        Some(Answer::Pending) => Some(None),
        None => None,
    });
    if let Some(tree) = known {
        return tree;
    }
    let connection = ExternalPlugins::running_connection(plugin, cx)?;
    let request = connection.request(method, params());
    ANSWERS.with_borrow_mut(|answers| {
        answers.entries.insert(entry.clone(), Answer::Pending);
        answers.order.push_back(entry.clone());
        while answers.order.len() > MAX_ANSWERS {
            if let Some(old) = answers.order.pop_front() {
                answers.entries.remove(&old);
            }
        }
    });
    let inbox = INBOX.with_borrow(Clone::clone)?;
    Runtime::handle(cx).spawn(async move {
        let tree = match request.await {
            Ok(Ok(Value::Null)) => None,
            Ok(Ok(value)) => {
                let tree = value.get("tree").cloned().unwrap_or(value);
                Node::parse(tree)
                    .inspect_err(|error| tracing::warn!(%error, "plugin renderer answer"))
                    .ok()
            }
            _ => None,
        };
        let _ = inbox.unbounded_send((entry, tree));
    });
    None
}

/// What a renderer answered for `target`: `None` until it has, then its
/// tree or `None` if it declined.
pub(super) fn answered(key: &str, target: &str) -> Option<Option<Rc<Node>>> {
    ANSWERS.with_borrow(
        |answers| match answers.entries.get(&(key.to_owned(), target.to_owned())) {
            Some(Answer::Ready(tree)) => Some(tree.clone()),
            _ => None,
        },
    )
}

/// A renderer's answer: `{"tree"}`, a bare tree, or `null`.
pub(super) fn tree_of(value: Value) -> Option<Node> {
    if value.is_null() {
        return None;
    }
    Node::parse(value.get("tree").cloned().unwrap_or(value)).ok()
}

/// Asks plugin renderer `key` about `target` through `requester`, once;
/// `parse` makes the answer a tree.
pub(super) fn ask_with(
    requester: &Requester,
    key: &str,
    target: &str,
    method: &str,
    params: Value,
    parse: fn(Value) -> Option<Node>,
) {
    let entry = (key.to_owned(), target.to_owned());
    let fresh = ANSWERS.with_borrow_mut(|answers| {
        if answers.entries.contains_key(&entry) {
            return false;
        }
        answers.entries.insert(entry.clone(), Answer::Pending);
        answers.order.push_back(entry.clone());
        while answers.order.len() > MAX_ANSWERS {
            if let Some(old) = answers.order.pop_front() {
                answers.entries.remove(&old);
            }
        }
        true
    });
    let Some(inbox) = INBOX.with_borrow(Clone::clone).filter(|_| fresh) else {
        return;
    };
    let request = requester.request(method, params, std::time::Duration::from_secs(10));
    requester.runtime().spawn(async move {
        let tree = match request.await {
            Ok(Ok(value)) => parse(value),
            Ok(Err(_)) | Err(_) => None,
        };
        let _ = inbox.unbounded_send((entry, tree));
    });
}

/// Draws message text a plugin's pattern matches.
struct Inline {
    key: &'static str,
    plugin: String,
    id: String,
    pattern: regex::Regex,
    requester: Requester,
}

impl crate::ui::renderers::TextRenderer for Inline {
    fn key(&self) -> &str {
        self.key
    }

    fn find(&self, text: &str, message: InMessage) -> Vec<std::ops::Range<usize>> {
        let id = message.row.id.to_hex();
        let mut found = Vec::new();
        for matched in self.pattern.find_iter(text).take(32) {
            let piece = matched.as_str();
            if piece.is_empty() || piece.len() > 200 {
                continue;
            }
            // Answers are the plugin's for this piece in this message.
            let target = format!("{id}/{piece}");
            match answered(self.key, &target) {
                Some(Some(_)) => found.push(matched.range()),
                Some(None) => {}
                None => ask_with(
                    &self.requester,
                    self.key,
                    &target,
                    "inline/render",
                    json!({
                        "renderer": self.id,
                        "text": piece,
                        "message": capabilities::message(message.row, message.profiles),
                    }),
                    tree_of,
                ),
            }
        }
        found
    }

    fn render(&self, matched: &str, message: &str, cx: &App) -> Option<AnyElement> {
        let tree = answered(self.key, &format!("{message}/{matched}"))??;
        Some(drawn(&self.plugin, &self.id, self.key, matched, &tree, cx))
    }
}

fn hashed(text: &str) -> String {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Draws a renderer's answer; its buttons send `render/event`.
pub(super) fn drawn(
    plugin: &str,
    id: &str,
    key: &str,
    target: &str,
    tree: &Node,
    cx: &App,
) -> AnyElement {
    let (plugin, renderer, place) = (plugin.to_owned(), id.to_owned(), target.to_owned());
    let fields = HashMap::new();
    let embeds = HashMap::new();
    let draw = Draw {
        send: Rc::new(move |element, event, value, cx| {
            ExternalPlugins::notify_running(
                &plugin,
                "render/event",
                json!({
                    "renderer": renderer,
                    "target": place,
                    "id": element,
                    "event": event,
                    "value": value,
                }),
                cx,
            );
        }),
        fields: &fields,
        embeds: &embeds,
        community: AppState::current(cx),
        place: key,
    };
    draw::node(tree, &hashed(target), &draw, cx)
}

fn message_json(row: &MessageRow, cx: &App) -> Value {
    let profiles = AppState::current(cx)
        .map(|community| community.read(cx).snapshot.profiles.clone())
        .unwrap_or_default();
    capabilities::message(row, &profiles)
}

/// The viewer, for actions on the user's own messages.
fn viewer(cx: &App) -> Option<String> {
    match &cx.global::<AppState>().identity {
        crate::community::IdentityState::Ready(identity) => Some(identity.pubkey_hex()),
        _ => None,
    }
}

struct Link {
    key: &'static str,
    plugin: String,
    id: String,
    prefixes: Vec<String>,
    order: i32,
}

impl LinkRenderer for Link {
    fn key(&self) -> &str {
        self.key
    }

    fn order(&self, _: &str) -> i32 {
        self.order
    }

    fn matches(&self, url: &str) -> bool {
        self.prefixes
            .iter()
            .any(|prefix| url.starts_with(prefix.as_str()))
    }

    fn render(&self, url: &str, label: Option<&str>, cx: &App) -> Option<AnyElement> {
        let tree = ask(
            &self.plugin,
            self.key,
            url,
            "link/render",
            || json!({ "renderer": self.id, "url": url, "label": label }),
            cx,
        )?;
        Some(drawn(&self.plugin, &self.id, self.key, url, &tree, cx))
    }
}

struct Message {
    key: &'static str,
    plugin: String,
    id: String,
    kinds: Vec<u16>,
    prefix: Option<String>,
    order: i32,
}

impl MessageRenderer for Message {
    fn key(&self) -> &str {
        self.key
    }

    fn order(&self, _: &MessageRow) -> i32 {
        self.order
    }

    fn matches(&self, row: &MessageRow) -> bool {
        (self.kinds.is_empty() && row.kind == 9 || self.kinds.contains(&row.kind))
            && self
                .prefix
                .as_ref()
                .is_none_or(|prefix| row.content.starts_with(prefix.as_str()))
    }

    fn render(&self, row: &Arc<MessageRow>, _: &Directory, cx: &App) -> Option<AnyElement> {
        // An edit is a new target.
        let target = format!("{}:{}", row.id.to_hex(), hashed(&row.content));
        let tree = ask(
            &self.plugin,
            self.key,
            &target,
            "message/render",
            || json!({ "renderer": self.id, "message": message_json(row, cx) }),
            cx,
        )?;
        Some(drawn(&self.plugin, &self.id, self.key, &target, &tree, cx))
    }
}

/// A plugin accessory beside messages' reactions.
struct MessageAccessoryView {
    key: &'static str,
    plugin: String,
    id: String,
    order: i32,
    kinds: Vec<u16>,
}

impl crate::ui::renderers::MessageAccessory for MessageAccessoryView {
    fn key(&self) -> &str {
        self.key
    }

    fn order(&self) -> i32 {
        self.order
    }

    fn render(&self, row: &Arc<MessageRow>, cx: &App) -> Option<AnyElement> {
        let kinds_ok = if self.kinds.is_empty() {
            row.kind == 9
        } else {
            self.kinds.contains(&row.kind)
        };
        if !kinds_ok {
            return None;
        }
        // The message as it is now: an edit or a new reply asks again.
        let target = format!(
            "{}:{}:{}",
            row.id.to_hex(),
            hashed(&row.content),
            row.reply_count
        );
        let tree = ask(
            &self.plugin,
            self.key,
            &target,
            "message/accessory",
            || json!({ "accessory": self.id, "message": message_json(row, cx) }),
            cx,
        )?;
        Some(drawn(&self.plugin, &self.id, self.key, &target, &tree, cx))
    }
}

struct AttachmentView {
    key: &'static str,
    plugin: String,
    id: String,
    mime: Vec<String>,
}

impl AttachmentRenderer for AttachmentView {
    fn key(&self) -> &str {
        self.key
    }

    fn matches(&self, attachment: &Attachment) -> bool {
        let mime = attachment.mime.as_deref().unwrap_or_default();
        self.mime
            .iter()
            .any(|prefix| mime.starts_with(prefix.as_str()))
    }

    fn render(
        &self,
        attachment: &Attachment,
        _: SharedString,
        _: &Media,
        cx: &App,
    ) -> Option<AnyElement> {
        let target = attachment.url.clone();
        let tree = ask(
            &self.plugin,
            self.key,
            &target,
            "attachment/render",
            || {
                json!({
                    "renderer": self.id,
                    "attachment": {
                        "url": attachment.url,
                        "mime": attachment.mime,
                        "name": attachment.name,
                        "size": attachment.size,
                    },
                })
            },
            cx,
        )?;
        Some(drawn(&self.plugin, &self.id, self.key, &target, &tree, cx))
    }
}

struct Action {
    key: &'static str,
    plugin: String,
    id: String,
    title: String,
    icon: Option<IconName>,
    kinds: Vec<u16>,
    /// The viewer's key when `mine` is set.
    mine: Option<String>,
}

impl MessageAction for Action {
    fn key(&self) -> &str {
        self.key
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn icon(&self) -> Option<IconName> {
        self.icon
    }

    fn matches(&self, row: &MessageRow) -> bool {
        (self.kinds.is_empty() && row.kind == 9 || self.kinds.contains(&row.kind))
            && self
                .mine
                .as_ref()
                .is_none_or(|viewer| row.author.to_hex() == *viewer)
    }

    fn run(&self, row: &Arc<MessageRow>, _: &mut Window, cx: &mut App) {
        let message = message_json(row, cx);
        ExternalPlugins::notify_running(
            &self.plugin,
            "message/action",
            json!({ "action": self.id, "message": message }),
            cx,
        );
    }

    fn marker(&self, row: &MessageRow, _: &App) -> Option<AnyElement> {
        let marked = MARKS.with_borrow(|marks| {
            marks
                .get(self.key)
                .is_some_and(|ids| ids.contains(&row.id.to_hex()))
        });
        marked.then(|| {
            Icon::new(self.icon.unwrap_or(IconName::Flag))
                .size(14.)
                .into_any_element()
        })
    }
}

/// `message/markers`: the messages action `id` marks now.
pub fn set_marks(plugin: &str, params: &Value, cx: &mut App) {
    let Some(action) = params.get("action").and_then(Value::as_str) else {
        return;
    };
    let ids: HashSet<String> = params
        .get("messages")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .take(10_000)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    MARKS.with_borrow_mut(|marks| marks.insert(format!("{plugin}/{action}"), ids));
    cx.refresh_windows();
}

/// `settings/visibility`: whether one of the plugin's Settings sections
/// is listed (`visible`), left out (`hidden`) or still being decided
/// (`pending`).
pub fn set_card_visibility(plugin: &str, params: &Value, cx: &mut App) {
    use crate::ui::settings::cards::{self, CardVisibility};
    let Some(card) = params.get("card").and_then(Value::as_str) else {
        return;
    };
    let state = match params.get("visibility").and_then(Value::as_str) {
        Some("hidden") => CardVisibility::Hidden,
        Some("pending") => CardVisibility::Pending,
        _ => CardVisibility::Visible,
    };
    CARD_STATES.with_borrow_mut(|states| states.insert(format!("{plugin}/{card}"), state));
    cards::changed(cx);
    cx.refresh_windows();
}

/// `page/badge`: the text on a page's row, or none.
pub fn set_badge(plugin: &str, params: &Value, cx: &mut App) {
    let Some(page) = params.get("page").and_then(Value::as_str) else {
        return;
    };
    let key = format!("{plugin}/{page}");
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .map(|text| text.chars().take(8).collect::<String>())
        .filter(|text| !text.is_empty());
    let tone = match params.get("tone").and_then(Value::as_str) {
        Some("accent") => BadgeTone::Accent,
        Some("success") => BadgeTone::Success,
        Some("warning") => BadgeTone::Warning,
        Some("danger") => BadgeTone::Danger,
        Some("prominent") => BadgeTone::Prominent,
        _ => BadgeTone::Neutral,
    };
    BADGES.with_borrow_mut(|badges| match text {
        Some(text) => badges.insert(key, (text, tone)),
        None => badges.remove(&key),
    });
    cx.refresh_windows();
}

/// A page row's badge for `key`.
pub fn badge(key: &'static str) -> crate::ui::pages::Badge {
    Rc::new(move |_| {
        BADGES.with_borrow(|badges| {
            badges
                .get(key)
                .map(|(text, tone)| Badge::new(text.clone()).tone(*tone).into_any_element())
        })
    })
}

/// A composer tool's toolbar button.
struct Tool {
    plugin: String,
    id: String,
    title: SharedString,
    icon: IconName,
    context: ToolContext,
    composer: u64,
}

impl Render for Tool {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (plugin, id, channel, composer, thread) = (
            self.plugin.clone(),
            self.id.clone(),
            self.context.channel.clone(),
            self.composer,
            self.context.thread.clone(),
        );
        let (disabled, invite_agents) = self
            .context
            .composer
            .upgrade()
            .map(|composer| {
                let composer = composer.read(cx);
                (composer.disabled(cx), composer.invites_agents())
            })
            .unwrap_or((true, false));
        IconButton::new(
            SharedString::from(format!("plugin-tool-{}-{}", self.plugin, self.id)),
            self.icon,
        )
        .size(IconButtonSize::Sm)
        .tooltip(self.title.clone())
        .disabled(disabled)
        .on_click(move |_, _, cx| {
            ExternalPlugins::notify_running(
                &plugin,
                "composer/tool",
                json!({
                    "tool": id,
                    "channel": channel,
                    "thread": thread,
                    "composer": composer,
                    "inviteAgents": invite_agents,
                }),
                cx,
            );
        })
    }
}

/// `composer/insert`: puts text at a composer's caret.
pub fn insert(params: &Value, cx: &mut App) -> Result<Value, capabilities::Failure> {
    let id = params
        .get("composer")
        .and_then(Value::as_u64)
        .ok_or((protocol::INVALID_PARAMS, "composer is required".to_owned()))?;
    let text = params
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.chars().count() <= 10_000)
        .ok_or((protocol::INVALID_PARAMS, "text is required".to_owned()))?
        .to_owned();
    let composer = COMPOSERS
        .with_borrow(|composers| composers.get(&id).and_then(WeakEntity::upgrade))
        .ok_or((1, "That composer is closed.".to_owned()))?;
    let window =
        crate::ui::navigation::main_window(cx).ok_or((1, "The window is not open.".to_owned()))?;
    let input = composer.read(cx).input();
    window
        .update(cx, |_, window, cx| {
            input.update(cx, |state, cx| state.insert(text, window, cx));
        })
        .map_err(|_| (1, "The window is not open.".to_owned()))?;
    Ok(Value::Null)
}

/// The open composer `params.composer` names, and the window it is in.
/// A composer id `composer/insert` and the like take, for `composer`.
pub(super) fn composer_id(composer: &Entity<Composer>) -> u64 {
    let id = NEXT_COMPOSER.with(|next| {
        next.set(next.get() + 1);
        next.get()
    });
    COMPOSERS.with_borrow_mut(|composers| {
        composers.retain(|_, weak| weak.upgrade().is_some());
        composers.insert(id, composer.downgrade());
    });
    id
}

fn composer_of(
    params: &Value,
    cx: &App,
) -> Result<(Entity<Composer>, AnyWindowHandle), capabilities::Failure> {
    let id = params
        .get("composer")
        .and_then(Value::as_u64)
        .ok_or((protocol::INVALID_PARAMS, "composer is required".to_owned()))?;
    let composer = COMPOSERS
        .with_borrow(|composers| composers.get(&id).and_then(WeakEntity::upgrade))
        .ok_or((1, "That composer is closed.".to_owned()))?;
    let window =
        crate::ui::navigation::main_window(cx).ok_or((1, "The window is not open.".to_owned()))?;
    Ok((composer, window))
}

/// `composer/insertMentions`: mention chips for `pubkeys` at the caret,
/// which also make them recipients (`insertMentions`).
pub fn insert_mentions(params: &Value, cx: &mut App) -> Result<Value, capabilities::Failure> {
    let (composer, window) = composer_of(params, cx)?;
    let pubkeys: Vec<String> = params
        .get("pubkeys")
        .and_then(Value::as_array)
        .ok_or((
            protocol::INVALID_PARAMS,
            "pubkeys must be a list".to_owned(),
        ))?
        .iter()
        .filter_map(Value::as_str)
        .filter(|key| key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .take(32)
        .collect();
    if pubkeys.is_empty() {
        return Err((protocol::INVALID_PARAMS, "pubkeys names no one".to_owned()));
    }
    let community = AppState::current(cx).ok_or((1, "No community is selected.".to_owned()))?;
    let profiles = community.read(cx).snapshot.profiles.clone();
    let recipients: Vec<(String, SharedString, Option<String>)> = pubkeys
        .into_iter()
        .map(|key| {
            let name = profiles
                .get(&key)
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| key.chars().take(10).collect());
            (key, name.into(), None)
        })
        .collect();
    let inserted = window
        .update(cx, |_, window, cx| {
            composer.update(cx, |composer, cx| {
                composer.insert_team(&recipients, None, window, cx)
            })
        })
        .map_err(|_| (1, "The window is not open.".to_owned()))?;
    if inserted {
        Ok(Value::Null)
    } else {
        Err((3, "Choose at most 32 recipients".to_owned()))
    }
}

/// `composer/insertResource`: a link to `uri`, labelled `label`, as one
/// chip at the caret (`insertResource`).
pub fn insert_resource(params: &Value, cx: &mut App) -> Result<Value, capabilities::Failure> {
    let (composer, window) = composer_of(params, cx)?;
    let text = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .ok_or((protocol::INVALID_PARAMS, format!("{key} is required")))
    };
    let (uri, label) = (text("uri")?.to_owned(), text("label")?.to_owned());
    window
        .update(cx, |_, window, cx| {
            crate::ui::composer_tools::insert_resource(&composer, &uri, &label, window, cx)
        })
        .map_err(|_| (1, "The window is not open.".to_owned()))?
        .map(|()| Value::Null)
        .map_err(|reason| (3, reason.to_owned()))
}

/// `composer/capture`: the plugin's view of kind `capture` in place of
/// the composer's editor, as voice notes record (`capture`), until
/// `composer/accept` or the user discards it.
pub fn capture(plugin: &str, params: &Value, cx: &mut App) -> Result<Value, capabilities::Failure> {
    let (composer, window) = composer_of(params, cx)?;
    let id = params
        .get("composer")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let weak = composer.downgrade();
    let accept: super::view::Accept = Rc::new(move |file, window, cx| {
        let _ = weak.update(cx, |composer, cx| {
            composer.accept_plugin_capture(file, window, cx)
        });
    });
    let (view, view_id) = super::view::ExternalView::capture(plugin, id, accept, cx);
    let started = window
        .update(cx, |_, window, cx| {
            composer.update(cx, |composer, cx| {
                composer.start_plugin_capture(view.into(), window, cx)
            })
        })
        .map_err(|_| (1, "The window is not open.".to_owned()))?;
    if started {
        Ok(json!({ "view": view_id }))
    } else {
        Err((3, "The composer is busy.".to_owned()))
    }
}

/// `composer/accept`: ends a capture, attaching the file in `data`
/// (base64, with `name` and `mime`) if given.
pub fn accept(
    params: &Value,
    accept: super::view::Accept,
    cx: &mut App,
) -> Result<Value, capabilities::Failure> {
    use base64::Engine;
    let file = match params.get("data").and_then(Value::as_str) {
        None => None,
        Some(data) => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| (protocol::INVALID_PARAMS, "data is not base64".to_owned()))?;
            if bytes.is_empty() || bytes.len() > 3 * 1024 * 1024 {
                return Err((
                    protocol::INVALID_PARAMS,
                    "A file must be 1 byte to 3 MiB.".to_owned(),
                ));
            }
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("capture")
                .chars()
                .take(200)
                .collect::<String>();
            let mime = params
                .get("mime")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| buzz_relay_client::upload::mime_type(&name, &bytes));
            let voice = match params.get("voice").filter(|voice| !voice.is_null()) {
                None => None,
                Some(voice) => {
                    let duration = voice
                        .get("duration")
                        .and_then(Value::as_f64)
                        .filter(|duration| duration.is_finite() && *duration > 0.0)
                        .ok_or((
                            protocol::INVALID_PARAMS,
                            "voice.duration must be seconds".to_owned(),
                        ))?;
                    let waveform = voice
                        .get("waveform")
                        .and_then(Value::as_array)
                        .map(|samples| {
                            samples
                                .iter()
                                .take(256)
                                .filter_map(Value::as_u64)
                                .map(|sample| sample.min(255) as u8)
                                .collect()
                        })
                        .unwrap_or_default();
                    Some((duration, waveform))
                }
            };
            Some(crate::ui::composer::Captured {
                name,
                mime,
                bytes,
                voice,
            })
        }
    };
    let window =
        crate::ui::navigation::main_window(cx).ok_or((1, "The window is not open.".to_owned()))?;
    window
        .update(cx, |_, window, cx| accept(file, window, cx))
        .map_err(|_| (1, "The window is not open.".to_owned()))?;
    Ok(Value::Null)
}

/// `composer/focus`: puts the caret back in the composer (`focus`).
pub fn focus(params: &Value, cx: &mut App) -> Result<Value, capabilities::Failure> {
    let (composer, window) = composer_of(params, cx)?;
    let input = composer.read(cx).input();
    window
        .update(cx, |_, window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx))
        })
        .map_err(|_| (1, "The window is not open.".to_owned()))?;
    Ok(Value::Null)
}

/// What one plugin added to these registries, to take away later.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Added {
    Card(&'static str),
    Category(&'static str),
    Renderer(&'static str),
    Tool(&'static str),
    Shortcut(&'static str),
    Completion(&'static str),
    AccountAction(&'static str),
    ReactionPicker(&'static str),
    NamingPolicy(&'static str),
}

pub fn take_away(added: Added, cx: &mut App) {
    match added {
        Added::Card(key) => crate::ui::settings::cards::unregister(key, cx),
        Added::Category(key) => crate::notifications::categories::unregister(key, cx),
        Added::Renderer(key) => crate::ui::renderers::unregister(key),
        Added::Tool(key) => crate::ui::composer_tools::unregister(key, cx),
        Added::Shortcut(key) => crate::shortcuts::unregister_external(key),
        Added::Completion(key) => composer_completions::unregister(key),
        Added::AccountAction(key) => crate::ui::account_actions::unregister(key, cx),
        Added::ReactionPicker(key) => crate::ui::emoji_picker::unregister_reaction_picker(key, cx),
        Added::NamingPolicy(key) => crate::ui::identity::unregister_policy(key),
    }
}

/// Registers everything in `contributions` but pages and panels.
pub fn register(
    plugin: &str,
    name: &str,
    contributions: &protocol::Contributions,
    requester: Option<Requester>,
    cx: &mut App,
) -> Vec<Added> {
    use crate::notifications::categories::{self, CategorySpec};
    use crate::ui::settings::cards::{self, CardPlacement, SettingsCard};
    let mut added = Vec::new();
    let key = |id: &str| intern(format!("{plugin}/{id}"));
    for card in &contributions.settings_cards {
        let card_key = key(&card.id);
        let (owner, id) = (plugin.to_owned(), card.id.clone());
        cards::register(
            SettingsCard {
                key: card_key,
                title: intern(card.title.clone()),
                icon: card.icon.as_deref().map(|name| super::icon(Some(name))),
                placement: match card.placement.to_ascii_lowercase().as_str() {
                    "community" => CardPlacement::Community,
                    "administration" => CardPlacement::Administration,
                    _ => CardPlacement::Group(intern(card.placement.clone())),
                },
                visibility: Some(Rc::new(move |_| {
                    CARD_STATES.with_borrow(|states| {
                        states
                            .get(card_key)
                            .copied()
                            .unwrap_or(cards::CardVisibility::Visible)
                    })
                })),
                build: cards::build(move |window, cx| {
                    super::ExternalView::settings(&owner, &id, window, cx)
                }),
            },
            cx,
        );
        cards::changed(cx);
        added.push(Added::Card(card_key));
    }
    for category in &contributions.notifications {
        let category_key = key(&category.id);
        categories::register(
            CategorySpec {
                key: category_key,
                label: intern(category.label.clone()),
            },
            cx,
        );
        added.push(Added::Category(category_key));
    }
    let mine = viewer(cx);
    for action in &contributions.message_actions {
        let action_key = key(&action.id);
        // A restart registers it again under the same key.
        crate::ui::renderers::unregister(action_key);
        crate::ui::renderers::register_action(Rc::new(Action {
            key: action_key,
            plugin: plugin.to_owned(),
            id: action.id.clone(),
            title: action.title.clone(),
            icon: action.icon.as_deref().map(|name| super::icon(Some(name))),
            kinds: action.kinds.clone(),
            mine: action.mine.then(|| mine.clone()).flatten(),
        }));
        added.push(Added::Renderer(action_key));
    }
    for renderer in &contributions.link_renderers {
        let renderer_key = key(&renderer.id);
        // A restart registers it again under the same key.
        crate::ui::renderers::unregister(renderer_key);
        crate::ui::renderers::register_link(Rc::new(Link {
            key: renderer_key,
            plugin: plugin.to_owned(),
            id: renderer.id.clone(),
            prefixes: renderer.matches.clone(),
            order: renderer.order,
        }));
        added.push(Added::Renderer(renderer_key));
    }
    for renderer in &contributions.message_renderers {
        let renderer_key = key(&renderer.id);
        // A restart registers it again under the same key.
        crate::ui::renderers::unregister(renderer_key);
        crate::ui::renderers::register_message(Rc::new(Message {
            key: renderer_key,
            plugin: plugin.to_owned(),
            id: renderer.id.clone(),
            kinds: renderer.kinds.clone(),
            prefix: renderer.prefix.clone(),
            order: renderer.order,
        }));
        added.push(Added::Renderer(renderer_key));
    }
    for renderer in &contributions.attachment_renderers {
        let renderer_key = key(&renderer.id);
        // A restart registers it again under the same key.
        crate::ui::renderers::unregister(renderer_key);
        crate::ui::renderers::register_attachment(Rc::new(AttachmentView {
            key: renderer_key,
            plugin: plugin.to_owned(),
            id: renderer.id.clone(),
            mime: renderer.mime.clone(),
        }));
        added.push(Added::Renderer(renderer_key));
    }
    for tool in &contributions.composer_tools {
        let tool_key = key(&tool.id);
        let (owner, id, title, icon) = (
            plugin.to_owned(),
            tool.id.clone(),
            SharedString::from(tool.title.clone()),
            super::icon(tool.icon.as_deref()),
        );
        crate::ui::composer_tools::register(
            crate::ui::composer_tools::ComposerTool {
                key: tool_key,
                order: tool.order,
                accessory: false,
                build: Rc::new(move |context, _, cx| {
                    let composer = NEXT_COMPOSER.with(|next| {
                        next.set(next.get() + 1);
                        next.get()
                    });
                    COMPOSERS.with_borrow_mut(|composers| {
                        composers.retain(|_, weak| weak.upgrade().is_some());
                        composers.insert(composer, context.composer.clone());
                    });
                    let (plugin, id, title) = (owner.clone(), id.clone(), title.clone());
                    cx.new(|_| Tool {
                        plugin,
                        id,
                        title,
                        icon,
                        context,
                        composer,
                    })
                    .into()
                }),
            },
            cx,
        );
        added.push(Added::Tool(tool_key));
        if tool.reaction_picker {
            let (owner, id) = (plugin.to_owned(), tool.id.clone());
            crate::ui::emoji_picker::register_reaction_picker(
                crate::ui::emoji_picker::ReactionPicker {
                    key: tool_key,
                    order: tool.order,
                    build: Rc::new(move |select, window, cx| {
                        super::view::ExternalView::reaction_picker(&owner, &id, select, window, cx)
                    }),
                },
                cx,
            );
            added.push(Added::ReactionPicker(tool_key));
        }
    }
    for accessory in &contributions.composer_accessories {
        let accessory_key = key(&accessory.id);
        if accessory.placement == protocol::AccessoryPlacement::Message {
            crate::ui::renderers::unregister(accessory_key);
            crate::ui::renderers::register_message_accessory(Rc::new(MessageAccessoryView {
                key: accessory_key,
                plugin: plugin.to_owned(),
                id: accessory.id.clone(),
                order: accessory.order,
                kinds: accessory.kinds.clone(),
            }));
            added.push(Added::Renderer(accessory_key));
            continue;
        }
        let (owner, id) = (plugin.to_owned(), accessory.id.clone());
        crate::ui::composer_tools::register(
            crate::ui::composer_tools::ComposerTool {
                key: accessory_key,
                order: accessory.order,
                accessory: true,
                build: Rc::new(move |context, window, cx| {
                    super::view::ExternalView::accessory(
                        &owner,
                        &id,
                        &context.channel,
                        context.thread.as_deref(),
                        window,
                        cx,
                    )
                }),
            },
            cx,
        );
        added.push(Added::Tool(accessory_key));
    }
    for action in &contributions.account_actions {
        let action_key = key(&action.id);
        let (owner, id, title) = (
            plugin.to_owned(),
            action.id.clone(),
            SharedString::from(action.title.clone()),
        );
        crate::ui::account_actions::register(
            crate::ui::account_actions::AccountAction {
                key: action_key,
                title: title.clone(),
                icon: super::icon(action.icon.as_deref()),
                build: Rc::new(move |close, window, cx| {
                    super::view::ExternalView::dialog(&owner, &id, title.clone(), close, window, cx)
                }),
            },
            cx,
        );
        added.push(Added::AccountAction(action_key));
    }
    for completion in &contributions.completions {
        let completion_key = key(&completion.id);
        let Some(trigger) = completion.trigger.chars().next() else {
            continue;
        };
        let (plugin, id) = (plugin.to_owned(), completion.id.clone());
        composer_completions::register(composer_completions::PluginCompletion {
            key: completion_key,
            trigger,
            order: completion.order,
            query: Rc::new(move |query, place, cx| {
                suggest(&plugin, completion_key, &id, query, place, cx)
            }),
            retry: Rc::new(move || forget_suggestions(completion_key)),
        });
        added.push(Added::Completion(completion_key));
    }
    if let Some(requester) = &requester {
        for policy in &contributions.naming_policies {
            added.push(Added::NamingPolicy(super::naming::register(
                plugin,
                &policy.id,
                requester.clone(),
            )));
        }
    }
    if let Some(requester) = requester {
        for renderer in &contributions.inline_renderers {
            let Ok(pattern) = renderer.regex() else {
                continue;
            };
            let renderer_key = key(&renderer.id);
            crate::ui::renderers::unregister(renderer_key);
            TEXT_KEYS.with_borrow_mut(|keys| keys.insert(renderer_key.to_owned()));
            crate::ui::renderers::register_text(Rc::new(Inline {
                key: renderer_key,
                plugin: plugin.to_owned(),
                id: renderer.id.clone(),
                pattern,
                requester: requester.clone(),
            }));
            added.push(Added::Renderer(renderer_key));
        }
    }
    let owner = intern(name.to_owned());
    for shortcut in &contributions.shortcuts {
        let shortcut_key = key(&shortcut.id);
        let defaults: Vec<crate::shortcuts::Binding> = shortcut
            .keys
            .iter()
            .filter_map(|keys| protocol::Chord::parse(keys).ok())
            .map(|chord| {
                crate::shortcuts::binding(&chord.key, chord.primary, chord.shift, chord.alt)
            })
            .collect();
        let (plugin, id) = (plugin.to_owned(), shortcut.id.clone());
        crate::shortcuts::register_external(crate::shortcuts::ExternalShortcut {
            key: shortcut_key,
            plugin: owner,
            title: intern(shortcut.title.clone()),
            defaults: Box::leak(defaults.into_boxed_slice()),
            order: shortcut.order,
            in_editable: shortcut.allow_in_editable,
            in_modal: shortcut.allow_in_modal,
            repeat: shortcut.repeat,
            run: Rc::new(move |_, cx| {
                ExternalPlugins::notify_running(&plugin, "shortcut", json!({ "shortcut": id }), cx);
            }),
        });
        added.push(Added::Shortcut(shortcut_key));
    }
    added
}
