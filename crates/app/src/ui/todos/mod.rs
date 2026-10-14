//! Todos (`buzz.todos`, buzz-app's `bundled/todos`, off by default): a
//! channel-header launcher that opens a side panel editing the `## Todos`
//! section of the channel Canvas.
//!
//! The draft, the text it started from, its base revision and the new
//! todo text are saved locally under `todos-draft-v1:<channel>` once
//! typing pauses, and when the panel closes, so a closed panel or app
//! keeps unsaved edits. Every edit saves at once, naming the base
//! revision; when the Canvas moved on, the draft is kept
//! until an explicit Refresh. Nothing reads or writes in the background.

pub mod model;

use crate::community::{AppState, Community};
use crate::ui::agent_activity::public_key_labels;
use crate::ui::canvas::{DraftWriter, js_error};
use crate::ui::channel_panels::{self, ChannelPanel, Close, Launcher, Opening, Placement};
use crate::ui::markdown::Directory;
use crate::ui::overlay::{self, Align, Dismiss};
use buzz_relay_client::canvas::CanvasError;
use buzz_relay_client::{EventId, Partition, RosterStatus, SessionStatus};
use buzz_ui::{
    ActiveTheme, AlertDialog, Avatar, AvatarShape, AvatarSize, Button, ButtonSize, ButtonVariant,
    Icon, IconButton, IconButtonSize, IconName, Menu, MenuItem, MenuLabel, PanelHeader, Popover,
    Spinner, Surface, TextField, TypeStyled, tokens::radius, tokens::space,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use model::{Assignee, Status, Todo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::time::Duration;

pub const KEY: &str = "buzz.todos/todos";

const CHANGED_ELSEWHERE: &str =
    "Canvas changed elsewhere. Refresh to load the latest todos. Your changes are kept until then.";
const ABOUT: &str = "Todos are shared in this channel’s Canvas. Changes save automatically. Refresh to load others’ changes. On older relays, simultaneous saves can overwrite edits.";

const STATUSES: [(Status, &str); 3] = [
    (Status::Todo, "To do"),
    (Status::Doing, "In progress"),
    (Status::Done, "Done"),
];

pub fn init(cx: &mut App) {
    channel_panels::register(
        ChannelPanel {
            key: KEY,
            title: "Todos",
            icon: IconName::ListChecks,
            matches: |_| false,
            launcher: Some(Launcher {
                icon: IconName::ListChecks,
                label: "Todos",
                placement: Placement::Side,
            }),
            menu: None,
            build: |opening, window, cx| cx.new(|cx| TodosPanel::new(opening, window, cx)).into(),
        },
        cx,
    );
}

/// The locally saved draft, in buzz-app's shape.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Draft {
    content: String,
    original: String,
    base: Option<String>,
    input: String,
}

impl Draft {
    fn dirty(&self) -> bool {
        self.content != self.original
    }
}

fn draft_key(channel: &str) -> String {
    format!("todos-draft-v1:{channel}")
}

/// A saved draft, or `None` when absent or malformed (`readDraft`).
fn parse_draft(json: &str) -> Option<Draft> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
    let base = match value.get("base")? {
        serde_json::Value::Null => None,
        serde_json::Value::String(base) => Some(base.clone()),
        _ => return None,
    };
    Some(Draft {
        content: text("content")?,
        original: text("original")?,
        base,
        input: text("input")?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Load,
    Save,
}

/// Which row menu is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Status(usize),
    Assignee(usize),
}

fn status_icon(status: Status, cx: &App) -> Icon {
    let c = cx.colors();
    let (icon, color) = match status {
        Status::Done => (IconName::CheckCircle, c.icon_status_complete),
        Status::Doing => (IconName::CircleHalf, c.icon_status_progress),
        Status::Todo => (IconName::Circle, c.text_subtle),
    };
    Icon::new(icon).size(16.).color(color)
}

pub struct TodosPanel {
    community: Entity<Community>,
    channel: String,
    close: Close,
    input: Entity<InputState>,
    /// The session was ready when the editor mounted; it remounts after
    /// the session drops and comes back.
    mounted: bool,
    /// The draft found when the editor mounted.
    saved: Option<Draft>,
    draft: Draft,
    loaded: bool,
    busy: Option<Busy>,
    error: Option<String>,
    conflict: bool,
    confirm_refresh: bool,
    /// Bumped by each load and save; older results are dropped.
    operation: u64,
    saving: bool,
    /// The loaded or saved revision's second.
    saved_at: u64,
    /// `canvas.available`, once known.
    available: Option<bool>,
    menu: Option<Choice>,
    about: bool,
    /// The people whose profiles were last asked for.
    people: Vec<String>,
    drafts: DraftWriter<Draft>,
    _subscriptions: Vec<Subscription>,
}

impl TodosPanel {
    fn new(opening: Opening, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a todo…"));
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        let value = input.read(cx).value().to_string();
                        if value != this.draft.input {
                            let next = Draft {
                                input: value,
                                ..this.draft.clone()
                            };
                            this.update(next, cx);
                        }
                    }
                    InputEvent::PressEnter { .. } => this.add(window, cx),
                    _ => {}
                },
            ),
            cx.observe_in(&opening.community, window, |this, _, window, cx| {
                this.follow_session(window, cx)
            }),
        ];
        let mut this = Self {
            community: opening.community,
            channel: opening.channel,
            close: opening.close,
            input,
            mounted: false,
            saved: None,
            draft: Draft::default(),
            loaded: false,
            busy: None,
            error: None,
            conflict: false,
            confirm_refresh: false,
            operation: 0,
            saving: false,
            saved_at: 0,
            available: None,
            menu: None,
            about: false,
            people: Vec::new(),
            drafts: DraftWriter::default(),
            _subscriptions: subscriptions,
        };
        this.follow_session(window, cx);
        this
    }

    fn ready(&self, cx: &App) -> bool {
        matches!(
            self.community.read(cx).snapshot.status,
            SessionStatus::Ready
        )
    }

    /// Mounts the editor when the session is ready and drops it, with
    /// any result still on the way, when it is not.
    fn follow_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (self.ready(cx), self.mounted) {
            (true, false) => self.mount(window, cx),
            (false, true) => {
                self.mounted = false;
                self.operation += 1;
                self.saving = false;
                cx.notify();
            }
            _ => {}
        }
    }

    fn mount(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drafts.flush();
        let saved = cx
            .global::<AppState>()
            .store
            .view_state(&self.partition(cx), &draft_key(&self.channel))
            .ok()
            .flatten()
            .and_then(|json| parse_draft(&json));
        self.draft = saved.clone().unwrap_or_default();
        self.saved = saved;
        self.mounted = true;
        self.loaded = false;
        self.busy = None;
        self.error = None;
        self.conflict = false;
        self.confirm_refresh = false;
        self.saving = false;
        self.saved_at = 0;
        self.menu = None;
        self.sync_input(window, cx);
        let session = self.community.read(cx).session.clone();
        cx.spawn(async move |this, cx| {
            let available = session.canvas_available().await;
            let _ = this.update(cx, |this, cx| {
                this.available = Some(available);
                cx.notify();
            });
        })
        .detach();
        self.load(false, window, cx);
    }

    fn partition(&self, cx: &App) -> Partition {
        self.community.read(cx).partition()
    }

    /// Shows the draft's new todo text in the field.
    fn sync_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.draft.input.clone();
        self.input.update(cx, |input, cx| {
            if input.value() != value.as_str() {
                input.set_value(value, window, cx);
            }
        });
    }

    /// Replaces the draft and saves it locally, once typing pauses, while
    /// it holds anything unsaved.
    fn update(&mut self, next: Draft, cx: &mut Context<Self>) {
        let keep = next.dirty() || !next.input.is_empty();
        let (partition, key) = (self.partition(cx), draft_key(&self.channel));
        self.drafts
            .save(partition, key, keep.then(|| next.clone()), true, cx);
        self.draft = next;
        cx.notify();
    }

    /// Reads the Canvas. A first read keeps a recovered unsaved draft and
    /// says whether the Canvas moved on; `replace` adopts the Canvas.
    fn load(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mounted {
            return;
        }
        self.operation += 1;
        let generation = self.operation;
        self.busy = Some(Busy::Load);
        self.error = None;
        cx.notify();
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.canvas_read(&channel, true).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.mounted || this.operation != generation {
                    return;
                }
                match result {
                    Ok(head) => {
                        this.saved_at = head.as_ref().map_or(0, |head| head.created_at);
                        this.loaded = true;
                        let content = head.as_ref().map(|head| head.content.clone());
                        let id = head.as_ref().map(|head| head.id.to_hex());
                        let saved = this.saved.as_ref();
                        let saved_dirty = saved.is_some_and(Draft::dirty);
                        let already_saved =
                            saved.is_some_and(|saved| Some(&saved.content) == content.as_ref());
                        let changed = !replace
                            && saved_dirty
                            && !already_saved
                            && saved.and_then(|saved| saved.base.clone()) != id;
                        this.conflict = changed;
                        if changed {
                            this.error = Some(CHANGED_ELSEWHERE.into());
                        } else if !replace && saved_dirty && !already_saved {
                            this.error = Some(
                                "Recovered unsaved changes. Retry to save them to Canvas.".into(),
                            );
                        }
                        if replace || !saved_dirty || already_saved {
                            let content = content.unwrap_or_default();
                            let input = match (replace, saved) {
                                (false, Some(saved)) => saved.input.clone(),
                                _ => String::new(),
                            };
                            this.update(
                                Draft {
                                    original: content.clone(),
                                    content,
                                    base: id,
                                    input,
                                },
                                cx,
                            );
                            this.sync_input(window, cx);
                        }
                    }
                    Err(error) => this.error = Some(js_error(&error)),
                }
                this.busy = None;
                cx.notify();
            });
        })
        .detach();
    }

    /// Saves `next` (the draft when `None`) over its base revision.
    fn save(&mut self, next: Option<Draft>, window: &mut Window, cx: &mut Context<Self>) {
        let next = next.unwrap_or_else(|| self.draft.clone());
        if !self.mounted
            || self.saving
            || self.busy.is_some()
            || !self.loaded
            || !next.dirty()
            || self.conflict
            || self.available != Some(true)
        {
            return;
        }
        self.saving = true;
        self.operation += 1;
        let generation = self.operation;
        self.busy = Some(Busy::Save);
        self.error = None;
        cx.notify();
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.clone();
        let saved_at = self.saved_at;
        let base = next
            .base
            .as_deref()
            .and_then(|base| EventId::from_hex(base).ok());
        cx.spawn_in(window, async move |this, cx| {
            // Canvas revisions use whole seconds. Wait once, without
            // retrying a write.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            let delay = ((saved_at + 1) * 1000).saturating_sub(now);
            if delay > 0 {
                cx.background_executor()
                    .timer(Duration::from_millis(delay.min(1000)))
                    .await;
            }
            let current = this
                .update(cx, |this, _| this.mounted && this.operation == generation)
                .unwrap_or(false);
            if !current {
                let _ = this.update(cx, |this, _| this.saving = false);
                return;
            }
            let result = session.canvas_save(&channel, &next.content, base).await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.saving = false;
                if !this.mounted || this.operation != generation {
                    return;
                }
                match result {
                    Ok(head) => {
                        this.saved_at = head.created_at;
                        let saved = Draft {
                            original: head.content.clone(),
                            content: head.content,
                            base: Some(head.id.to_hex()),
                            ..this.draft.clone()
                        };
                        this.update(saved, cx);
                    }
                    Err(error) => {
                        this.conflict = error.is_conflict();
                        this.error = Some(match error {
                            CanvasError::Conflict => CHANGED_ELSEWHERE.into(),
                            CanvasError::Failed(message) => {
                                format!("Couldn’t save. Your changes are still here. {message}")
                            }
                        });
                    }
                }
                this.busy = None;
                cx.notify();
            });
        })
        .detach();
    }

    fn input_disabled(&self, parse_error: bool) -> bool {
        !self.loaded
            || self.busy == Some(Busy::Load)
            || self.available != Some(true)
            || parse_error
            || self.conflict
    }

    fn disabled(&self) -> bool {
        let parse_error = model::read_todos(&self.draft.content).is_err();
        self.input_disabled(parse_error) || self.busy == Some(Busy::Save)
    }

    /// Applies one source edit and saves it.
    fn change(
        &mut self,
        edit: impl FnOnce(&str) -> Result<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        if !self.mounted || self.disabled() || self.saving {
            cx.notify();
            return;
        }
        match edit(&self.draft.content) {
            Ok(content) => {
                let next = Draft {
                    content,
                    ..self.draft.clone()
                };
                self.update(next.clone(), cx);
                self.save(Some(next), window, cx);
            }
            Err(reason) => {
                self.error = Some(format!("Error: {reason}"));
                cx.notify();
            }
        }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.mounted || self.disabled() || self.saving || self.draft.input.trim().is_empty() {
            return;
        }
        match model::add_todo(&self.draft.content, &self.draft.input) {
            Ok(content) => {
                let next = Draft {
                    content,
                    input: String::new(),
                    ..self.draft.clone()
                };
                self.update(next.clone(), cx);
                self.sync_input(window, cx);
                self.save(Some(next), window, cx);
            }
            Err(reason) => {
                self.error = Some(format!("Error: {reason}"));
                cx.notify();
            }
        }
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loaded && (self.draft.dirty() || !self.draft.input.is_empty()) {
            self.confirm_refresh = true;
            cx.notify();
            return;
        }
        let replace = self.loaded;
        self.load(replace, window, cx);
    }

    fn save_status(&self) -> &'static str {
        if self.busy == Some(Busy::Save) {
            "Saving…"
        } else if self.draft.dirty() {
            "Unsaved changes"
        } else if self.loaded {
            "Saved in Canvas"
        } else {
            "Channel Canvas"
        }
    }

    fn dismiss_menu(cx: &mut Context<Self>) -> Dismiss {
        let weak = cx.entity().downgrade();
        Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.menu = None;
                this.about = false;
                cx.notify();
            });
        })
    }

    fn header(
        &self,
        channel_name: SharedString,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> PanelHeader {
        let c = *cx.colors();
        let status = self.save_status();
        let about = div()
            .relative()
            .child(
                IconButton::new("todos-about", IconName::Info)
                    .size(IconButtonSize::Sm)
                    .tooltip("About todos")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.about = !this.about;
                        cx.notify();
                    })),
            )
            .when(self.about, |this| {
                this.child(overlay::dropdown(
                    window,
                    Align::End,
                    Popover::new().title(status).description(ABOUT),
                    Self::dismiss_menu(cx),
                ))
            });
        let loading = self.busy == Some(Busy::Load);
        let refresh = IconButton::new("todos-refresh", IconName::ArrowsClockwise)
            .size(IconButtonSize::Sm)
            .tooltip("Refresh todos")
            .disabled(self.busy.is_some())
            .when(loading, |this| this.child(Spinner::new().size(16.)))
            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx)));
        let close = self.close.clone();
        PanelHeader::new()
            .label(Some(IconName::ListChecks), "Todos")
            .leading(
                div()
                    .min_w_0()
                    .truncate()
                    .text_caption()
                    .text_color(c.text_subtle)
                    .child(channel_name),
            )
            .action(about)
            .action(refresh)
            .action(
                IconButton::new("todos-hide", IconName::X)
                    .size(IconButtonSize::Sm)
                    .variant(ButtonVariant::Ghost)
                    .tooltip("Hide todos")
                    .on_click(move |_, window, cx| close(window, cx)),
            )
    }

    /// A row's compact choice trigger and, when open, its menu
    /// (`TodoChoice`).
    #[allow(clippy::too_many_arguments)]
    fn choice(
        &self,
        id: SharedString,
        which: Choice,
        title: String,
        trigger: AnyElement,
        avatar: bool,
        menu_title: &'static str,
        options: Vec<ChoiceOption>,
        selected: &str,
        disabled: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let button = IconButton::new(id.clone(), IconName::User)
            .size(IconButtonSize::Sm)
            .variant(if avatar {
                ButtonVariant::Avatar
            } else {
                ButtonVariant::Ghost
            })
            .tooltip(title)
            .disabled(disabled)
            .child(trigger)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = (this.menu != Some(which)).then_some(which);
                cx.notify();
            }));
        let open = self.menu == Some(which) && !disabled;
        div()
            .relative()
            .flex_none()
            .child(button)
            .when(open, |this| {
                let mut menu = Menu::new().child(MenuLabel::new(menu_title));
                for (index, option) in options.into_iter().enumerate() {
                    let ChoiceOption {
                        value,
                        label,
                        icon,
                        disabled,
                    } = option;
                    let checked = value == selected;
                    let on_pick = option_handler(which, value);
                    menu = menu.child(
                        MenuItem::new(SharedString::from(format!("{id}-option-{index}")), label)
                            .leading(icon)
                            .checked(checked)
                            .disabled(disabled)
                            .on_click(
                                cx.listener(move |this, _, window, cx| on_pick(this, window, cx)),
                            ),
                    );
                }
                this.child(overlay::dropdown(
                    window,
                    Align::End,
                    menu,
                    Self::dismiss_menu(cx),
                ))
            })
            .into_any_element()
    }

    fn pick_status(
        &mut self,
        offset: usize,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(status) = STATUSES
            .iter()
            .map(|(status, _)| *status)
            .find(|status| status_value(*status) == value)
        else {
            return;
        };
        let current = model::read_todos(&self.draft.content)
            .ok()
            .and_then(|todos| todos.items.into_iter().find(|item| item.offset == offset))
            .map(|item| item.status);
        if current == Some(status) {
            self.menu = None;
            cx.notify();
            return;
        }
        self.change(
            |content| model::set_status(content, offset, status),
            window,
            cx,
        );
    }

    fn pick_assignee(
        &mut self,
        offset: usize,
        pubkey: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Membership is checked again at the moment of choosing.
        self.menu = None;
        cx.notify();
        let Some(members) = self.members(cx) else {
            return;
        };
        if !pubkey.is_empty() && !members.iter().any(|member| member == pubkey) {
            return;
        }
        let assignee = (!pubkey.is_empty()).then(|| {
            let labels = public_key_labels(&self.people);
            let fallback = labels
                .get(pubkey)
                .cloned()
                .unwrap_or_else(|| pubkey.to_owned());
            Assignee {
                pubkey: pubkey.to_owned(),
                name: self.resolve_name(pubkey, &fallback, Some(&members), cx),
            }
        });
        self.change(
            |content| model::assign_todo(content, offset, assignee.as_ref()),
            window,
            cx,
        );
    }

    /// The channel's members, `None` when the channel is not listed.
    fn members(&self, cx: &App) -> Option<Vec<String>> {
        self.community
            .read(cx)
            .snapshot
            .channels
            .iter()
            .find(|channel| channel.id == self.channel)
            .map(|channel| channel.members.clone())
    }

    /// `resolveIdentity(pubkey, fallback, members)`: a placeholder profile
    /// name does not count as a name.
    fn resolve_name(
        &self,
        pubkey: &str,
        fallback: &str,
        members: Option<&[String]>,
        cx: &App,
    ) -> String {
        let community = self.community.read(cx);
        let names = Directory::of(community, members, cx).names;
        let name = names.name(pubkey, fallback);
        let placeholder = community
            .snapshot
            .profiles
            .get(&pubkey.to_ascii_lowercase())
            .is_some_and(|profile| profile.name_is_fallback && profile.name == name);
        if placeholder {
            fallback.to_owned()
        } else {
            name
        }
    }
}

fn status_value(status: Status) -> &'static str {
    match status {
        Status::Todo => "todo",
        Status::Doing => "doing",
        Status::Done => "done",
    }
}

type Pick = Rc<dyn Fn(&mut TodosPanel, &mut Window, &mut Context<TodosPanel>)>;

fn option_handler(which: Choice, value: String) -> Pick {
    Rc::new(move |this, window, cx| match which {
        Choice::Status(offset) => this.pick_status(offset, &value, window, cx),
        Choice::Assignee(offset) => this.pick_assignee(offset, &value, window, cx),
    })
}

struct ChoiceOption {
    value: String,
    label: String,
    icon: AnyElement,
    disabled: bool,
}

/// Names and avatars for one render.
struct People<'a> {
    panel: &'a TodosPanel,
    members: Option<Vec<String>>,
    labels: HashMap<String, String>,
}

impl People<'_> {
    fn name(&self, pubkey: &str, fallback: &str, cx: &App) -> String {
        self.panel
            .resolve_name(pubkey, fallback, self.members.as_deref(), cx)
    }

    /// `userLabel`: `Name · npub…xyz`, or the key label alone.
    fn label(&self, pubkey: &str, fallback: &str, cx: &App) -> String {
        let key = self
            .labels
            .get(pubkey)
            .cloned()
            .unwrap_or_else(|| pubkey.to_owned());
        let name = self.name(pubkey, fallback, cx);
        if name.is_empty() {
            key
        } else {
            format!("{name} · {key}")
        }
    }

    /// `personIcon`.
    fn icon(&self, pubkey: &str, fallback: &str, cx: &App) -> AnyElement {
        let community = self.panel.community.read(cx);
        let profile = community.snapshot.profiles.get(pubkey);
        let mut name = self.name(pubkey, fallback, cx);
        if name.is_empty() {
            name = self
                .labels
                .get(pubkey)
                .cloned()
                .unwrap_or_else(|| "Unknown member".into());
        }
        let mut avatar = Avatar::new(name).size(AvatarSize::Small).alt("");
        if profile.is_some_and(|profile| profile.is_agent) {
            avatar = avatar.shape(AvatarShape::Squircle);
        }
        if let Some(picture) = profile.and_then(|profile| profile.picture.clone()) {
            avatar = avatar.image(crate::ui::media::avatar(
                &picture,
                community.session.media(),
            ));
        }
        avatar.into_any_element()
    }
}

impl TodosPanel {
    fn row(
        &self,
        item: &Todo,
        people: &People,
        disabled: bool,
        (first, last): (bool, bool),
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let offset = item.offset;
        let done = item.status == Status::Done;
        let next = if done { Status::Todo } else { Status::Done };
        let check_state = match item.status {
            Status::Done => buzz_ui::CheckboxState::Checked,
            Status::Doing => buzz_ui::CheckboxState::Indeterminate,
            Status::Todo => buzz_ui::CheckboxState::Unchecked,
        };
        let check = div()
            .id(SharedString::from(format!("todo-{offset}")))
            .flex()
            .items_start()
            .gap(rems(0.75))
            .min_w_0()
            .min_h(rems(2.5))
            .when(!disabled, |this| {
                this.cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.change(
                            |content| model::set_status(content, offset, next),
                            window,
                            cx,
                        )
                    }))
            })
            .when(disabled, |this| this.cursor_not_allowed())
            .child(
                div().pt(rems(0.625)).child(
                    buzz_ui::Checkbox::new(
                        SharedString::from(format!("todo-{offset}-box")),
                        check_state,
                    )
                    .disabled(disabled),
                ),
            )
            .child(
                div()
                    .min_w_0()
                    .py(rems(0.625))
                    .text_body_sm()
                    .text_color(if done { c.text_subtle } else { c.text_standard })
                    .when(done, |this| this.line_through())
                    .child(item.label.clone()),
            );
        let status = self.choice(
            format!("todo-{offset}-status").into(),
            Choice::Status(offset),
            "Status".into(),
            status_icon(item.status, cx).into_any_element(),
            false,
            "Change status",
            STATUSES
                .iter()
                .map(|(status, label)| ChoiceOption {
                    value: status_value(*status).into(),
                    label: (*label).into(),
                    icon: status_icon(*status, cx).into_any_element(),
                    disabled: false,
                })
                .collect(),
            status_value(item.status),
            disabled,
            window,
            cx,
        );
        let members = people.members.as_deref();
        let assigned = item.assignee.as_ref();
        let user_icon = || Icon::new(IconName::User).size(16.).into_any_element();
        let mut choices: Vec<(String, String)> = members
            .unwrap_or_default()
            .iter()
            .map(|pubkey| (pubkey.clone(), people.label(pubkey, "", cx)))
            .collect();
        choices.sort_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then(a.1.cmp(&b.1))
        });
        let mut options = vec![ChoiceOption {
            value: String::new(),
            label: "Unassigned".into(),
            icon: user_icon(),
            disabled: false,
        }];
        for (pubkey, _) in &choices {
            let fallback = assigned
                .filter(|a| &a.pubkey == pubkey)
                .map_or("", |a| a.name.as_str());
            options.push(ChoiceOption {
                value: pubkey.clone(),
                label: people.label(pubkey, fallback, cx),
                icon: people.icon(pubkey, "", cx),
                disabled: false,
            });
        }
        if let Some(a) = assigned.filter(|a| !members.is_some_and(|m| m.contains(&a.pubkey))) {
            options.push(ChoiceOption {
                value: a.pubkey.clone(),
                label: format!(
                    "{} ({})",
                    people.label(&a.pubkey, &a.name, cx),
                    if members.is_some() {
                        "not in channel"
                    } else {
                        "membership unavailable"
                    }
                ),
                icon: people.icon(&a.pubkey, &a.name, cx),
                disabled: true,
            });
        }
        let assignee = self.choice(
            format!("todo-{offset}-assignee").into(),
            Choice::Assignee(offset),
            assigned.map_or("Assign task".into(), |a| {
                people.label(&a.pubkey, &a.name, cx)
            }),
            assigned.map_or_else(user_icon, |a| people.icon(&a.pubkey, &a.name, cx)),
            assigned.is_some(),
            "Assign to",
            options,
            assigned.map_or("", |a| a.pubkey.as_str()),
            disabled || members.is_none(),
            window,
            cx,
        );
        div()
            .flex()
            .items_center()
            .gap(space::S2)
            .py(space::S3)
            .when(first, |this| this.pt(space::S2))
            .when(last, |this| this.pb(px(0.)))
            .when(!last, |this| {
                this.border_b_1().border_color(c.border_standard)
            })
            .child(div().flex_1().min_w_0().child(check))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space::S1)
                    .min_w_0()
                    .child(status)
                    .child(div().max_w(rems(10.)).min_w_0().child(assignee)),
            )
            .into_any_element()
    }

    fn confirm_dialog(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = AlertDialog::new("Discard unsaved changes?")
            .description("Refreshing replaces your local changes and new todo text with the saved channel Canvas.")
            .action(
                Button::new("todos-keep", "Keep editing")
                    .surface(Surface::Floating)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.confirm_refresh = false;
                        cx.notify();
                    })),
            )
            .action(
                Button::new("todos-discard", "Discard and refresh")
                    .destructive()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.confirm_refresh = false;
                        this.load(true, window, cx);
                    })),
            );
        let weak = cx.entity().downgrade();
        let dismiss: Dismiss = Rc::new(move |_, cx| {
            let _ = weak.update(cx, |this, cx| {
                this.confirm_refresh = false;
                cx.notify();
            });
        });
        overlay::modal(window, dialog, Some(dismiss))
    }
}

impl Render for TodosPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let root = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(c.surface_panel)
            .text_color(c.text_standard);
        if !self.mounted || !self.ready(cx) {
            return root
                .child(
                    div()
                        .p(space::S6)
                        .text_body_sm()
                        .child("Connect to this channel to view todos."),
                )
                .into_any_element();
        }
        let community = self.community.read(cx);
        let channel_name: SharedString = community
            .snapshot
            .channels
            .iter()
            .find(|channel| channel.id == self.channel)
            .map_or_else(|| self.channel.clone(), |channel| channel.name.clone())
            .into();
        let roster_error = matches!(community.snapshot.roster, RosterStatus::Error(_));
        let session = community.session.clone();
        let members = self.members(cx);
        let parsed = model::read_todos(&self.draft.content);
        let parse_error = parsed.as_ref().err().cloned();
        let items = parsed.map(|todos| todos.items).unwrap_or_default();
        let input_disabled = self.input_disabled(parse_error.is_some());
        let disabled = input_disabled || self.busy == Some(Busy::Save);
        let dirty = self.draft.dirty();

        // Ask for the profiles of everyone the panel names.
        let people: Vec<String> = members
            .iter()
            .flatten()
            .cloned()
            .chain(
                items
                    .iter()
                    .filter_map(|item| item.assignee.as_ref().map(|a| a.pubkey.clone())),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if people != self.people {
            self.people = people.clone();
            session.want_profiles(people);
        }
        let people = People {
            panel: self,
            labels: public_key_labels(&self.people),
            members,
        };

        let add = div()
            .flex()
            .items_center()
            .gap(space::S2)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(TextField::new(&self.input).disabled(input_disabled)),
            )
            .child(
                Button::new("todos-add", "Add")
                    .prominent()
                    .size(ButtonSize::Md)
                    .disabled(disabled || self.draft.input.trim().is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
            );
        let message = self.error.clone().or(parse_error.clone());
        let feedback = (message.is_some() || (dirty && self.busy.is_none())).then(|| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(space::S3)
                .children(message.map(|message| {
                    div()
                        .flex_1()
                        .min_w(rems(16.))
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(message)
                }))
                .when(self.conflict && self.busy.is_none(), |this| {
                    this.child(
                        Button::new("todos-refresh-conflict", "Refresh todos")
                            .size(ButtonSize::Sm)
                            .outline()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_refresh = true;
                                cx.notify();
                            })),
                    )
                })
                .when(
                    dirty && self.busy.is_none() && !self.conflict && parse_error.is_none(),
                    |this| {
                        this.child(
                            Button::new("todos-retry", "Retry")
                                .size(ButtonSize::Sm)
                                .outline()
                                .disabled(disabled)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save(None, window, cx)),
                                ),
                        )
                    },
                )
        });
        let users = (roster_error || people.members.is_none()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(space::S2)
                .text_caption()
                .text_color(c.text_subtle)
                .child("Channel members unavailable or out of date.")
                .child(
                    Button::new("todos-retry-users", "Retry users")
                        .size(ButtonSize::Sm)
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            let community = this.community.read(cx);
                            community.session.retry();
                            community.session.want_profiles(this.people.clone());
                        })),
                )
        });
        let unavailable = (self.available == Some(false)).then(|| {
            div()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("This connection doesn’t support saving Canvas.")
        });
        let mut body: Vec<AnyElement> = Vec::new();
        if !self.loaded {
            body.push(
                div()
                    .text_body_sm()
                    .text_color(c.text_subtle)
                    .child(if self.busy.is_some() {
                        "Loading todos…"
                    } else {
                        "Couldn’t load todos. Try Refresh."
                    })
                    .into_any_element(),
            );
        } else if parse_error.is_none() {
            if items.is_empty() {
                body.push(
                    empty_state(
                        IconName::ListChecks,
                        "No todos yet",
                        "Add a todo above to track work with this channel. Changes save automatically.",
                        cx,
                    )
                    .into_any_element(),
                );
            }
            for (status, title) in STATUSES {
                let group: Vec<&Todo> = items.iter().filter(|item| item.status == status).collect();
                if group.is_empty() {
                    continue;
                }
                let count = group.len();
                let rows: Vec<AnyElement> = group
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        self.row(
                            item,
                            &people,
                            disabled,
                            (index == 0, index + 1 == count),
                            window,
                            cx,
                        )
                    })
                    .collect();
                body.push(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap(space::S2)
                                .text_label_sm()
                                .child(title)
                                .child(div().opacity(0.5).child(count.to_string())),
                        )
                        .children(rows)
                        .into_any_element(),
                );
            }
        }
        let header = self.header(channel_name, window, cx);
        let confirm = self
            .confirm_refresh
            .then(|| self.confirm_dialog(window, cx));
        root.child(header)
            .child(
                div()
                    .id("todos-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .max_w(rems(45.))
                            .mx_auto()
                            .p(space::S6)
                            .flex()
                            .flex_col()
                            .gap(space::S6)
                            .child(add)
                            .children(feedback)
                            .children(users)
                            .children(unavailable)
                            .children(body),
                    ),
            )
            .children(confirm)
            .into_any_element()
    }
}

/// `EmptyState` (`level` 4): a bordered card with an icon, title and
/// description.
pub(crate) fn empty_state(
    icon: IconName,
    title: &'static str,
    description: &'static str,
    cx: &App,
) -> Div {
    let c = cx.colors();
    div()
        .w_full()
        .border_1()
        .border_color(c.border_standard)
        .rounded(radius::CONTAINER)
        .flex()
        .flex_col()
        .items_center()
        .gap(space::S4)
        .py(space::S8)
        .px(space::S6)
        .text_center()
        .child(Icon::new(icon).size(32.).color(c.text_subtle))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(space::S2)
                .max_w(rems(30.))
                .child(div().text_label_sm().child(title))
                .child(
                    div()
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(description),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn drafts_round_trip_in_buzz_app_shape() {
        let draft = Draft {
            content: "## Todos\n- [ ] A\n".into(),
            original: String::new(),
            base: None,
            input: "next".into(),
        };
        let json = serde_json::to_string(&draft).unwrap();
        assert_eq!(parse_draft(&json), Some(draft));
        assert_eq!(
            parse_draft(r#"{"content":"x","original":"x","base":"abc","input":""}"#)
                .and_then(|draft| draft.base),
            Some("abc".into())
        );
        // Missing or mistyped fields are ignored, as buzz-app does.
        assert_eq!(
            parse_draft(r#"{"content":"x","original":"x","base":null}"#),
            None
        );
        assert_eq!(
            parse_draft(r#"{"content":"x","original":"x","base":1,"input":""}"#),
            None
        );
        assert_eq!(parse_draft("null"), None);
        assert_eq!(draft_key("c1"), "todos-draft-v1:c1");
    }
}
