//! Search Buzz (ui.md §2.3; `PageSearch.tsx`, `SearchResults.tsx`): the
//! header's search palette. Typed text matches pages and conversations by
//! name and searches messages on the relay; `from:`, `in:`, `after:` and
//! `before:` narrow the message search (`parseSearchOperators.ts`). An
//! author picked for `from:` shows as a chip in the field.

use super::message::{channel_icon, display_name};
use super::modal::{Dismissed, Leading, OpenChannel, group_label, note, row};
use super::sidebar::{Page, channel_label};
use crate::community::Community;
use buzz_relay_client::members::{Person, match_tier};
use buzz_relay_client::search::{self, Day, match_name};
use buzz_relay_client::{
    ChannelSummary, ChannelType, EventId, MessageSearch, Profile, SearchOutcome,
};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Icon, IconButton, IconButtonSize, IconName, Surface,
    TextField, TypeStyled,
    tokens::{radius, shadow},
};
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use nostr::nips::nip19::ToBech32;
use std::rc::Rc;
use std::time::Duration;

/// Typeahead waits for a brief pause before reading the relay.
const DEBOUNCE: Duration = Duration::from_millis(180);
/// Conversations shown per name group.
const NAME_MATCHES: usize = 8;
/// People and agents the `from:` picker shows.
const AUTHOR_MATCHES: usize = 12;
const PLACEHOLDER: &str = "Search pages, conversations and messages…";

/// Shows a page of the app.
pub type OpenPage = Rc<dyn Fn(Page, &mut Window, &mut App)>;

/// The pages search offers, in buzz-app's order (`presentation.ts`):
/// fixed pages, then others by name, then Settings.
const PAGES: [(Page, &str, IconName); 3] = [
    (Page::Messages, "Messages", IconName::ChatsCircle),
    (Page::Agents, "Agents", IconName::Robot),
    (Page::Settings, "Settings", IconName::Gear),
];

/// What a result row does.
#[derive(Clone)]
enum Action {
    Open(ChannelSummary, Option<EventId>),
    /// Search only this conversation.
    Scope(ChannelSummary),
    /// Replace the query, e.g. to complete an operator.
    Query(String),
    /// Search messages by this author; `prefix` is the text typed before
    /// `from:`.
    Author {
        pubkey: String,
        name: SharedString,
        prefix: String,
    },
    Page(Page),
}

/// The author picked from the `from:` picker.
#[derive(Clone)]
struct Author {
    pubkey: String,
    name: SharedString,
}

struct Entry {
    key: String,
    leading: EntryLeading,
    label: SharedString,
    detail: Option<SharedString>,
    action: Action,
}

enum EntryLeading {
    Icon(IconName),
    Person(String),
}

struct Group {
    label: &'static str,
    entries: Vec<Entry>,
    empty: Option<SharedString>,
}

/// Relay answers for the query they were read for.
#[derive(Default)]
struct Remote {
    query: String,
    scope: Option<String>,
    messages: Option<Result<SearchOutcome, String>>,
    public: Vec<ChannelSummary>,
    /// `in:` named nothing readable.
    unresolved_channel: bool,
    people: Option<Result<Vec<Person>, String>>,
}

pub struct SearchDialog {
    community: Entity<Community>,
    /// The channel open behind the palette, for "Search in …".
    current: Option<ChannelSummary>,
    scope: Option<ChannelSummary>,
    input: Entity<InputState>,
    highlighted: usize,
    remote: Remote,
    loading: bool,
    pending: Option<Task<()>>,
    /// Shown as a chip; the query carries it as `from:<hex>`.
    author: Option<Author>,
    on_open: OpenChannel,
    on_page: OpenPage,
    _subscription: Subscription,
}

impl EventEmitter<Dismissed> for SearchDialog {}

impl SearchDialog {
    pub fn new(
        community: Entity<Community>,
        current: Option<ChannelSummary>,
        on_open: OpenChannel,
        on_page: OpenPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(PLACEHOLDER));
        input.update(cx, |state, cx| state.focus(window, cx));
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.changed(window, cx);
                }
            });
        Self {
            community,
            current,
            scope: None,
            input,
            highlighted: 0,
            remote: Remote::default(),
            loading: false,
            pending: None,
            author: None,
            on_open,
            on_page,
            _subscription: subscription,
        }
    }

    /// The query as searched: the chip's `from:<hex>`, then the typed text
    /// (`SearchChoices.tsx` maps edits back the same way).
    fn query(&self, cx: &App) -> String {
        let text = self.display(cx);
        match &self.author {
            Some(author) => format!("from:{} {}", author.pubkey, text.trim_start()),
            None => text,
        }
    }

    /// What the field shows.
    fn display(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.highlighted = 0;
        self.schedule(window, cx);
        cx.notify();
    }

    /// Reads the relay for the current query after a pause. A newer query
    /// drops the older read.
    fn schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let scope = self.scope.clone();
        let parsed = search::parse(query.trim());
        let author_prompt = search::unfinished_author(&query);
        let needs_relay = author_prompt.is_some()
            || !parsed.text.is_empty()
            || parsed.has_filters()
            || scope.is_some() && !query.trim().is_empty();
        if !needs_relay {
            self.pending = None;
            self.loading = false;
            self.remote = Remote {
                query,
                scope: scope.map(|s| s.id),
                ..Default::default()
            };
            return;
        }
        self.loading = true;
        let community = self.community.read(cx);
        let session = community.session.clone();
        let channels = community.snapshot.channels.clone();
        let profiles = community.snapshot.profiles.clone();
        self.pending = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let mut remote = Remote {
                query: query.clone(),
                scope: scope.as_ref().map(|s| s.id.clone()),
                ..Default::default()
            };
            if let Some(needle) = author_prompt {
                let needle = needle.trim_start_matches('@').to_string();
                remote.people = Some(if needle.is_empty() {
                    Ok(Vec::new())
                } else {
                    session
                        .search_members(&needle, 1)
                        .await
                        .map(|page| page.people)
                });
            } else {
                let mut channel = scope.as_ref().map(|s| s.id.clone());
                if scope.is_none()
                    && let Some(name) = parsed.channel_name()
                {
                    channel = operator_channel(name, &channels, &profiles, &session).await;
                    remote.unresolved_channel = channel.is_none();
                }
                if !remote.unresolved_channel && (!parsed.text.is_empty() || parsed.has_filters()) {
                    let (since, until) = parsed.range(local_midnight);
                    let from = parsed.from.as_deref().map(|value| {
                        if search::is_hex_key(value) {
                            value.to_ascii_lowercase()
                        } else {
                            value.to_string()
                        }
                    });
                    remote.messages = Some(
                        session
                            .search_messages(MessageSearch {
                                text: parsed.text.clone(),
                                from,
                                channel,
                                since,
                                until,
                            })
                            .await,
                    );
                }
                if scope.is_none() && !parsed.text.is_empty() {
                    remote.public = session
                        .search_public_channels(&parsed.text, false)
                        .await
                        .unwrap_or_default();
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.remote = remote;
                this.loading = false;
                cx.notify();
            });
        }));
    }

    fn set_query(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.focus(window, cx);
        });
        self.changed(window, cx);
    }

    fn run(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Open(channel, message) => {
                (self.on_open)(&channel, message, window, cx);
                cx.emit(Dismissed);
            }
            Action::Scope(channel) => {
                self.scope = Some(channel);
                self.input.update(cx, |state, cx| {
                    state.set_placeholder("Search messages…", window, cx);
                });
                self.set_query(String::new(), window, cx);
            }
            Action::Query(text) => self.set_query(text, window, cx),
            Action::Author {
                pubkey,
                name,
                prefix,
            } => {
                self.author = Some(Author { pubkey, name });
                self.set_query(prefix, window, cx);
            }
            Action::Page(page) => {
                (self.on_page)(page, window, cx);
                cx.emit(Dismissed);
            }
        }
    }

    fn unscope(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.scope = None;
        self.input.update(cx, |state, cx| {
            state.set_placeholder(PLACEHOLDER, window, cx);
            state.focus(window, cx);
        });
        self.changed(window, cx);
    }

    /// The chip's ×: the author no longer narrows the search.
    fn remove_author(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.author = None;
        self.input.update(cx, |state, cx| state.focus(window, cx));
        self.changed(window, cx);
    }

    /// The `from:` picker (`SearchResults.tsx:248-351`): the relay's
    /// people and the scoped conversation's members whose names match,
    /// accents and case folded; exact names, then people before agents,
    /// then members.
    fn author_groups(&self, needle: &str, current: bool, cx: &App) -> Vec<Group> {
        let snapshot = &self.community.read(cx).snapshot;
        let display = self.display(cx);
        let prefix = display
            .rfind("from:")
            .map(|at| display[..at].to_string())
            .unwrap_or_default();
        let members: Vec<String> = self
            .scope
            .as_ref()
            .map(|scope| scope.members.clone())
            .unwrap_or_default();
        let mut candidates: Vec<(String, Profile)> = members
            .iter()
            .filter_map(|key| Some((key.clone(), (**snapshot.profiles.get(key)?).clone())))
            .collect();
        let remote = match (current, &self.remote.people) {
            (true, Some(Ok(people))) => Some(people),
            _ => None,
        };
        for person in remote.into_iter().flatten() {
            if !candidates.iter().any(|(key, _)| *key == person.pubkey) {
                candidates.push((person.pubkey.clone(), person.profile.clone()));
            }
        }
        let mut ranked: Vec<(u8, String, Profile)> = candidates
            .into_iter()
            .filter_map(|(key, profile)| {
                let tier = match_tier(&profile.name, needle).filter(|tier| *tier < 4)?;
                Some((tier, key, profile))
            })
            .collect();
        ranked.sort_by_key(|(tier, key, profile)| {
            (
                *tier != 0,
                profile.is_agent,
                !members.contains(key),
                profile.name.to_lowercase(),
            )
        });
        ranked.truncate(AUTHOR_MATCHES);
        let entry = |key: &String, profile: &Profile| Entry {
            key: format!("author:{key}"),
            leading: EntryLeading::Person(key.clone()),
            label: profile.name.clone().into(),
            detail: Some(key.chars().take(12).collect::<String>().into()),
            action: Action::Author {
                pubkey: key.clone(),
                name: profile.name.clone().into(),
                prefix: prefix.clone(),
            },
        };
        let people: Vec<Entry> = ranked
            .iter()
            .filter(|(_, _, profile)| !profile.is_agent)
            .map(|(_, key, profile)| entry(key, profile))
            .collect();
        let agents: Vec<Entry> = ranked
            .iter()
            .filter(|(_, _, profile)| profile.is_agent)
            .map(|(_, key, profile)| entry(key, profile))
            .collect();
        let empty = match (current, &self.remote.people) {
            _ if needle.is_empty() => Some("Type a name to search people."),
            _ if !people.is_empty() || !agents.is_empty() => None,
            (true, Some(Ok(_))) => Some("No matching people. Try a different name."),
            (true, Some(Err(_))) => Some("People search is unavailable. Try a longer name."),
            _ => Some("Searching people…"),
        };
        let mut groups = vec![Group {
            label: "People",
            entries: people,
            empty: empty.map(Into::into),
        }];
        if !agents.is_empty() {
            groups.push(Group {
                label: "Agents",
                entries: agents,
                empty: None,
            });
        }
        groups
    }

    fn groups(&self, cx: &App) -> Vec<Group> {
        let community = self.community.read(cx);
        let snapshot = &community.snapshot;
        let profiles = &snapshot.profiles;
        let query = self.query(cx);
        let parsed = search::parse(query.trim());
        let current = self.remote.query == query
            && self.remote.scope.as_deref() == self.scope.as_ref().map(|s| s.id.as_str());
        let display = self.display(cx);
        let channels: Vec<&ChannelSummary> = snapshot
            .channels
            .iter()
            .filter(|c| !c.archived && (!c.hidden || c.channel_type == Some(ChannelType::Dm)))
            .collect();
        let label = |channel: &ChannelSummary| channel_label(channel, profiles);
        let conversation = |channel: &ChannelSummary, detail: Option<SharedString>| Entry {
            key: format!("channel:{}", channel.id),
            leading: EntryLeading::Icon(IconName::ChatCircle),
            label: label(channel),
            detail: Some(detail.unwrap_or_else(|| conversation_detail(channel).into())),
            action: Action::Open(channel.clone(), None),
        };

        // A `from:` still being typed picks a person.
        if let Some(needle) = search::unfinished_author(&query) {
            let needle = needle.trim_start_matches('@').to_string();
            return self.author_groups(&needle, current, cx);
        }

        // An `after:` or `before:` still being typed picks a day.
        if let Some((prefix, operator)) = date_prompt(&display) {
            let entries = date_presets(Local::now().date_naive())
                .into_iter()
                .map(|(label, day)| {
                    let date = day.format("%Y-%m-%d").to_string();
                    Entry {
                        key: format!("date:{label}"),
                        leading: EntryLeading::Icon(IconName::Calendar),
                        label: label.into(),
                        detail: Some(date.clone().into()),
                        action: Action::Query(format!("{prefix}{operator}:{date} ")),
                    }
                })
                .collect();
            return vec![Group {
                label: "Dates",
                entries,
                empty: None,
            }];
        }

        let messages: Vec<Entry> = match (current, &self.remote.messages) {
            (true, Some(Ok(outcome))) => outcome
                .hits
                .iter()
                .filter_map(|hit| {
                    let channel = snapshot
                        .channels
                        .iter()
                        .chain(outcome.channels.iter())
                        .find(|c| c.id == hit.channel)?;
                    let date = Local
                        .timestamp_opt(hit.created_at as i64, 0)
                        .single()
                        .map(|t| t.format("%-m/%-d/%Y").to_string())
                        .unwrap_or_default();
                    Some(Entry {
                        key: hit.id.to_hex(),
                        leading: EntryLeading::Icon(IconName::ChatCircle),
                        label: hit.preview.clone().into(),
                        detail: Some(
                            format!(
                                "{} · {} · {date}",
                                label(channel),
                                display_name(profiles, &hit.author)
                            )
                            .into(),
                        ),
                        action: Action::Open(channel.clone(), Some(hit.id)),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let message_empty: SharedString = if self.loading || !current {
            "Searching messages…".into()
        } else if matches!(self.remote.messages, Some(Err(_))) {
            "Message search is unavailable.".into()
        } else if self.remote.unresolved_channel {
            "No conversation matches that in: name.".into()
        } else if matches!(&self.remote.messages, Some(Ok(o)) if o.ambiguous_author) {
            "Several people have that name. Type from: and pick one.".into()
        } else if !query.trim().is_empty() {
            "No matching messages in accessible conversations.".into()
        } else if self.scope.is_some() {
            "Type to search messages in this conversation.".into()
        } else {
            "Type to search messages in this community.".into()
        };

        if self.scope.is_some() {
            return vec![Group {
                label: "Most relevant",
                empty: messages.is_empty().then_some(message_empty),
                entries: messages,
            }];
        }

        let scope_action = self.current.as_ref().map(|channel| Entry {
            key: format!("scope:{}", channel.id),
            leading: EntryLeading::Icon(IconName::ChatCircle),
            label: format!(
                "Search {} {}",
                if channel.channel_type == Some(ChannelType::Dm) {
                    "conversation with"
                } else {
                    "in"
                },
                label(channel)
            )
            .into(),
            detail: Some("Search messages in this conversation".into()),
            action: Action::Scope(channel.clone()),
        });
        let page = |(page, label, icon): (Page, &str, IconName)| Entry {
            key: format!("page:{label}"),
            leading: EntryLeading::Icon(icon),
            label: label.to_string().into(),
            detail: None,
            action: Action::Page(page),
        };

        if query.trim().is_empty() {
            let mut recent: Vec<&ChannelSummary> =
                channels.iter().copied().filter(|c| !c.read_only).collect();
            // Newest message first (`lastActivityAt ?? updatedAt`).
            let activity = |c: &ChannelSummary| {
                snapshot
                    .unread
                    .latest
                    .get(&c.id)
                    .copied()
                    .or(c.last_activity_at)
                    .or(c.updated_at)
                    .unwrap_or(0)
            };
            recent.sort_by_key(|c| std::cmp::Reverse(activity(c)));
            let mut groups: Vec<Group> = Vec::new();
            if let Some(entry) = scope_action {
                groups.push(Group {
                    label: "This conversation",
                    entries: vec![entry],
                    empty: None,
                });
            }
            groups.push(Group {
                label: "Recent activity",
                entries: recent
                    .into_iter()
                    .take(4)
                    .map(|c| conversation(c, None))
                    .collect(),
                empty: Some("No recent activity yet.".into()),
            });
            groups.push(Group {
                label: "Actions",
                entries: PAGES.into_iter().map(page).collect(),
                empty: None,
            });
            return groups;
        }

        // Names match by case only, as `matchName` does.
        let needle = parsed.text.trim_start_matches('#').to_lowercase();
        let rank = |name: &str| match_name(name, &needle);
        let mut named: Vec<(&ChannelSummary, u8)> = channels
            .iter()
            .filter(|_| !needle.is_empty())
            .filter_map(|c| Some((*c, rank(&label(c))?)))
            .collect();
        named.sort_by_key(|(c, rank)| (*rank, label(c).to_lowercase()));
        let mut channel_rows: Vec<(u8, Entry)> = named
            .iter()
            .filter(|(c, _)| c.channel_type != Some(ChannelType::Dm))
            .map(|(c, rank)| (*rank, conversation(c, None)))
            .collect();
        if current {
            for channel in &self.remote.public {
                if !named.iter().any(|(c, _)| c.id == channel.id) {
                    let rank = rank(&channel.name).unwrap_or(6);
                    channel_rows.push((rank, conversation(channel, None)));
                }
            }
        }
        channel_rows.sort_by_key(|(rank, _)| *rank);
        let dms: Vec<(u8, Entry)> = named
            .iter()
            .filter(|(c, _)| c.channel_type == Some(ChannelType::Dm))
            .map(|(c, rank)| (*rank, conversation(c, None)))
            .take(NAME_MATCHES)
            .collect();
        let mut pages: Vec<(u8, Entry)> = PAGES
            .into_iter()
            .filter(|_| !needle.is_empty())
            .filter_map(|entry| Some((rank(entry.1)?, page(entry))))
            .collect();
        pages.sort_by_key(|(rank, _)| *rank);
        let mut named_groups: Vec<(u8, Group)> = [
            ("Channels", channel_rows),
            ("Direct messages", dms),
            ("Pages", pages),
        ]
        .into_iter()
        .filter(|(_, rows)| !rows.is_empty())
        .map(|(label, rows)| {
            let best = rows.iter().map(|(rank, _)| *rank).min().unwrap_or(u8::MAX);
            let entries = rows
                .into_iter()
                .map(|(_, entry)| entry)
                .take(NAME_MATCHES)
                .collect();
            (
                best,
                Group {
                    label,
                    entries,
                    empty: None,
                },
            )
        })
        .collect();
        // The group with the best match leads, so Enter opens it.
        named_groups.sort_by_key(|(best, _)| *best);
        let any_named = !named_groups.is_empty();
        let mut groups: Vec<Group> = named_groups.into_iter().map(|(_, g)| g).collect();
        if let Some(entry) = scope_action {
            groups.push(Group {
                label: "This conversation",
                entries: vec![entry],
                empty: None,
            });
        }
        groups.push(Group {
            label: "Most relevant",
            empty: (messages.is_empty() && !any_named).then_some(message_empty),
            entries: messages,
        });
        groups
    }

    fn move_highlight(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count: usize = self.groups(cx).iter().map(|g| g.entries.len()).sum();
        if count == 0 {
            return;
        }
        let next = (self.highlighted as isize + delta).rem_euclid(count as isize);
        self.highlighted = next as usize;
        cx.notify();
    }

    fn run_highlighted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action = self
            .groups(cx)
            .into_iter()
            .flat_map(|g| g.entries)
            .nth(self.highlighted)
            .map(|entry| entry.action);
        if let Some(action) = action {
            self.run(action, window, cx);
        }
    }
}

/// Resolves an `in:` value: a channel id, or a conversation name, joined
/// first and then public.
async fn operator_channel(
    name: &str,
    channels: &[ChannelSummary],
    profiles: &buzz_relay_client::Profiles,
    session: &buzz_relay_client::Session,
) -> Option<String> {
    if search::is_channel_uuid(name) {
        let id = name.to_ascii_lowercase();
        if channels.iter().any(|c| c.id == id) {
            return Some(id);
        }
        return session
            .resolve_channels(vec![id])
            .await
            .ok()?
            .into_iter()
            .next()
            .map(|c| c.id);
    }
    let wanted = name.to_lowercase();
    if let Some(channel) = channels.iter().find(|c| {
        c.name.to_lowercase() == wanted || channel_label(c, profiles).to_lowercase() == wanted
    }) {
        return Some(channel.id.clone());
    }
    session
        .search_public_channels(name, true)
        .await
        .ok()?
        .into_iter()
        .next()
        .map(|c| c.id)
}

/// The Unix time of a day's local midnight.
fn local_midnight(day: Day) -> Option<u64> {
    let date = NaiveDate::from_ymd_opt(day.year, day.month, day.day)?;
    let midnight = Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .earliest()?;
    u64::try_from(midnight.timestamp()).ok()
}

/// The text before an unfinished `after:`/`before:` token, and which one.
fn date_prompt(query: &str) -> Option<(String, &'static str)> {
    let token_start = query.rfind(char::is_whitespace).map_or(0, |at| at + 1);
    let token = &query[token_start..];
    let (operator, value) = token.split_once(':')?;
    let operator = match operator.to_ascii_lowercase().as_str() {
        "after" => "after",
        "before" => "before",
        _ => return None,
    };
    Day::parse(value)
        .is_none()
        .then(|| (query[..token_start].to_string(), operator))
}

/// Today, yesterday, this week, last week and this month (weeks start on
/// Monday).
fn date_presets(today: NaiveDate) -> Vec<(&'static str, NaiveDate)> {
    let monday = today - chrono::Days::new(u64::from(today.weekday().num_days_from_monday()));
    vec![
        ("Today", today),
        ("Yesterday", today.pred_opt().unwrap_or(today)),
        ("This week", monday),
        ("Last week", monday - chrono::Days::new(7)),
        ("This month", today.with_day(1).unwrap_or(today)),
    ]
}

fn conversation_detail(channel: &ChannelSummary) -> &'static str {
    if channel.archived {
        "Archived channel"
    } else if channel.read_only && !channel.cached {
        "Public channel · not joined"
    } else {
        match channel.channel_type {
            Some(ChannelType::Dm) => "Direct message",
            Some(ChannelType::Session) => "Session",
            _ => "Conversation",
        }
    }
}

impl SearchDialog {
    /// The search field (`SearchChoices.tsx`): magnifier, the author chip
    /// when one is picked, the text, and a clear button.
    fn field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let chip = self.author.as_ref().map(|author| {
            let npub = nostr::prelude::PublicKey::from_hex(&author.pubkey)
                .ok()
                .and_then(|key| key.to_bech32().ok())
                .unwrap_or_else(|| author.pubkey.clone());
            div()
                .id("search-author-chip")
                .max_w(px(240.))
                .px(rems(0.5))
                .py(rems(0.125))
                .rounded(radius::CHIP)
                .bg(c.affordance_subtle)
                .hover(|s| s.bg(c.affordance_subtle_hover))
                .text_body_sm()
                .text_color(c.text_link)
                .truncate()
                .cursor_pointer()
                .tooltip(buzz_ui::Tooltip::text(npub))
                .child(format!("× from:@{}", author.name))
                .on_click(cx.listener(|this, _, window, cx| this.remove_author(window, cx)))
        });
        let input = self.input.clone();
        let clear = (!self.display(cx).is_empty()).then(|| {
            IconButton::new("search-clear", IconName::X)
                .size(IconButtonSize::Sm)
                .surface(Surface::Floating)
                .on_click(move |_, window, cx| {
                    input.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                })
        });
        TextField::new(&self.input)
            .surface(Surface::Floating)
            .leading(
                div()
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .child(Icon::new(IconName::MagnifyingGlass).size(16.))
                    .children(chip),
            )
            .when_some(clear, |this, clear| this.trailing(clear))
    }
}

impl Render for SearchDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let groups = self.groups(cx);
        let count: usize = groups.iter().map(|g| g.entries.len()).sum();
        if count > 0 && self.highlighted >= count {
            self.highlighted = count - 1;
        }
        let community = self.community.read(cx);
        let profiles = community.snapshot.profiles.clone();
        let media = community.session.media().clone();
        let mut index = 0;
        let mut results = div().flex().flex_col().gap(rems(0.75));
        for (n, group) in groups.into_iter().enumerate() {
            let mut section = div().flex().flex_col().when(n > 0, |this| {
                this.border_t_1()
                    .border_color(c.border_standard)
                    .pt(rems(0.5))
            });
            section = section.child(group_label(group.label, cx));
            let empty = group.entries.is_empty().then_some(group.empty).flatten();
            for entry in group.entries {
                let leading = match &entry.leading {
                    EntryLeading::Icon(icon) => Leading::Icon(*icon),
                    EntryLeading::Person(pubkey) => {
                        let name = display_name(&profiles, pubkey);
                        let mut avatar = Avatar::new(name).size(AvatarSize::Px(20.));
                        if let Some(picture) = profiles.get(pubkey).and_then(|p| p.picture.clone())
                        {
                            avatar = avatar.image(super::media::avatar(&picture, &media));
                        }
                        Leading::Element(avatar.into_any_element())
                    }
                };
                let action = entry.action.clone();
                let position = index;
                section =
                    section.child(
                        row(
                            SharedString::from(format!("search-{}", entry.key)),
                            leading,
                            entry.label,
                            entry.detail,
                            position == self.highlighted,
                            cx,
                        )
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.highlighted != position {
                                this.highlighted = position;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.run(action.clone(), window, cx),
                        )),
                    );
                index += 1;
            }
            if let Some(empty) = empty {
                section = section.child(note(empty, cx));
            }
            results = results.child(section);
        }

        let top = (window.viewport_size().height * 0.17).clamp(px(48.), px(160.));
        let width = (window.viewport_size().width - px(32.)).min(px(640.));
        let title = if self.scope.is_some() {
            "Search this conversation"
        } else {
            "Search Buzz"
        };
        let scope = self.scope.as_ref().map(|channel| {
            let label = channel_label(channel, &profiles);
            div().flex().child(
                div()
                    .id("search-scope")
                    .flex()
                    .items_center()
                    .gap(rems(0.5))
                    .mb(rems(0.5))
                    .px(rems(0.5))
                    .py(rems(0.25))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(c.border_standard)
                    .bg(c.affordance_subtle)
                    .text_body_sm()
                    .cursor_pointer()
                    .child(buzz_ui::Icon::new(channel_icon(channel)).size(14.))
                    .child(label)
                    .child(buzz_ui::Icon::new(IconName::X).size(14.))
                    .on_click(cx.listener(|this, _, window, cx| this.unscope(window, cx))),
            )
        });
        div()
            .id("search-backdrop")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(top)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(Dismissed)),
            )
            .child(
                div()
                    .id("search-palette")
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .w(width)
                    .max_h(window.viewport_size().height - top - px(16.))
                    .p(rems(1.5))
                    .bg(c.surface_popover)
                    .border_1()
                    .border_color(c.border_standard)
                    .rounded(buzz_ui::tokens::radius::PANEL)
                    .shadow(shadow::sm())
                    .text_color(c.text_standard)
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "down" => this.move_highlight(1, cx),
                            "up" => this.move_highlight(-1, cx),
                            "enter" => this.run_highlighted(window, cx),
                            "escape" => cx.emit(Dismissed),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .child(div().text_label().child(title))
                            .child(
                                IconButton::new("search-close", IconName::X)
                                    .size(IconButtonSize::Sm)
                                    .surface(Surface::Floating)
                                    .tooltip("Close search")
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
                            ),
                    )
                    .child(div().pt(rems(1.5)).children(scope).child(self.field(cx)))
                    .child(
                        div()
                            .id("search-results")
                            .flex_1()
                            .min_h_0()
                            .max_h(window.viewport_size().height * 0.55)
                            .overflow_y_scroll()
                            .mt(rems(0.5))
                            .mx(rems(-0.5))
                            .px(rems(0.5))
                            .py(rems(0.25))
                            .child(results),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn recognizes_unfinished_date_tokens() {
        assert_eq!(
            date_prompt("deploy after:"),
            Some(("deploy ".to_string(), "after"))
        );
        assert_eq!(date_prompt("BEFORE:20"), Some((String::new(), "before")));
        assert_eq!(date_prompt("after:2026-01-02"), None);
        assert_eq!(date_prompt("in:general"), None);
    }

    #[test]
    fn date_presets_start_weeks_on_monday() {
        let thursday = NaiveDate::from_ymd_opt(2026, 10, 8).unwrap();
        let presets = date_presets(thursday);
        assert_eq!(presets[1].1, NaiveDate::from_ymd_opt(2026, 10, 7).unwrap());
        assert_eq!(presets[2].1, NaiveDate::from_ymd_opt(2026, 10, 5).unwrap());
        assert_eq!(presets[3].1, NaiveDate::from_ymd_opt(2026, 9, 28).unwrap());
        assert_eq!(presets[4].1, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
    }

    #[test]
    fn pages_follow_buzz_app_order() {
        let labels: Vec<&str> = PAGES.iter().map(|(_, label, _)| *label).collect();
        assert_eq!(labels, ["Messages", "Agents", "Settings"]);
        assert_eq!(match_name("Settings", "set"), Some(1));
    }
}
