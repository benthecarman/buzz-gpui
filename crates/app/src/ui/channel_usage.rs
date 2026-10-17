//! Channel usage (`buzz.channel-usage`, buzz-app's
//! `bundled/channel-usage`): "View channel usage" in the channel's menu
//! opens a tab of the token and cost counters agents reported for this
//! channel, read from the viewer's saved turn metrics.
//!
//! Usage is attributed by the channel an agent reported, never by thread.
//! The archive scan stops at 2,000 records; the projection and totals are
//! `buzz_relay_client::usage`.

use crate::community::{AppState, Community, IdentityState};
use crate::ui::channel_panels::{self, ChannelPanel, MenuEntry, Opening};
use crate::ui::overlay::{self, Align, Dismiss};
use crate::ui::settings::agent_archive;
use buzz_relay_client::ChannelSummary;
use buzz_relay_client::archive;
use buzz_relay_client::usage::{
    self, Counters, MAX_RECORDS, Projection, SavedMetrics, Scan, UsageRecord, UsageSession,
    aggregate_session_usage, project_usage,
};
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, Icon, IconName, Menu, MenuItem, MenuSize, TypeStyled,
    tokens::radius, tokens::space,
};
use chrono::{Local, TimeZone};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const KEY: &str = "buzz.channel-usage/usage";

pub fn init(cx: &mut App) {
    channel_panels::register(
        ChannelPanel {
            key: KEY,
            order: 0,
            title: "Usage",
            icon: IconName::Browser,
            matches: channel_panels::matches(|_| false),
            launcher: None,
            menu: Some(MenuEntry {
                label: "View channel usage",
                eligible: std::rc::Rc::new(eligible),
            }),
            build: channel_panels::build(|opening, _, cx| {
                cx.new(|cx| ChannelUsage::new(opening, cx)).into()
            }),
        },
        cx,
    );
}

/// Who may see a channel's usage (`eligible`): a member of a live,
/// writable, unarchived channel, with the archive available.
pub fn allowed(channel: &ChannelSummary, viewer: &str, archive: bool) -> bool {
    archive
        && !channel.cached
        && !channel.read_only
        && !channel.archived
        && channel.members.iter().any(|member| member == viewer)
}

fn eligible(channel: &ChannelSummary, cx: &App) -> bool {
    let Some(community) = AppState::current(cx) else {
        return false;
    };
    let viewer = community.read(cx).session.viewer().to_string();
    allowed(channel, &viewer, agent_archive(cx).is_some())
}

// Display, as buzz-app formats it (en-US).

const DASH: &str = "—";

/// `toLocaleString("en-US")`, or "—" when not reported.
fn number(value: Option<u64>) -> String {
    let Some(value) = value else {
        return DASH.into();
    };
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `Number.prototype.toFixed`: the exact binary value rounded half up.
fn to_fixed(value: f64, digits: usize) -> String {
    let exact = format!("{value:.1100}");
    let (whole, fraction) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = whole.bytes().chain(fraction.bytes().take(digits)).collect();
    if fraction.as_bytes().get(digits).is_some_and(|d| *d >= b'5') {
        let mut index = kept.len();
        loop {
            if index == 0 {
                kept.insert(0, b'1');
                break;
            }
            index -= 1;
            if kept[index] == b'9' {
                kept[index] = b'0';
            } else {
                kept[index] += 1;
                break;
            }
        }
    }
    let split = kept.len() - digits;
    let text = String::from_utf8(kept).unwrap_or_default();
    if digits == 0 {
        text
    } else {
        format!("{}.{}", &text[..split], &text[split..])
    }
}

/// Dollars: four places under a cent, otherwise two.
fn usd(value: Option<f64>) -> String {
    match value {
        None => DASH.into(),
        Some(value) => format!(
            "${}",
            to_fixed(value, if value > 0. && value < 0.01 { 4 } else { 2 })
        ),
    }
}

/// `new Date(ms).toLocaleString()` in en-US: "10/10/2026, 3:04:05 PM".
fn date(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%-m/%-d/%Y, %-I:%M:%S %p").to_string())
        .unwrap_or_else(|| "Invalid Date".into())
}

/// A session's running total, for the agent picker.
fn total(record: Option<&UsageRecord>) -> String {
    let counters = record.and_then(|record| record.cumulative);
    match counters {
        Some(c) if c.total_tokens.is_some() => format!("{} tokens", number(c.total_tokens)),
        Some(c) if c.input_tokens.is_some() || c.output_tokens.is_some() => format!(
            "Input {} / Output {}",
            number(c.input_tokens),
            number(c.output_tokens)
        ),
        _ => DASH.into(),
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// "Session 2 · 3 turns · 10/10/2026, 3:04:05 PM".
fn session_option(entry: &UsageSession, index: usize) -> String {
    format!(
        "{} {} · {} · {}",
        if entry.session_id.is_some() {
            "Session"
        } else {
            "Unidentified session"
        },
        index + 1,
        plural(entry.turns.len(), "turn", "turns"),
        entry
            .turns
            .first()
            .map_or("No turns".into(), |turn| date(turn.timestamp))
    )
}

/// "#3", "Session setup" or "—".
fn turn_label(row: &UsageRecord) -> String {
    if row
        .turn_id
        .as_deref()
        .is_some_and(|id| id.ends_with(":initial"))
    {
        "Session setup".into()
    } else {
        row.turn_seq.map_or(DASH.into(), |seq| format!("#{seq}"))
    }
}

/// What the view shows for its choices.
#[derive(Debug, PartialEq)]
struct Model<'a> {
    /// Agents with usage here, newest session first.
    agents: Vec<&'a str>,
    /// The chosen agent, if it still has usage.
    agent: Option<&'a str>,
    /// Its sessions.
    groups: Vec<&'a UsageSession>,
    /// The chosen session; the only one when there is just one.
    group: Option<&'a UsageSession>,
}

fn model<'a>(projection: &'a Projection, agent: Option<&str>, selected: Option<&str>) -> Model<'a> {
    let mut agents: Vec<&str> = Vec::new();
    for group in &projection.groups {
        if !agents.contains(&group.agent.as_str()) {
            agents.push(&group.agent);
        }
    }
    let agent = agent.and_then(|agent| agents.iter().copied().find(|a| *a == agent));
    let groups: Vec<&UsageSession> = projection
        .groups
        .iter()
        .filter(|group| Some(group.agent.as_str()) == agent)
        .collect();
    let group = groups
        .iter()
        .copied()
        .find(|group| Some(group.key.as_str()) == selected)
        .or_else(|| (groups.len() == 1).then(|| groups[0]));
    Model {
        agents,
        agent,
        groups,
        group,
    }
}

/// The agent picker's option for `agent`.
fn agent_option(projection: &Projection, agent: &str, name: &str) -> String {
    let sessions: Vec<&UsageSession> = projection
        .groups
        .iter()
        .filter(|group| group.agent == agent)
        .collect();
    let summary = match sessions.as_slice() {
        [only] => total(only.latest.as_ref()),
        _ => format!("{} sessions", sessions.len()),
    };
    format!("{name} · {summary}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Loading,
    Ready,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Agent,
    Session,
}

pub struct ChannelUsage {
    community: Entity<Community>,
    channel: String,
    /// [`allowed`] for the channel as the sidebar knows it now.
    allowed: bool,
    status: Status,
    projection: Projection,
    skipped: usize,
    partial: bool,
    /// Bumped by every reset; a scan of an older generation is dropped.
    generation: Arc<AtomicU64>,
    pending: bool,
    agent: Option<String>,
    selected: Option<String>,
    /// The turn whose details are open.
    expanded: Option<String>,
    /// Open "Identifiers and provenance" disclosures, by turn.
    provenance: HashSet<String>,
    /// "Session identity and pricing provenance" is open.
    session_details: bool,
    picker: Option<Picker>,
    agent_focus: FocusHandle,
    _load: Option<Task<()>>,
    _observe: Subscription,
}

impl ChannelUsage {
    fn new(opening: Opening, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&opening.community, |this, _, cx| this.sync(cx));
        let mut this = Self {
            community: opening.community,
            channel: opening.channel,
            allowed: false,
            status: Status::Loading,
            projection: Projection::default(),
            skipped: 0,
            partial: false,
            generation: Arc::default(),
            pending: false,
            agent: None,
            selected: None,
            expanded: None,
            provenance: HashSet::new(),
            session_details: false,
            picker: None,
            agent_focus: cx.focus_handle(),
            _load: None,
            _observe: observe,
        };
        this.sync(cx);
        this
    }

    /// Whether the channel is still listed and usable here, as of now.
    fn is_allowed(&self, cx: &App) -> bool {
        let community = self.community.read(cx);
        let snapshot = &community.snapshot;
        !snapshot.cached
            && snapshot
                .channels
                .iter()
                .find(|channel| channel.id == self.channel)
                .is_some_and(|channel| {
                    allowed(
                        channel,
                        community.session.viewer(),
                        agent_archive(cx).is_some(),
                    )
                })
    }

    /// Losing access hides and forgets what was read; regaining it reads
    /// again.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let allowed = self.is_allowed(cx);
        if allowed == self.allowed {
            return;
        }
        self.allowed = allowed;
        self.reset();
        if allowed {
            self.load(cx);
        }
        cx.notify();
    }

    fn reset(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.pending = false;
        self._load = None;
        self.status = Status::Loading;
        self.projection = Projection::default();
        self.skipped = 0;
        self.partial = false;
    }

    fn source(&self, cx: &App) -> Option<SavedMetrics> {
        let IdentityState::Ready(identity) = &cx.global::<AppState>().identity else {
            return None;
        };
        let community = self.community.read(cx);
        if identity.pubkey_hex() != community.session.viewer() {
            return None;
        }
        Some(SavedMetrics {
            archive: agent_archive(cx)?,
            identity: identity.clone(),
            community: archive::community_key(&community.origin),
        })
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if !self.allowed || self.pending {
            return;
        }
        let Some(source) = self.source(cx) else {
            self.status = Status::Error;
            return;
        };
        self.pending = true;
        self.status = Status::Loading;
        let generation = self.generation.clone();
        let token = generation.load(Ordering::SeqCst);
        let scan = cx.background_spawn(async move {
            usage::scan(&source, || generation.load(Ordering::SeqCst) == token)
        });
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation.load(Ordering::SeqCst) != token {
                    return;
                }
                this.pending = false;
                match result {
                    Scan::Ready {
                        records,
                        skipped,
                        partial,
                        ..
                    } => {
                        this.projection = project_usage(&records, &this.channel);
                        this.skipped = skipped;
                        this.partial = partial;
                        this.status = Status::Ready;
                    }
                    Scan::Error => this.status = Status::Error,
                    Scan::Cancelled => return,
                }
                cx.notify();
            });
        }));
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reset();
        self.load(cx);
        cx.notify();
    }

    /// The agent's name as this channel knows it, else its short npub.
    fn name(&self, agent: &str, cx: &App) -> String {
        let profile = self
            .community
            .read(cx)
            .snapshot
            .profiles
            .get(agent)
            .cloned();
        match profile {
            Some(profile) if !profile.name_is_fallback && !profile.name.is_empty() => {
                profile.name.clone()
            }
            _ => super::keys::format_public_key(agent).unwrap_or_else(|| "Agent".into()),
        }
    }

    fn dismiss(cx: &mut Context<Self>) -> Dismiss {
        let weak = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.picker = None;
                cx.notify();
            });
        })
    }

    fn choose_agent(&mut self, agent: String, cx: &mut Context<Self>) {
        self.agent = Some(agent);
        self.selected = None;
        self.picker = None;
        self.forget_details();
        cx.notify();
    }

    fn choose_session(&mut self, key: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(key.as_str()) {
            self.forget_details();
        }
        self.selected = Some(key);
        self.picker = None;
        cx.notify();
    }

    /// A different session shows its turns closed (`key={group.key}`).
    fn forget_details(&mut self) {
        self.expanded = None;
        self.provenance.clear();
        self.session_details = false;
    }

    /// A `variant="compact"` select: an outline button showing the chosen
    /// option, opening a menu of every option.
    fn compact_select(
        &self,
        which: Picker,
        value: Option<String>,
        placeholder: &'static str,
        options: Vec<(String, String, bool)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let c = *cx.colors();
        let open = self.picker == Some(which);
        let ix = c.interaction(buzz_ui::Surface::default());
        let id = match which {
            Picker::Agent => "usage-agent",
            Picker::Session => "usage-session",
        };
        let trigger = div()
            .id(id)
            .flex()
            .items_center()
            .justify_between()
            .gap(rems(0.5))
            .min_w_0()
            .max_w_full()
            .min_h(rems(2.))
            .px(rems(1.))
            .py(rems(0.375))
            .rounded(radius::CAPSULE)
            .border_1()
            // The value and caret identify it; its boundary stays quiet.
            .border_color(c.border_standard)
            .text_label_sm()
            .text_color(c.text_standard)
            .cursor_pointer()
            .when(open, |this| this.bg(ix.pressed))
            .hover(|s| s.bg(ix.hover))
            .active(|s| s.bg(ix.pressed))
            .when(which == Picker::Agent, |this| {
                this.track_focus(&self.agent_focus)
            })
            .child(match value {
                Some(value) => div().min_w_0().truncate().child(value),
                None => div()
                    .min_w_0()
                    .truncate()
                    .text_color(c.text_metadata)
                    .child(placeholder),
            })
            .child(
                Icon::new(if open {
                    IconName::CaretUp
                } else {
                    IconName::CaretDown
                })
                .size(14.),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.picker = (this.picker != Some(which)).then_some(which);
                cx.notify();
            }));
        let menu = open.then(|| {
            let items = options
                .into_iter()
                .enumerate()
                .map(|(index, (value, label, checked))| {
                    MenuItem::new((id, index), label)
                        .checked(checked)
                        .on_click(cx.listener(move |this, _, _, cx| match which {
                            Picker::Agent => this.choose_agent(value.clone(), cx),
                            Picker::Session => this.choose_session(value.clone(), cx),
                        }))
                });
            overlay::dropdown(
                window,
                Align::Start,
                // At least as wide as the trigger (`--anchor-width`): the
                // session trigger fills its 24rem column.
                div()
                    .min_w(rems(match which {
                        Picker::Agent => 11.25,
                        Picker::Session => 20.,
                    }))
                    .child(
                        Menu::new().size(MenuSize::Wide).child(
                            div()
                                .id(SharedString::from(format!("{id}-options")))
                                .flex()
                                .flex_col()
                                .max_h(rems(23.5))
                                .overflow_y_scroll()
                                .children(items),
                        ),
                    ),
                Self::dismiss(cx),
            )
        });
        div().relative().min_w_0().child(trigger).children(menu)
    }
}

/// `.details h2, .details h3`: label-sized, body weight.
fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .my(space::S2)
        .text_size(rems(1.))
        .line_height(rems(1.428_571_4))
        .font_weight(FontWeight::NORMAL)
        .child(text.into())
}

fn paragraph(text: impl Into<SharedString>) -> Div {
    div().child(text.into())
}

/// Tabular figures, so counters line up.
fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// The `.breakdown` list of counters.
fn breakdown(counts: Option<&Counters>) -> Div {
    let rows = [
        (
            "Provider total tokens",
            number(counts.and_then(|c| c.total_tokens)),
        ),
        ("Input", number(counts.and_then(|c| c.input_tokens))),
        ("Output", number(counts.and_then(|c| c.output_tokens))),
        (
            "Cache reads",
            number(counts.and_then(|c| c.cache_read_tokens)),
        ),
        (
            "Cache writes",
            number(counts.and_then(|c| c.cache_write_tokens)),
        ),
        ("Estimated USD", usd(counts.and_then(|c| c.cost_usd))),
    ];
    pairs(rows.into_iter().map(|(term, value)| (term.into(), value)))
}

/// A two-column `dl`: terms, and values flush right.
fn pairs(rows: impl Iterator<Item = (SharedString, String)>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(space::S1)
        .max_w(rems(32.))
        .children(rows.map(|(term, value)| {
            div()
                .flex()
                .gap(space::S2)
                .child(div().flex_1().min_w(rems(7.)).child(term))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_right()
                        .font_features(tabular())
                        .child(value),
                )
        }))
}

/// A `<details>` disclosure.
fn details(
    id: impl Into<ElementId>,
    summary: &'static str,
    open: bool,
    content: impl FnOnce() -> AnyElement,
    toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(rems(0.25))
                .cursor_pointer()
                .child(
                    Icon::new(if open {
                        IconName::CaretDown
                    } else {
                        IconName::CaretRight
                    })
                    .size(12.),
                )
                .child(summary)
                .on_click(toggle),
        )
        .when(open, |this| this.child(content()))
}

impl ChannelUsage {
    fn turns(&self, group: &UsageSession, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let rows = group.turns.iter().map(|row| {
            let open = self.expanded.as_deref() == Some(row.id.as_str());
            let id = row.id.clone();
            let head = div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap(space::S1)
                .font_features(tabular())
                .child(div().child(turn_label(row)))
                .child(div().text_color(c.text_subtle).child(date(row.timestamp)))
                .child(div().child(number(row.turn.and_then(|t| t.total_tokens))))
                .child(
                    Button::new(
                        SharedString::from(format!("usage-turn-{id}")),
                        format!(
                            "{} · {}",
                            usd(row.turn.and_then(|t| t.cost_usd)),
                            if open { "−" } else { "+" }
                        ),
                    )
                    .ghost()
                    .size(ButtonSize::Xs)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.expanded = if this.expanded.as_deref() == Some(id.as_str()) {
                            None
                        } else {
                            Some(id.clone())
                        };
                        cx.notify();
                    })),
                );
            let body = open.then(|| {
                let usage = if row.conflict {
                    paragraph("Conflicting turn records. Per-turn usage unavailable.")
                } else if let Some(turn) = &row.turn {
                    breakdown(Some(turn))
                } else {
                    paragraph("Per-turn usage unavailable.")
                };
                let shown = self.provenance.contains(&row.id);
                let id = row.id.clone();
                let reported =
                    |value: &Option<String>| value.clone().unwrap_or_else(|| "Not reported".into());
                let facts = vec![
                    ("Session".into(), reported(&row.session_id)),
                    ("Turn".into(), reported(&row.turn_id)),
                    (
                        "Harness / model".into(),
                        format!("{} / {}", row.harness, reported(&row.model)),
                    ),
                    ("Stop reason".into(), reported(&row.stop_reason)),
                    ("Pricing identity".into(), reported(&row.pricing)),
                ];
                div().child(usage).child(details(
                    SharedString::from(format!("usage-provenance-{}", row.id)),
                    "Identifiers and provenance",
                    shown,
                    move || pairs(facts.into_iter()).into_any_element(),
                    cx.listener(move |this, _, _, cx| {
                        if !this.provenance.remove(&id) {
                            this.provenance.insert(id.clone());
                        }
                        cx.notify();
                    }),
                ))
            });
            div().py(space::S1).child(head).children(body)
        });
        div()
            .child(heading(format!(
                "Recorded turns · {} · newest first",
                group.turns.len()
            )))
            .child(div().children(rows))
    }

    fn session(&self, group: &UsageSession, cx: &mut Context<Self>) -> Div {
        let latest = group.latest.as_ref();
        let first = group.turns.first();
        let harness = latest
            .map(|l| l.harness.clone())
            .or_else(|| first.map(|t| t.harness.clone()))
            .unwrap_or_else(|| "Not reported".into());
        let model = latest
            .and_then(|l| l.model.clone())
            .or_else(|| first.and_then(|t| t.model.clone()))
            .unwrap_or_else(|| "Not reported".into());
        let identity = format!(
            "{} · {}",
            group.session_id.as_deref().unwrap_or("Not reported"),
            latest
                .and_then(|l| l.pricing.as_deref())
                .unwrap_or("Not reported")
        );
        div()
            .child(paragraph(format!(
                "{} · {harness} / {model}",
                match &group.session_id {
                    Some(id) => format!("Session {id}"),
                    None => "Unidentified session".into(),
                }
            )))
            .child(heading("Latest reported session counters"))
            .child(paragraph(format!(
                "Last reported {}",
                latest.map_or("Not reported".into(), |l| date(l.timestamp))
            )))
            .when(latest.is_none(), |this| {
                this.child(paragraph("Session counters unavailable."))
            })
            .child(breakdown(latest.and_then(|l| l.cumulative.as_ref())))
            .child(paragraph(
                "Cache counts are part of input, not additional tokens.",
            ))
            .child(details(
                "usage-session-details",
                "Session identity and pricing provenance",
                self.session_details,
                move || paragraph(identity).into_any_element(),
                cx.listener(|this, _, _, cx| {
                    this.session_details = !this.session_details;
                    cx.notify();
                }),
            ))
            .child(self.turns(group, cx))
    }
}

impl Render for ChannelUsage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.allowed {
            return div().into_any_element();
        }
        let c = *cx.colors();
        let projection = self.projection.clone();
        let view = model(&projection, self.agent.as_deref(), self.selected.as_deref());
        let names: Vec<(String, String)> = view
            .agents
            .iter()
            .map(|agent| (agent.to_string(), self.name(agent, cx)))
            .collect();
        let name_of = |agent: &str| {
            names
                .iter()
                .find(|(id, _)| id == agent)
                .map(|(_, name)| name.clone())
                .unwrap_or_default()
        };
        let agent_options: Vec<(String, String, bool)> = names
            .iter()
            .map(|(id, name)| {
                (
                    id.clone(),
                    agent_option(&projection, id, name),
                    view.agent == Some(id.as_str()),
                )
            })
            .collect();
        let agent_value = view.agent.and_then(|agent| {
            agent_options
                .iter()
                .find(|(id, _, _)| id == agent)
                .map(|(_, label, _)| label.clone())
        });
        let agent_picker = self.compact_select(
            Picker::Agent,
            agent_value,
            "Select agent",
            agent_options,
            window,
            cx,
        );
        let strip = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(space::S1)
            .py(space::S1)
            .px(space::PANEL_INSET)
            .child(
                div()
                    .text_color(c.text_subtle)
                    .whitespace_nowrap()
                    .child("Channel session usage"),
            )
            .child(div().min_w_0().max_w(rems(20.)).child(agent_picker))
            .when(self.status == Status::Loading, |this| {
                this.child(div().child("Loading saved usage…"))
            })
            .child(
                Button::new("usage-refresh", "Refresh")
                    .ghost()
                    .size(ButtonSize::Xs)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );
        let details = view.agent.map(|agent| {
            let name = name_of(agent);
            let totals = aggregate_session_usage(
                &view.groups.iter().map(|g| (*g).clone()).collect::<Vec<_>>(),
            );
            let count = view.groups.len();
            let session_options = view
                .groups
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    (
                        entry.key.clone(),
                        session_option(entry, index),
                        view.group.is_some_and(|g| g.key == entry.key),
                    )
                })
                .collect::<Vec<_>>();
            let session_value = view.group.and_then(|group| {
                session_options
                    .iter()
                    .find(|(key, _, _)| *key == group.key)
                    .map(|(_, label, _)| label.clone())
            });
            let session_picker = self.compact_select(
                Picker::Session,
                session_value,
                "Select session",
                session_options,
                window,
                cx,
            );
            div()
                // Not shrunk: the owner scrolls the whole column.
                .flex_none()
                .py(space::S2)
                .px(space::PANEL_INSET)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::S1)
                        .child(heading(format!("Session usage · {name}")))
                        .child(
                            Button::new("usage-close", "Close")
                                .ghost()
                                .size(ButtonSize::Xs)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    window.focus(&this.agent_focus, cx);
                                    this.agent = None;
                                    this.picker = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(heading(format!(
                    "Totals across {}",
                    plural(count, "session", "sessions")
                )))
                .child(paragraph(
                    "Latest reported counters per session, not thread attribution. Cache \
                     counts are part of input. — means not reported for every session.",
                ))
                .when(totals.complete < count, |this| {
                    this.child(paragraph(format!(
                        "Incomplete: {} of {count} sessions have trustworthy cumulative \
                         counters.",
                        totals.complete
                    )))
                })
                .child(breakdown(Some(&totals.counters)))
                .child(div().max_w(rems(24.)).mt(space::S2).child(session_picker))
                .children(view.group.map(|group| self.session(group, cx)))
        });
        let incomplete = self.skipped > 0 || self.projection.unreadable > 0;
        let empty = view.agents.is_empty() && self.status == Status::Ready;
        div()
            .id("channel-usage")
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .text_body()
            .text_color(c.text_standard)
            .child(strip)
            .children(details)
            // An unstyled `role="alert"` paragraph in buzz-app.
            .when(self.status == Status::Error, |this| {
                this.child(paragraph(
                    "Could not read saved usage. Use Refresh to retry.",
                ))
            })
            .when(empty, |this| {
                this.child(paragraph(if incomplete {
                    "Some saved metrics could not be read. Usage history is incomplete."
                } else if self.partial {
                    "No matching records in the saved history scanned."
                } else {
                    "No matching records in the saved history searched."
                }))
            })
            .when(!view.agents.is_empty() && incomplete, |this| {
                this.child(paragraph(
                    "Some saved metrics could not be read. Usage history is incomplete.",
                ))
            })
            .when(self.partial, |this| {
                this.child(paragraph(format!(
                    "Partial history: {}-record scan limit reached.",
                    number(Some(MAX_RECORDS as u64))
                )))
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_relay_client::archive::ArchiveRecord;
    use core::prelude::v1::test;
    use serde_json::json;

    #[test]
    fn offers_usage_only_for_an_authorized_live_channel_with_the_archive() {
        let viewer = "a".repeat(64);
        let channel = ChannelSummary {
            id: "channel".into(),
            name: "Channel".into(),
            members: vec![viewer.clone()],
            ..ChannelSummary::default()
        };
        assert!(allowed(&channel, &viewer, true));
        let changes: [fn(&mut ChannelSummary); 4] = [
            |c| c.cached = true,
            |c| c.read_only = true,
            |c| c.archived = true,
            |c| c.members.clear(),
        ];
        for change in changes {
            let mut changed = channel.clone();
            change(&mut changed);
            assert!(!allowed(&changed, &viewer, true));
        }
        assert!(!allowed(&channel, &viewer, false), "no archive");
    }

    #[test]
    fn formats_numbers_dollars_and_dates_as_buzz_app() {
        assert_eq!(number(None), "—");
        assert_eq!(number(Some(0)), "0");
        assert_eq!(number(Some(999)), "999");
        assert_eq!(number(Some(1_234_567)), "1,234,567");
        assert_eq!(usd(None), "—");
        assert_eq!(usd(Some(0.)), "$0.00");
        assert_eq!(usd(Some(0.004)), "$0.0040");
        assert_eq!(usd(Some(0.00005)), "$0.0001", "half up, as toFixed");
        assert_eq!(usd(Some(0.01)), "$0.01");
        assert_eq!(usd(Some(1.005)), "$1.00", "1.005 is just under in binary");
        assert_eq!(usd(Some(0.125)), "$0.13", "exact ties round up");
        assert_eq!(usd(Some(9.999)), "$10.00");
        assert_eq!(usd(Some(1234.5)), "$1234.50");
        let ms = Local
            .with_ymd_and_hms(2026, 10, 10, 15, 4, 5)
            .unwrap()
            .timestamp_millis();
        assert_eq!(date(ms), "10/10/2026, 3:04:05 PM");
    }

    fn key(n: u64) -> String {
        format!("{n:064x}")
    }

    /// `ChannelUsage.test.tsx`'s frame.
    fn frame(agent: u64, session: &str, turn: u64, total: Option<u64>) -> ArchiveRecord {
        ArchiveRecord {
            id: key(agent * 100 + turn + if session == "b" { 50 } else { 0 }),
            agent: key(agent),
            created_at: turn,
            received_at: turn as i64,
            plaintext: json!({
                "harness": "goose",
                "model": "model",
                "timestamp": chrono::DateTime::from_timestamp((turn + 1) as i64, 0).unwrap().to_rfc3339(),
                "channelId": "channel",
                "sessionId": session,
                "turnId": format!("turn-{turn}"),
                "turnSeq": turn,
                "turn": {"inputTokens": 0, "costUsd": 0},
                "cumulative": {"inputTokens": 10, "totalTokens": total, "costUsd": 0},
            })
            .to_string(),
        }
    }

    #[test]
    fn opens_the_chosen_agent_and_requires_a_choice_among_sessions() {
        let projection = project_usage(
            &[
                frame(1, "a", 1, Some(0)),
                frame(1, "b", 2, Some(20)),
                frame(2, "a", 1, Some(30)),
            ],
            "channel",
        );
        let none = model(&projection, None, None);
        assert_eq!(none.agents.len(), 2);
        assert_eq!((none.agent, none.group), (None, None));
        let one = key(1);
        assert_eq!(
            agent_option(&projection, &one, "npub…abc"),
            "npub…abc · 2 sessions"
        );
        assert_eq!(agent_option(&projection, &key(2), "Bot"), "Bot · 30 tokens");
        let chosen = model(&projection, Some(&one), None);
        assert_eq!(chosen.groups.len(), 2);
        assert_eq!(chosen.group, None, "two sessions: nothing chosen yet");
        let totals = aggregate_session_usage(
            &chosen
                .groups
                .iter()
                .map(|g| (*g).clone())
                .collect::<Vec<_>>(),
        );
        assert_eq!(number(totals.counters.total_tokens), "20");
        assert_eq!(number(totals.counters.input_tokens), "20");
        assert_eq!(usd(totals.counters.cost_usd), "$0.00");
        let label = session_option(chosen.groups[0], 0);
        assert!(label.starts_with("Session 1 · 1 turn · "), "{label}");
        let picked = model(&projection, Some(&one), Some(&chosen.groups[1].key));
        assert_eq!(picked.group.map(|g| &g.key), Some(&chosen.groups[1].key));
        let single = model(&projection, Some(&key(2)), None);
        assert!(single.group.is_some(), "one session opens by itself");
        // An agent no longer present is no choice at all.
        assert_eq!(model(&projection, Some(&key(9)), None).agent, None);
    }

    #[test]
    fn keeps_incomplete_totals_unknown_across_a_hundred_sessions() {
        let mut records: Vec<ArchiveRecord> = (0..100u64)
            .map(|index| ArchiveRecord {
                id: key(index + 1000),
                ..frame(1, &format!("session-{index}"), index + 1, Some(index + 1))
            })
            .collect();
        records[0].plaintext = json!({
            "harness": "goose",
            "timestamp": "1970-01-01T00:00:02Z",
            "channelId": "channel",
            "turnSeq": 1,
            "turn": {"inputTokens": 1},
        })
        .to_string();
        let projection = project_usage(&records, "channel");
        let one = key(1);
        assert_eq!(agent_option(&projection, &one, "A"), "A · 100 sessions");
        let view = model(&projection, Some(&one), None);
        let totals =
            aggregate_session_usage(&view.groups.iter().map(|g| (*g).clone()).collect::<Vec<_>>());
        assert_eq!(totals.complete, 99);
        let c = totals.counters;
        let shown = [
            number(c.total_tokens),
            number(c.input_tokens),
            number(c.output_tokens),
            number(c.cache_read_tokens),
            number(c.cache_write_tokens),
            usd(c.cost_usd),
        ];
        assert!(shown.iter().all(|value| value == "—"));
        assert_eq!(view.groups.len(), 100);
        assert!(session_option(view.groups[98], 98).starts_with("Session 99 · "));
    }

    #[test]
    fn labels_turns_and_running_totals() {
        let projection = project_usage(&[frame(1, "a", 3, None)], "channel");
        let row = &projection.groups[0].turns[0];
        assert_eq!(turn_label(row), "#3");
        let setup = UsageRecord {
            turn_id: Some("abc:initial".into()),
            ..row.clone()
        };
        assert_eq!(turn_label(&setup), "Session setup");
        let unnumbered = UsageRecord {
            turn_seq: None,
            ..row.clone()
        };
        assert_eq!(turn_label(&unnumbered), "—");
        assert_eq!(total(Some(row)), "Input 10 / Output —");
        assert_eq!(total(None), "—");
    }
}
