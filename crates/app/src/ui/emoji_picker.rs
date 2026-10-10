//! The emoji picker (ui.md §8.5, `EmojiPicker.tsx` over emoji-mart):
//! search, categories, frequently used, skin tones and the community's
//! custom emoji. The composer inserts what it picks; message rows react
//! with it.
//!
//! Also here: emoji search shared with the composer's `:` completion
//! (`emoji-search.ts`), and the reaction usage counts behind "Frequently
//! used" and the hover bar's quick reactions (`quick-reactions.ts`).

use super::gif_picker::{self, GifPanel, PickedGif};
use super::media;
use crate::community::{AppState, Community};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::{CustomEmoji, EmojiSnapshot, Partition};
use buzz_ui::{
    ActiveTheme, Icon, IconName, MaterialStyled, SearchField, Tab, TabVariant, Tabs, Tooltip,
    TypeStyled,
};
use emojis::{Emoji, Group, SkinTone, UnicodeVersion};
use gpui_kit::component::input::{Escape, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};

/// The bundled font draws emoji through Unicode 16.
const NEWEST: UnicodeVersion = UnicodeVersion::new(16, 0);
const PER_LINE: usize = 6;
const CELL: f32 = 52.;
const WIDTH: f32 = 360.;
const GRID_HEIGHT: f32 = 236.;
/// emoji-mart's defaults for an empty history, one row.
const FREQUENT_DEFAULTS: [&str; 6] = ["👍", "😀", "😘", "😍", "😆", "😜"];

/// What the user picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Unicode(String),
    Custom(CustomEmoji),
}

impl Pick {
    /// The text it stands for: the emoji itself, or `:code:`.
    pub fn text(&self) -> String {
        match self {
            Pick::Unicode(text) => text.clone(),
            Pick::Custom(emoji) => format!(":{}:", emoji.shortcode),
        }
    }
}

pub struct Picked(pub Pick);

/// Escape in the picker: close it without a pick.
pub struct Dismissed;

// ---------------------------------------------------------------------------
// Search

/// One searchable emoji.
#[derive(Clone, Debug, PartialEq)]
pub enum Candidate {
    Unicode(&'static Emoji),
    Custom(CustomEmoji),
}

impl Candidate {
    pub fn shortcode(&self) -> &str {
        match self {
            Candidate::Unicode(emoji) => emoji.shortcode().unwrap_or_default(),
            Candidate::Custom(emoji) => &emoji.shortcode,
        }
    }

    pub fn pick(&self, tone: Option<SkinTone>) -> Pick {
        match self {
            Candidate::Unicode(emoji) => Pick::Unicode(toned(emoji, tone).as_str().to_string()),
            Candidate::Custom(emoji) => Pick::Custom(emoji.clone()),
        }
    }
}

fn toned(emoji: &'static Emoji, tone: Option<SkinTone>) -> &'static Emoji {
    tone.and_then(|tone| emoji.with_skin_tone(tone))
        .unwrap_or(emoji)
}

struct Indexed {
    emoji: &'static Emoji,
    /// Shortcodes, then the words of the name, lowercase.
    terms: Vec<String>,
}

static INDEX: LazyLock<Vec<Indexed>> = LazyLock::new(|| {
    emojis::iter()
        .filter(|emoji| emoji.unicode_version() <= NEWEST)
        .map(|emoji| Indexed {
            emoji,
            terms: emoji
                .shortcodes()
                .map(str::to_lowercase)
                .chain(
                    emoji
                        .name()
                        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
                        .filter(|word| !word.is_empty())
                        .map(str::to_lowercase),
                )
                .collect(),
        })
        .collect()
});

fn normalize(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
        .collect()
}

/// `shortcodeScore`: exact, prefix, substring, then subsequence span.
fn shortcode_score(query: &str, shortcode: &str) -> Option<(u8, usize)> {
    let target = normalize(shortcode);
    if target == query {
        return Some((0, 0));
    }
    if target.starts_with(query) {
        return Some((1, 0));
    }
    if let Some(index) = target.find(query) {
        return Some((2, index));
    }
    let (mut first, mut last, mut wanted) = (None, 0, query.chars().peekable());
    for (index, c) in target.chars().enumerate() {
        if wanted.peek() == Some(&c) {
            wanted.next();
            first.get_or_insert(index);
            last = index;
        }
    }
    (wanted.peek().is_none()).then(|| (3, last - first.unwrap_or(0)))
}

/// `rankShortcodes`.
fn rank(query: &str, items: &[Candidate]) -> Vec<(Candidate, u8)> {
    let mut ranked: Vec<(Candidate, (u8, usize))> = items
        .iter()
        .filter_map(|item| Some((item.clone(), shortcode_score(query, item.shortcode())?)))
        .collect();
    ranked.sort_by(|(a, sa), (b, sb)| {
        sa.cmp(sb)
            .then(a.shortcode().len().cmp(&b.shortcode().len()))
            .then(a.shortcode().cmp(b.shortcode()))
    });
    ranked
        .into_iter()
        .map(|(item, (tier, _))| (item, tier))
        .collect()
}

/// `searchEmoji`: name and shortcode word matches, then fuzzy shortcode
/// matches, with exact and prefix shortcode matches first.
pub fn search(query: &str, custom: &[CustomEmoji], limit: usize) -> Vec<Candidate> {
    let needle = normalize(query);
    if needle.is_empty() {
        return Vec::new();
    }
    let lower = query.to_lowercase();
    let mut semantic: Vec<(usize, &Indexed)> = INDEX
        .iter()
        .filter_map(|entry| {
            entry
                .terms
                .iter()
                .enumerate()
                .filter(|(_, term)| term.starts_with(&lower))
                .map(|(index, term)| index * 2 + usize::from(*term != lower))
                .min()
                .map(|score| (score, entry))
        })
        .collect();
    semantic.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.emoji.as_str().cmp(y.emoji.as_str())));
    let semantic: Vec<Candidate> = semantic
        .into_iter()
        .map(|(_, entry)| Candidate::Unicode(entry.emoji))
        .collect();
    let customs: Vec<Candidate> = custom.iter().cloned().map(Candidate::Custom).collect();
    let natives: Vec<Candidate> = INDEX
        .iter()
        .map(|entry| Candidate::Unicode(entry.emoji))
        .collect();
    let mut combined = semantic.clone();
    combined.extend(rank(&needle, &customs).into_iter().map(|(item, _)| item));
    combined.extend(
        rank(&needle, &natives)
            .into_iter()
            .map(|(item, _)| item)
            .filter(|item| !semantic.contains(item)),
    );
    let strong: Vec<Candidate> = rank(&needle, &combined)
        .into_iter()
        .filter(|(_, tier)| *tier <= 1)
        .map(|(item, _)| item)
        .collect();
    let mut ranked = strong.clone();
    ranked.extend(combined.into_iter().filter(|item| !strong.contains(item)));
    ranked.truncate(limit);
    ranked
}

// ---------------------------------------------------------------------------
// Usage

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Used {
    emoji: String,
    count: u64,
    used: u64,
}

/// Reaction counts per community, saved in its `quick-reactions` pref.
#[derive(Default)]
struct Usage(HashMap<Partition, Vec<Used>>);

impl Global for Usage {}

const USAGE_PREF: &str = "quick-reactions";
pub const QUICK_DEFAULTS: [&str; 3] = ["👍", "❤️", "😂"];

fn usage<'a>(partition: &Partition, cx: &'a mut App) -> &'a mut Vec<Used> {
    if !cx.has_global::<Usage>() {
        cx.set_global(Usage::default());
    }
    let stored = (!cx.global::<Usage>().0.contains_key(partition)).then(|| {
        cx.global::<AppState>()
            .store
            .partition_pref(partition, USAGE_PREF)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<Used>>(&json).ok())
            .unwrap_or_default()
    });
    let entries = cx
        .global_mut::<Usage>()
        .0
        .entry(partition.clone())
        .or_default();
    if let Some(stored) = stored {
        *entries = stored;
    }
    entries
}

/// Most used first, at most `limit`.
fn most_used(partition: &Partition, limit: usize, cx: &mut App) -> Vec<String> {
    let mut entries = usage(partition, cx).clone();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then(b.used.cmp(&a.used)));
    entries
        .into_iter()
        .take(limit)
        .map(|entry| entry.emoji)
        .collect()
}

/// The hover bar's three reactions: the most used, then the defaults.
/// Custom ones stay only while the palette still has them.
pub fn quick_reactions(
    partition: &Partition,
    emoji: &EmojiSnapshot,
    cx: &mut App,
) -> Vec<SharedString> {
    let mut quick: Vec<SharedString> = Vec::new();
    for value in most_used(partition, 24, cx)
        .into_iter()
        .chain(QUICK_DEFAULTS.iter().map(|s| s.to_string()))
    {
        let custom_gone = value
            .strip_prefix(':')
            .and_then(|code| code.strip_suffix(':'))
            .is_some_and(|code| emoji.get(&code.to_lowercase()).is_none());
        if !custom_gone && !quick.iter().any(|q| q.as_ref() == value) {
            quick.push(value.into());
        }
        if quick.len() == 3 {
            break;
        }
    }
    quick
}

/// Counts one reaction (`recordReaction`).
pub fn record_reaction(partition: &Partition, emoji: &str, cx: &mut App) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let entries = usage(partition, cx);
    match entries.iter_mut().find(|entry| entry.emoji == emoji) {
        Some(entry) => {
            entry.count += 1;
            entry.used = now;
        }
        None => entries.push(Used {
            emoji: emoji.to_string(),
            count: 1,
            used: now,
        }),
    }
    entries.sort_by(|a, b| b.count.cmp(&a.count).then(b.used.cmp(&a.used)));
    entries.truncate(24);
    let json = serde_json::to_string(&entries).expect("usage serializes");
    if let Err(error) = cx
        .global::<AppState>()
        .store
        .set_partition_pref(partition, USAGE_PREF, &json)
    {
        tracing::warn!(%error, "could not save reaction usage");
    }
}

/// Adds the viewer's `content` reaction to `row`, or removes it if they
/// already reacted with it, counting additions for the quick reactions.
pub fn toggle_reaction(
    community: &Entity<Community>,
    channel: &str,
    row: &MessageRow,
    content: &str,
    cx: &mut App,
) {
    let community = community.read(cx);
    let (session, partition) = (community.session.clone(), community.partition());
    let viewer = session.viewer().to_string();
    let own = row
        .reactions
        .iter()
        .find(|group| group.content == content)
        .and_then(|group| group.events.iter().find(|r| r.author.to_hex() == viewer))
        .map(|reaction| reaction.id);
    if own.is_none() {
        record_reaction(&partition, content, cx);
    }
    let (channel, id, content) = (channel.to_string(), row.id, content.to_string());
    cx.foreground_executor()
        .spawn(async move {
            let result = match own {
                Some(reaction) => session.remove(&channel, reaction).await,
                None => session.react(&channel, id, &content).await,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "reaction failed");
            }
        })
        .detach();
}

/// One hover-bar reaction: an emoji, or a custom one's image.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickReaction {
    pub content: SharedString,
    pub image: Option<String>,
}

/// The quick reactions as message rows show them.
pub fn quick_bar(community: &Entity<Community>, cx: &mut App) -> Vec<QuickReaction> {
    let (partition, emoji) = {
        let community = community.read(cx);
        (community.partition(), community.snapshot.emoji.clone())
    };
    quick_reactions(&partition, &emoji, cx)
        .into_iter()
        .map(|content| {
            let image = content
                .strip_prefix(':')
                .and_then(|code| code.strip_suffix(':'))
                .and_then(|code| emoji.get(&code.to_lowercase()))
                .map(|custom| custom.url.clone());
            QuickReaction { content, image }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The picker

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Frequent,
    People,
    Nature,
    Food,
    Activity,
    Travel,
    Objects,
    Symbols,
    Flags,
    Custom,
}

impl Category {
    const ALL: [Category; 10] = [
        Category::Frequent,
        Category::People,
        Category::Nature,
        Category::Food,
        Category::Activity,
        Category::Travel,
        Category::Objects,
        Category::Symbols,
        Category::Flags,
        Category::Custom,
    ];

    fn title(self) -> &'static str {
        match self {
            Category::Frequent => "Frequently used",
            Category::People => "Smileys & People",
            Category::Nature => "Animals & Nature",
            Category::Food => "Food & Drink",
            Category::Activity => "Activity",
            Category::Travel => "Travel & Places",
            Category::Objects => "Objects",
            Category::Symbols => "Symbols",
            Category::Flags => "Flags",
            Category::Custom => "Custom",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Category::Frequent => IconName::Clock,
            Category::People => IconName::Smiley,
            Category::Nature => IconName::Paw,
            Category::Food => IconName::Lemon,
            Category::Activity => IconName::Barbell,
            Category::Travel => IconName::Car,
            Category::Objects => IconName::Bulb,
            Category::Symbols => IconName::Category,
            Category::Flags => IconName::Flag,
            Category::Custom => IconName::Asterisk,
        }
    }

    fn groups(self) -> &'static [Group] {
        match self {
            Category::People => &[Group::SmileysAndEmotion, Group::PeopleAndBody],
            Category::Nature => &[Group::AnimalsAndNature],
            Category::Food => &[Group::FoodAndDrink],
            Category::Activity => &[Group::Activities],
            Category::Travel => &[Group::TravelAndPlaces],
            Category::Objects => &[Group::Objects],
            Category::Symbols => &[Group::Symbols],
            Category::Flags => &[Group::Flags],
            Category::Frequent | Category::Custom => &[],
        }
    }
}

const TONES: [(Option<SkinTone>, &str); 6] = [
    (None, "✋"),
    (Some(SkinTone::Light), "✋🏻"),
    (Some(SkinTone::MediumLight), "✋🏼"),
    (Some(SkinTone::Medium), "✋🏽"),
    (Some(SkinTone::MediumDark), "✋🏾"),
    (Some(SkinTone::Dark), "✋🏿"),
];

#[derive(Clone)]
enum Row {
    Header(&'static str),
    Cells(Vec<Candidate>),
    Empty,
}

pub struct EmojiPicker {
    community: Entity<Community>,
    search: Entity<InputState>,
    list: ListState,
    rows: Rc<Vec<Row>>,
    /// Where each shown category starts.
    sections: Vec<(Category, usize)>,
    active: Category,
    tone: usize,
    tones_open: bool,
    /// The palette the rows were built from.
    built_for: Option<Arc<[CustomEmoji]>>,
    /// The composer's picker also offers GIFs, when the relay does.
    gifs: Option<GifTab>,
    _subscriptions: Vec<Subscription>,
}

struct GifTab {
    panel: Entity<GifPanel>,
    /// `None` until the relay's NIP-11 answers.
    available: Option<bool>,
    showing: bool,
    _check: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<Picked> for EmojiPicker {}
impl EventEmitter<PickedGif> for EmojiPicker {}
impl EventEmitter<Dismissed> for EmojiPicker {}

impl EmojiPicker {
    pub fn new(community: Entity<Community>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji"));
        let list = ListState::new(0, ListAlignment::Top, px(400.));
        let weak = cx.entity().downgrade();
        list.set_scroll_handler(move |event, _, cx| {
            let first = event.visible_range.start;
            let _ = weak.update(cx, |this, cx| {
                let active = this
                    .sections
                    .iter()
                    .rev()
                    .find(|(_, start)| *start <= first)
                    .map(|(category, _)| *category);
                if let Some(active) = active
                    && active != this.active
                {
                    this.active = active;
                    cx.notify();
                }
            });
        });
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.rebuild(cx);
                }
            }),
            cx.observe(&community, |this, _, cx| {
                let custom = this.community.read(cx).snapshot.emoji.entries.clone();
                if this
                    .built_for
                    .as_ref()
                    .is_none_or(|built| !Arc::ptr_eq(built, &custom))
                {
                    this.rebuild(cx);
                }
            }),
        ];
        let mut picker = Self {
            community,
            search,
            list,
            rows: Rc::default(),
            sections: Vec::new(),
            active: Category::Frequent,
            tone: 0,
            tones_open: false,
            built_for: None,
            gifs: None,
            _subscriptions: subscriptions,
        };
        picker.rebuild(cx);
        picker
    }

    /// Clears the search and scrolls to the top, for a fresh open.
    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.tones_open = false;
        if let Some(gifs) = &mut self.gifs {
            gifs.showing = false;
        }
        self.check_gifs(cx);
        self.rebuild(cx);
    }

    /// Adds the GIF tab, shown once the relay says it offers GIF search.
    pub fn enable_gifs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.new(|cx| GifPanel::new(self.community.clone(), window, cx));
        let subscription = cx.subscribe(&panel, |_, _, PickedGif(markdown), cx| {
            cx.emit(PickedGif(markdown.clone()))
        });
        self.gifs = Some(GifTab {
            panel,
            available: None,
            showing: false,
            _check: None,
            _subscription: subscription,
        });
        self.check_gifs(cx);
    }

    /// Asks the relay again unless it already said yes; a "no" or a
    /// failure is not remembered, as a relay can turn GIFs on.
    fn check_gifs(&mut self, cx: &mut Context<Self>) {
        let Some(gifs) = &mut self.gifs else {
            return;
        };
        if gifs.available == Some(true) || gifs._check.is_some() {
            return;
        }
        let community = self.community.clone();
        gifs._check = Some(cx.spawn(async move |this, cx| {
            let available = gif_picker::available(&community, cx).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(gifs) = &mut this.gifs {
                    gifs.available = Some(available);
                    gifs._check = None;
                    cx.notify();
                }
            });
        }));
    }

    fn show_gifs(&mut self, showing: bool, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        let Some(gifs) = &mut self.gifs else {
            return;
        };
        if gifs.showing == showing {
            return;
        }
        gifs.showing = showing;
        if showing {
            gifs.panel
                .update(cx, |panel, cx| panel.open(&query, window, cx));
        } else {
            self.focus(window, cx);
        }
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.search.update(cx, |state, cx| state.focus(window, cx));
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let community = self.community.read(cx);
        let custom = community.snapshot.emoji.entries.clone();
        let partition = community.partition();
        self.built_for = Some(custom.clone());
        let query = self.search.read(cx).value().to_string();
        let mut rows = Vec::new();
        self.sections.clear();
        let push_cells = |rows: &mut Vec<Row>, cells: Vec<Candidate>| {
            for chunk in cells.chunks(PER_LINE) {
                rows.push(Row::Cells(chunk.to_vec()));
            }
        };
        if query.trim().is_empty() {
            for category in Category::ALL {
                let cells: Vec<Candidate> = match category {
                    Category::Frequent => {
                        let mut frequent = most_used(&partition, PER_LINE, cx);
                        for default in FREQUENT_DEFAULTS {
                            if frequent.len() < PER_LINE && !frequent.iter().any(|f| f == default) {
                                frequent.push(default.to_string());
                            }
                        }
                        frequent
                            .iter()
                            .filter_map(|value| match value.strip_prefix(':') {
                                Some(rest) => custom
                                    .iter()
                                    .find(|e| Some(e.shortcode.as_str()) == rest.strip_suffix(':'))
                                    .cloned()
                                    .map(Candidate::Custom),
                                None => emojis::get(value).map(Candidate::Unicode),
                            })
                            .collect()
                    }
                    Category::Custom => custom.iter().cloned().map(Candidate::Custom).collect(),
                    _ => INDEX
                        .iter()
                        .filter(|entry| category.groups().contains(&entry.emoji.group()))
                        .map(|entry| Candidate::Unicode(entry.emoji))
                        .collect(),
                };
                if cells.is_empty() {
                    continue;
                }
                self.sections.push((category, rows.len()));
                rows.push(Row::Header(category.title()));
                push_cells(&mut rows, cells);
            }
        } else {
            let found = search(&query, &custom, 240);
            rows.push(Row::Header("Search results"));
            if found.is_empty() {
                rows.push(Row::Empty);
            }
            push_cells(&mut rows, found);
        }
        self.rows = Rc::new(rows);
        self.list.reset(self.rows.len());
        self.active = Category::Frequent;
        cx.notify();
    }

    fn jump(&mut self, category: Category, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search.read(cx).value().is_empty() {
            self.search
                .update(cx, |state, cx| state.set_value("", window, cx));
            self.rebuild(cx);
        }
        if let Some((_, row)) = self.sections.iter().find(|(c, _)| *c == category) {
            self.list.scroll_to(ListOffset {
                item_ix: *row,
                offset_in_item: px(0.),
            });
            self.active = category;
            cx.notify();
        }
    }

    fn tone(&self) -> Option<SkinTone> {
        TONES[self.tone].0
    }
}

fn cell(
    candidate: &Candidate,
    tone: Option<SkinTone>,
    picker: WeakEntity<EmojiPicker>,
    community: &Entity<Community>,
    cx: &App,
) -> AnyElement {
    let c = cx.colors();
    let pick = candidate.pick(tone);
    let label: SharedString = match candidate {
        Candidate::Unicode(emoji) if emoji.shortcode().is_none() => emoji.name().into(),
        _ => format!(":{}:", candidate.shortcode()).into(),
    };
    let glyph = match candidate {
        Candidate::Unicode(emoji) => div()
            .text_size(px(30.))
            .line_height(px(36.))
            .child(toned(emoji, tone).as_str())
            .into_any_element(),
        Candidate::Custom(emoji) => {
            img(media::image(&emoji.url, community.read(cx).session.media()))
                .size(px(32.))
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        }
    };
    div()
        .id(SharedString::from(format!("emoji-{}", pick.text())))
        .size(px(CELL))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(12.))
        .cursor_pointer()
        .hover(|s| s.bg(c.affordance_popover_selected))
        .tooltip(Tooltip::text(label))
        .child(glyph)
        .on_click(move |_, _, cx| {
            let pick = pick.clone();
            let _ = picker.update(cx, |_, cx| cx.emit(Picked(pick)));
        })
        .into_any_element()
}

impl Render for EmojiPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let rows = self.rows.clone();
        let tone = self.tone();
        let weak = cx.entity().downgrade();
        let community = self.community.clone();
        let grid = list(self.list.clone(), move |index, _, cx| {
            let Some(row) = rows.get(index) else {
                return div().into_any_element();
            };
            let c = cx.colors();
            match row {
                Row::Header(title) => div()
                    .px(px(16.))
                    .pt(px(8.))
                    .pb(px(4.))
                    .text_caption()
                    .text_color(c.text_subtle)
                    .child(*title)
                    .into_any_element(),
                Row::Empty => div()
                    .px(px(16.))
                    .py(px(24.))
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child("No emoji found")
                    .into_any_element(),
                Row::Cells(cells) => div()
                    .flex()
                    .px(px(12.))
                    .children(
                        cells
                            .iter()
                            .map(|candidate| cell(candidate, tone, weak.clone(), &community, cx)),
                    )
                    .into_any_element(),
            }
        })
        .h(px(GRID_HEIGHT))
        .w_full();

        let nav = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .h(px(44.))
            .border_t_1()
            .border_color(c.border_standard)
            .children(self.sections.iter().map(|(category, _)| {
                let category = *category;
                let active = category == self.active;
                div()
                    .id(SharedString::from(format!(
                        "emoji-nav-{}",
                        category.title()
                    )))
                    .size(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .text_color(if active {
                        c.text_standard
                    } else {
                        c.text_subtle
                    })
                    .when(active, |d| d.bg(c.affordance_subtle))
                    .hover(|s| s.bg(c.affordance_subtle_hover))
                    .tooltip(Tooltip::text(category.title()))
                    .child(Icon::new(category.icon()).size(16.))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.jump(category, window, cx)),
                    )
            }))
            .child(div().flex_1())
            .child(self.tone_control(cx));

        let tabs = self
            .gifs
            .as_ref()
            .filter(|gifs| gifs.available == Some(true))
            .map(|gifs| (gifs.showing, gifs.panel.clone()));
        div()
            .id("emoji-picker")
            .capture_action(cx.listener(|_, _: &Escape, _, cx| {
                cx.stop_propagation();
                cx.emit(Dismissed);
            }))
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .floating_surface(&c)
            .overflow_hidden()
            .occlude()
            .text_color(c.text_standard)
            .when_some(tabs.clone(), |d, (showing, _)| {
                // `Tabs variant="panel" label="Media type"`.
                d.child(
                    div().mx(px(12.)).mt(px(12.)).child(
                        Tabs::new(TabVariant::Panel)
                            .tab(
                                Tab::new("picker-emoji", "Emoji")
                                    .selected(!showing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_gifs(false, window, cx)
                                    })),
                            )
                            .tab(Tab::new("picker-gif", "GIF").selected(showing).on_click(
                                cx.listener(|this, _, window, cx| this.show_gifs(true, window, cx)),
                            )),
                    ),
                )
            })
            .map(|d| match tabs {
                Some((true, panel)) => d.child(div().pt(px(12.)).child(panel)),
                _ => d
                    .child(
                        div()
                            .px(px(12.))
                            .pt(px(12.))
                            .pb(px(4.))
                            .child(SearchField::new(&self.search).capsule()),
                    )
                    .child(grid)
                    .child(nav),
            })
    }
}

impl EmojiPicker {
    /// The skin tone dot, which opens the six choices in its place.
    fn tone_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let swatch = |index: usize, cx: &mut Context<Self>| {
            let (_, hand) = TONES[index];
            div()
                .id(("emoji-tone", index))
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .when(index == self.tone, |d| d.bg(c.affordance_subtle))
                .hover(|s| s.bg(c.affordance_subtle_hover))
                .text_size(px(16.))
                .child(hand)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.tones_open {
                        this.tone = index;
                        this.tones_open = false;
                    } else {
                        this.tones_open = true;
                    }
                    cx.notify();
                }))
        };
        if self.tones_open {
            div()
                .flex()
                .gap(px(2.))
                .children((0..TONES.len()).map(|index| swatch(index, cx)))
                .into_any_element()
        } else {
            div()
                .id("emoji-tone-open")
                .child(swatch(self.tone, cx))
                .tooltip(Tooltip::text("Skin tone"))
                .into_any_element()
        }
    }
}

/// The picker as a popup: it takes the pointer and calls `on_dismiss`
/// for a press anywhere else. Callers anchor it.
pub fn emoji_picker(
    picker: &Entity<EmojiPicker>,
    on_dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .occlude()
        .on_mouse_down_out(on_dismiss)
        .child(picker.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn codes(found: &[Candidate]) -> Vec<&str> {
        found.iter().map(Candidate::shortcode).collect()
    }

    #[test]
    fn ranks_prefix_shortcodes_first_like_the_reference() {
        let found = search("smi", &[], 7);
        assert_eq!(
            codes(&found),
            [
                "smile",
                "smirk",
                "smiley",
                "smile_cat",
                "smirk_cat",
                "smiley_cat",
                "smiling_imp"
            ]
        );
        assert_eq!(codes(&search("+1", &[], 1)), ["+1"]);
        assert!(search("", &[], 5).is_empty());
    }

    #[test]
    fn finds_custom_emoji_by_shortcode() {
        let custom = [CustomEmoji {
            shortcode: "partyparrot".into(),
            url: "https://x.test/p.gif".into(),
        }];
        let found = search("partyp", &custom, 3);
        assert_eq!(found[0], Candidate::Custom(custom[0].clone()));
        assert_eq!(found[0].pick(None).text(), ":partyparrot:");
    }

    #[test]
    fn applies_skin_tones_where_they_exist() {
        let wave = Candidate::Unicode(emojis::get("👋").unwrap());
        assert_eq!(wave.pick(Some(SkinTone::Dark)).text(), "👋🏿");
        let grin = Candidate::Unicode(emojis::get("😀").unwrap());
        assert_eq!(grin.pick(Some(SkinTone::Dark)).text(), "😀");
    }
}
