//! The Reminders plugin (`bundled/reminders`): the Reminders page with its
//! due badge, the "Remind me" message action and its alarm marker, and
//! OS notifications when a reminder comes due.
//!
//! One [`Hub`] follows the selected community's reminders and owns the
//! one due-time timer, which also advances the clock the badge and page
//! read. A per-community baseline, saved as buzz-app's
//! `buzz-reminders-checked.v1`, lets reminders that came due while the app
//! was closed notify on the next launch.

mod dialog;
mod model;
mod page;

use super::pages::{self, Page, PageSpec};
use super::renderers::{self, MessageAction};
use crate::community::{AppState, Community};
use crate::notifications::categories::{self, CategorySpec, Submission};
use crate::ui::navigation::{Place, Target};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::{
    ChannelType, EventId, Reminder, RemindersState, RemindersStatus, RosterRefresh, SessionStatus,
};
use buzz_ui::{ActiveTheme, Icon, IconName};
use gpui_kit::*;
pub use model::now_seconds;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

pub const PAGE: Page = Page::new("buzz.reminders/reminders");
const CATEGORY: &str = "buzz.reminders/reminders";
/// The saved check time per `<origin>:<viewer>`.
const CHECKED: &str = "buzz-reminders-checked.v1";

pub fn register(cx: &mut App) {
    pages::register(
        PageSpec {
            badge: Some(std::rc::Rc::new(badge)),
            ..PageSpec::sidebar(PAGE, "Reminders", IconName::Alarm, |window, cx| {
                cx.new(|cx| page::RemindersPage::new(window, cx)).into()
            })
        },
        cx,
    );
    categories::register(
        CategorySpec {
            key: CATEGORY,
            label: "Reminders",
        },
        cx,
    );
    renderers::register_action(Rc::new(RemindMe));
    let hub = cx.new(Hub::new);
    cx.set_global(HubHandle(hub));
}

struct HubHandle(Entity<Hub>);

impl Global for HubHandle {}

pub(crate) fn hub(cx: &App) -> Option<Entity<Hub>> {
    cx.try_global::<HubHandle>().map(|handle| handle.0.clone())
}

/// The selected community's reminders and the shared clock.
pub(crate) struct Hub {
    community: Option<Entity<Community>>,
    state: Arc<RemindersState>,
    now: u64,
    /// Message ids with a pending reminder.
    pending: HashSet<String>,
    /// Per scope: the saved check time when first bound this run.
    baselines: HashMap<String, u64>,
    /// Per scope: `id:not_before` keys notified since the baseline.
    notified: HashMap<String, HashSet<String>>,
    /// The check time last saved, so a busy session writes at most once a
    /// second.
    saved: Option<(String, u64)>,
    /// The pending ids changed since the last check.
    markers_changed: bool,
    scope: String,
    timer: Option<Task<()>>,
    _watch: Option<Task<()>>,
    _community: Option<Subscription>,
}

impl Hub {
    fn new(cx: &mut Context<Self>) -> Self {
        cx.observe_global::<AppState>(|this, cx| this.bind(cx))
            .detach();
        let mut hub = Self {
            community: None,
            state: Arc::new(RemindersState::default()),
            now: now_seconds(),
            pending: HashSet::new(),
            baselines: HashMap::new(),
            notified: HashMap::new(),
            saved: None,
            markers_changed: false,
            scope: String::new(),
            timer: None,
            _watch: None,
            _community: None,
        };
        hub.bind(cx);
        hub
    }

    pub fn community(&self) -> Option<&Entity<Community>> {
        self.community.as_ref()
    }

    pub fn state(&self) -> &Arc<RemindersState> {
        &self.state
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    /// Follows the selected community.
    fn bind(&mut self, cx: &mut Context<Self>) {
        let current = AppState::current(cx);
        if current == self.community {
            return;
        }
        self.community = current.clone();
        self.state = Arc::new(RemindersState::default());
        self.pending.clear();
        self._watch = None;
        self._community = None;
        let Some(community) = current else {
            cx.notify();
            return;
        };
        let (scope, receiver) = {
            let community = community.read(cx);
            let partition = community.partition();
            (
                format!("{}:{}", partition.origin, partition.viewer),
                community.session.reminders(),
            )
        };
        self.scope = scope.clone();
        if !self.baselines.contains_key(&scope) {
            let checked = read_checked(&scope, cx).unwrap_or_else(now_seconds);
            self.baselines.insert(scope.clone(), checked);
            write_checked(&scope, checked, cx);
            self.saved = Some((scope, checked));
        }
        self._watch = Some(follow(receiver, cx));
        self._community = Some(cx.observe(&community, |this, _, cx| this.check(cx)));
        self.check(cx);
    }

    /// The list changed: recompute the markers and check due times.
    fn changed(&mut self) {
        let pending = model::pending_message_ids(&self.state.reminders);
        if pending != self.pending {
            self.pending = pending;
            self.markers_changed = true;
        }
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        self.timer = None;
        self.now = now_seconds();
        cx.notify();
        if std::mem::take(&mut self.markers_changed) {
            // Message rows draw the alarm marker from here.
            cx.refresh_windows();
        }
        let Some(community) = self.community.clone() else {
            return;
        };
        if !crate::plugins::enabled("buzz.reminders") {
            return;
        }
        // Nothing notifies before the first history read succeeds.
        if !self.state.hydrated {
            return;
        }
        let snapshot = community.read(cx).snapshot.clone();
        // Hold notifications until a full roster read verified the list
        // they are judged against.
        if snapshot.status != SessionStatus::Ready
            || snapshot.live_status.roster != RosterRefresh::Verified
        {
            return;
        }
        let now = self.now;
        let scope = self.scope.clone();
        let baseline = *self.baselines.get(&scope).unwrap_or(&now);
        let origin = community.read(cx).origin.to_string();
        let due: Vec<Reminder> = model::due_since(&self.state.reminders, baseline, now)
            .into_iter()
            .cloned()
            .collect();
        for reminder in due {
            let key = format!(
                "{}:{}",
                reminder.id,
                reminder.not_before.unwrap_or_default()
            );
            if !self
                .notified
                .entry(scope.clone())
                .or_default()
                .insert(key.clone())
            {
                continue;
            }
            let message = reminder.navigable_target();
            let channel = message.and_then(|message| {
                snapshot
                    .channels
                    .iter()
                    .find(|channel| channel.id == message.channel_id)
            });
            let place = match message {
                Some(message) => Place::Conversation {
                    channel: message.channel_id.clone(),
                    message: EventId::from_hex(&message.event_id).ok(),
                    thread: None,
                    members: false,
                },
                None => Place::Page(PAGE),
            };
            let title = match channel {
                Some(channel)
                    if channel.channel_type != Some(ChannelType::Dm)
                        && !channel.name.is_empty() =>
                {
                    format!("Reminder due · #{}", channel.name)
                }
                _ => "Reminder due".into(),
            };
            let body = [
                reminder
                    .target
                    .as_ref()
                    .map(|target| target.preview.as_str()),
                reminder.note.as_deref(),
            ]
            .into_iter()
            .flatten()
            .find(|text| !crate::notifications::plain_text(text).is_empty())
            .unwrap_or("A reminder is waiting")
            .to_owned();
            categories::submit(
                CATEGORY,
                Submission {
                    source_key: key,
                    title,
                    body,
                    target: Target {
                        community: Some(origin.clone()),
                        place,
                    },
                },
                cx,
            );
        }
        // The saved time is what the next launch treats as seen, so it
        // moves only while live and after a read made during this
        // connection succeeded.
        if self.state.status == RemindersStatus::Ready
            && self.state.error.is_none()
            && snapshot.live
        {
            let fresh = self
                .saved
                .as_ref()
                .is_none_or(|(saved_scope, at)| *saved_scope != scope || *at != now);
            if fresh {
                write_checked(&scope, now, cx);
                self.saved = Some((scope, now));
            }
        }
        // After sleep the timer fires late; the baseline covers the gap.
        if let Some(delay) = model::next_delay(&self.state.reminders, now) {
            self.timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(delay))
                    .await;
                let _ = this.update(cx, |this, cx| this.check(cx));
            }));
        }
    }
}

/// Mirrors the session's list into the hub, checking due times on each
/// change.
fn follow(
    mut receiver: tokio::sync::watch::Receiver<Arc<RemindersState>>,
    cx: &mut Context<Hub>,
) -> Task<()> {
    use futures::StreamExt;
    let (sender, mut values) = futures::channel::mpsc::unbounded();
    let _ = sender.unbounded_send(receiver.borrow_and_update().clone());
    crate::runtime::Runtime::handle(cx).spawn(async move {
        while receiver.changed().await.is_ok() {
            let value = receiver.borrow_and_update().clone();
            if sender.unbounded_send(value).is_err() {
                return;
            }
        }
    });
    cx.spawn(async move |this, cx| {
        while let Some(mut value) = values.next().await {
            while let Ok(newer) = values.try_recv() {
                value = newer;
            }
            let updated = this.update(cx, |this, cx| {
                this.state = value;
                this.changed();
                this.check(cx);
            });
            if updated.is_err() {
                return;
            }
        }
    })
}

fn read_checked(scope: &str, cx: &App) -> Option<u64> {
    let raw = cx
        .global::<AppState>()
        .store
        .device_pref(&format!("{CHECKED}:{scope}"))
        .ok()??;
    (raw.len() <= 12 && !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
        .then(|| raw.parse().ok())
        .flatten()
}

fn write_checked(scope: &str, at: u64, cx: &App) {
    // The in-memory baseline still prevents repeats while this runs.
    let _ = cx
        .global::<AppState>()
        .store
        .set_device_pref(&format!("{CHECKED}:{scope}"), &at.to_string());
}

/// The sidebar row's due count.
fn badge(cx: &App) -> Option<AnyElement> {
    let hub = hub(cx)?;
    let hub = hub.read(cx);
    let due = model::count_due(&hub.state.reminders, hub.now);
    (due > 0).then(|| {
        div()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.colors().text_navigation_unread)
            .child(due.to_string())
            .into_any_element()
    })
}

/// "Remind me" in a message's menu, with the alarm marker beside the
/// time of messages that have a pending reminder.
struct RemindMe;

impl MessageAction for RemindMe {
    fn key(&self) -> &str {
        "buzz.reminders/remind-me"
    }

    fn title(&self) -> &str {
        "Remind me"
    }

    fn icon(&self) -> Option<IconName> {
        Some(IconName::Alarm)
    }

    fn matches(&self, _: &MessageRow) -> bool {
        true
    }

    fn run(&self, row: &Arc<MessageRow>, _: &mut Window, cx: &mut App) {
        let Some(community) = AppState::current(cx) else {
            return;
        };
        let row = row.clone();
        crate::ui::navigation::with_workspace(cx, move |workspace, window, cx| {
            let dialog = cx.new(|cx| dialog::RemindDialog::new(community, &row, window, cx));
            workspace.show_dialog(dialog, cx);
        });
    }

    fn marker(&self, row: &MessageRow, cx: &App) -> Option<AnyElement> {
        let hub = hub(cx)?;
        hub.read(cx).pending.contains(&row.id.to_hex()).then(|| {
            div()
                .id(SharedString::from(format!("reminder-marker-{}", row.id)))
                .flex()
                .text_color(cx.colors().text_subtle)
                .tooltip(buzz_ui::Tooltip::text("Reminder set"))
                .child(Icon::new(IconName::Alarm).size(14.))
                .into_any_element()
        })
    }
}
