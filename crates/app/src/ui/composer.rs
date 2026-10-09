//! The message composer shared by the channel page and the thread panel
//! (ui.md §4.8-4.9, `MessageComposer.tsx`, `ComposerCompletions.tsx`).
//!
//! It owns the text, mention chips, `@` `#` `:` completion, the emoji
//! picker, the formatting toolbar, attachments and the saved draft. The
//! page that hosts it decides what a submission means (a message, a
//! reply or an edit) and reports back with [`Composer::sent`] or
//! [`Composer::failed`].

use super::emoji_picker::{self, Candidate, Dismissed, EmojiPicker, Picked};
use super::media;
use super::message::{self, channel_icon};
use crate::community::{AppState, Community};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::messages::media_byte_limit;
use buzz_relay_client::upload::{self, MAX_UPLOAD, UploadError, UploadFile};
use buzz_relay_client::{Attachment, ChannelType, EventId, Mentions, Partition};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, ButtonVariant, Icon, IconButton, IconButtonSize, IconName,
    Spinner, Tooltip, TypeStyled,
};
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, InlineToken, InlineTokenContext, InputContent, InputEvent,
    MoveDown, MoveUp, Textarea, TextareaState,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// `maxLength` of the editor.
const MAX_CHARS: usize = 16_000;
/// At most this many files per message (`attachment-draft.ts`).
const MAX_FILES: usize = 10;
/// Drafts are saved this long after the last keystroke.
const DRAFT_DELAY: Duration = Duration::from_millis(400);
/// Completion lists.
const MENTION_LIMIT: usize = 50;
const CHANNEL_LIMIT: usize = 20;
const EMOJI_LIMIT: usize = 100;

pub enum ComposerEvent {
    Submit(Submission),
    /// Up in an empty composer: edit the viewer's last message.
    EditLast,
    CancelReply,
}

#[derive(Clone)]
pub struct Submission {
    pub content: String,
    /// Pubkeys of the chosen mention chips, first use first.
    pub mentioned: Vec<String>,
    pub attachments: Vec<Attachment>,
    /// The message being edited, if any.
    pub editing: Option<Arc<MessageRow>>,
}

/// The thread panel's reply target, shown between the text and the tools.
#[derive(Clone, PartialEq)]
pub enum ReplyContext {
    Parent {
        name: SharedString,
        excerpt: SharedString,
    },
    Missing,
}

impl EventEmitter<ComposerEvent> for Composer {}

pub struct Composer {
    community: Entity<Community>,
    channel: String,
    thread_root: Option<EventId>,
    input: Entity<TextareaState>,
    placeholder: SharedString,
    sending: bool,
    /// The host allows sending (a thread needs a loaded reply target).
    can_send: bool,
    editing: Option<Arc<MessageRow>>,
    /// The draft set aside while editing.
    stash: Option<InputContent>,
    context: Option<ReplyContext>,
    formatting: bool,
    completion: Option<Completion>,
    /// The query start Escape dismissed, until the text moves on.
    dismissed: Option<usize>,
    /// The text length and caret the completion was computed for.
    completion_key: Option<(usize, usize)>,
    completion_scroll: ScrollHandle,
    picker: Entity<EmojiPicker>,
    picker_open: bool,
    picker_closed: Option<std::time::Instant>,
    attachments: Vec<Pending>,
    next_attachment: u64,
    drag_over: bool,
    notice: Option<SharedString>,
    save_draft: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Composer {
    pub fn new(
        community: Entity<Community>,
        channel: String,
        thread_root: Option<EventId>,
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder.clone())
                .submit_on_enter(true)
                .auto_grow(1, 9)
        });
        let picker = cx.new(|cx| EmojiPicker::new(community.clone(), window, cx));
        let subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                        InputEvent::Change => this.changed(window, cx),
                        _ => {}
                    }
                }),
                cx.subscribe_in(&picker, window, |this, _, Picked(pick), window, cx| {
                    this.picker_open = false;
                    this.insert_text(&pick.text(), window, cx);
                }),
                cx.subscribe_in(&picker, window, |this, _, _: &Dismissed, window, cx| {
                    this.picker_open = false;
                    this.focus(window, cx);
                    cx.notify();
                }),
                cx.observe(&community, |_, _, cx| cx.notify()),
            ];
        cx.on_release(|this: &mut Self, cx| this.write_draft(cx))
            .detach();
        let mut composer = Self {
            community,
            channel,
            thread_root,
            input,
            placeholder,
            sending: false,
            can_send: true,
            editing: None,
            stash: None,
            context: None,
            formatting: false,
            completion: None,
            dismissed: None,
            completion_key: None,
            completion_scroll: ScrollHandle::new(),
            picker,
            picker_open: false,
            picker_closed: None,
            attachments: Vec::new(),
            next_attachment: 0,
            drag_over: false,
            notice: None,
            save_draft: None,
            _subscriptions: subscriptions,
        };
        composer.load_draft(window, cx);
        composer
    }

    /// The text field, for focus.
    pub fn input(&self) -> Entity<TextareaState> {
        self.input.clone()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn editing(&self) -> Option<&Arc<MessageRow>> {
        self.editing.as_ref()
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if placeholder == self.placeholder {
            return;
        }
        self.placeholder = placeholder.clone();
        if self.editing.is_none() {
            self.input.update(cx, |state, cx| {
                state.set_placeholder(placeholder, window, cx)
            });
        }
    }

    pub fn set_context(&mut self, context: Option<ReplyContext>, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            cx.notify();
        }
    }

    pub fn set_can_send(&mut self, can_send: bool, cx: &mut Context<Self>) {
        if self.can_send != can_send {
            self.can_send = can_send;
            cx.notify();
        }
    }

    /// Puts `row`'s text in the composer to edit it (§4.8 edit mode). The
    /// draft waits until the edit ends.
    pub fn start_edit(
        &mut self,
        row: Arc<MessageRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_none() {
            self.write_draft(cx);
            self.stash = Some(self.input.read(cx).content());
        }
        let content = edit_content(&edit_text(&row));
        let end = content.text().len();
        self.editing = Some(row);
        self.completion = None;
        self.input.update(cx, |state, cx| {
            state.set_value(content, window, cx);
            state.set_selected_range(end..end, cx);
            state.set_placeholder("Edit message", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_none() {
            return;
        }
        let stash = self.stash.take().unwrap_or_else(|| InputContent::new(""));
        let placeholder = self.placeholder.clone();
        self.input.update(cx, |state, cx| {
            let end = stash.text().len();
            state.set_value(stash, window, cx);
            state.set_selected_range(end..end, cx);
            state.set_placeholder(placeholder, window, cx);
        });
        self.completion = None;
        cx.notify();
    }

    /// The host delivered the submission: clear it.
    pub fn sent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sending = false;
        self.notice = None;
        if self.editing.is_some() {
            self.cancel_edit(window, cx);
            return;
        }
        self.attachments.clear();
        self.completion = None;
        self.input
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.save_draft = None;
        self.write_draft(cx);
        cx.notify();
    }

    /// The host could not deliver it; the text stays.
    pub fn failed(&mut self, cx: &mut Context<Self>) {
        self.sending = false;
        cx.notify();
    }

    fn has_text(&self, cx: &App) -> bool {
        !self.input.read(cx).value().trim().is_empty()
    }

    fn uploads_ready(&self) -> bool {
        self.attachments
            .iter()
            .all(|item| matches!(item.status, Status::Ready(_)))
    }

    fn can_submit(&self, cx: &App) -> bool {
        !self.sending
            && self.can_send
            && self.uploads_ready()
            && self.input.read(cx).value().chars().count() <= MAX_CHARS
            && (self.has_text(cx) || (!self.attachments.is_empty() && self.editing.is_none()))
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let state = self.input.read(cx);
        let content = state.value().trim().to_string();
        if let Some(row) = &self.editing
            && content == edit_text(row).trim()
        {
            self.cancel_edit(window, cx);
            return;
        }
        let mut mentioned: Vec<String> = Vec::new();
        for span in state.tokens() {
            let id = span.token().id().to_string();
            if span.token().text().starts_with('@') && !mentioned.contains(&id) {
                mentioned.push(id);
            }
        }
        let attachments = if self.editing.is_some() {
            Vec::new()
        } else {
            self.attachments
                .iter()
                .filter_map(|item| match &item.status {
                    Status::Ready(attachment) => Some(attachment.clone()),
                    _ => None,
                })
                .collect()
        };
        self.sending = true;
        self.completion = None;
        cx.emit(ComposerEvent::Submit(Submission {
            content,
            mentioned,
            attachments,
            editing: self.editing.clone(),
        }));
        cx.notify();
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).value().chars().count() > MAX_CHARS {
            self.notice = Some(format!("Messages hold up to {MAX_CHARS} characters.").into());
        } else if self.notice.is_some() {
            self.notice = None;
        }
        if self.accept_terminator(window, cx) {
            return;
        }
        self.schedule_draft(cx);
        self.refresh_completion(cx);
        cx.notify();
    }

    fn insert_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.input.update(cx, |state, cx| {
            state.focus(window, cx);
            state.replace(text, window, cx);
        });
        cx.notify();
    }

    // ----- drafts -----

    fn draft_key(&self) -> String {
        match self.thread_root {
            Some(root) => format!("draft:{}:thread:{}", self.channel, root.to_hex()),
            None => format!("draft:{}", self.channel),
        }
    }

    fn partition(&self, cx: &App) -> Partition {
        self.community.read(cx).partition()
    }

    fn load_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = cx
            .global::<AppState>()
            .store
            .view_state(&self.partition(cx), &self.draft_key())
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<Draft>(&json).ok());
        if let Some(draft) = saved {
            let content = draft.content();
            let end = content.text().len();
            self.input.update(cx, |state, cx| {
                state.set_value(content, window, cx);
                state.set_selected_range(end..end, cx);
            });
        }
    }

    fn schedule_draft(&mut self, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            return;
        }
        self.save_draft = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.save_draft = None;
                this.write_draft(cx);
            });
        }));
    }

    fn write_draft(&mut self, cx: &mut App) {
        if self.editing.is_some() {
            return;
        }
        let content = self.input.read(cx).content();
        let value = (!content.text().trim().is_empty())
            .then(|| serde_json::to_string(&Draft::from(&content)).expect("drafts serialize"));
        let saved = cx.global::<AppState>().store.set_view_state(
            &self.partition(cx),
            &self.draft_key(),
            value.as_deref(),
        );
        if let Err(error) = saved {
            tracing::warn!(%error, "could not save the draft");
        }
    }

    // ----- completion -----

    /// Recomputes the `@` `#` `:` popup for the text before the caret.
    fn refresh_completion(&mut self, cx: &mut Context<Self>) {
        let state = self.input.read(cx);
        let text = state.value().to_string();
        let selection = state.selected_range();
        self.completion_key = Some((text.len(), selection.end));
        let query = (selection.is_empty())
            .then(|| find_query(&text, selection.end))
            .flatten()
            .filter(|query| {
                // A chip's own `@Name` is not a search.
                !state
                    .tokens()
                    .iter()
                    .any(|span| span.range().contains(&query.start))
            });
        let Some(query) = query else {
            self.completion = None;
            self.dismissed = None;
            return;
        };
        if self.dismissed == Some(query.start) {
            self.completion = None;
            return;
        }
        self.dismissed = None;
        let items = self.suggestions(&query, cx);
        if items.is_empty() {
            self.completion = None;
            return;
        }
        let selected = match &self.completion {
            Some(old) if old.query.start == query.start && old.query.trigger == query.trigger => {
                old.selected.min(items.len() - 1)
            }
            _ => 0,
        };
        self.completion = Some(Completion {
            query,
            items,
            selected,
        });
    }

    fn suggestions(&self, query: &Query, cx: &App) -> Vec<Suggestion> {
        let community = self.community.read(cx);
        let snapshot = &community.snapshot;
        match query.trigger {
            Trigger::Mention => {
                let channel = snapshot.channels.iter().find(|c| c.id == self.channel);
                let members: Vec<String> = channel.map(|c| c.members.clone()).unwrap_or_default();
                let dm = channel.is_some_and(|c| c.channel_type == Some(ChannelType::Dm));
                let mut people: Vec<(String, bool)> =
                    members.iter().map(|key| (key.clone(), true)).collect();
                let mut others: Vec<&String> = snapshot
                    .profiles
                    .keys()
                    .filter(|key| !members.contains(key))
                    .collect();
                others.sort();
                people.extend(others.into_iter().map(|key| (key.clone(), false)));
                let mut ranked: Vec<(u8, Suggestion)> = people
                    .into_iter()
                    .filter_map(|(pubkey, member)| {
                        let profile = snapshot.profiles.get(&pubkey);
                        let name: SharedString = message::display_name(&snapshot.profiles, &pubkey);
                        // An unnamed outsider is only an identity fallback.
                        if !member && profile.is_none_or(|p| p.name_is_fallback) {
                            return None;
                        }
                        let tier = name_match(&name, &query.text)?;
                        Some((
                            tier,
                            Suggestion::Person {
                                pubkey,
                                name,
                                picture: profile.and_then(|p| p.picture.clone()),
                                agent: profile.is_some_and(|p| p.is_agent),
                                member,
                                dm,
                            },
                        ))
                    })
                    .collect();
                if query.text.contains(' ') && ranked.iter().all(|(tier, _)| *tier > 1) {
                    // A multi-word query that continues no name is prose.
                    return Vec::new();
                }
                ranked.sort_by(|(ta, a), (tb, b)| {
                    let key = |s: &Suggestion| match s {
                        Suggestion::Person { member, name, .. } => (!member, name.to_lowercase()),
                        _ => (true, String::new()),
                    };
                    let (ma, na) = key(a);
                    let (mb, nb) = key(b);
                    ma.cmp(&mb).then(ta.cmp(tb)).then(na.cmp(&nb))
                });
                ranked
                    .into_iter()
                    .take(MENTION_LIMIT)
                    .map(|(_, s)| s)
                    .collect()
            }
            Trigger::Channel => {
                let spaced = query.text.contains(char::is_whitespace);
                let mut ranked: Vec<(u8, Suggestion)> = snapshot
                    .channels
                    .iter()
                    .filter(|c| {
                        !c.name.trim().is_empty()
                            && !c.archived
                            && !c.hidden
                            && !matches!(
                                c.channel_type,
                                Some(ChannelType::Dm | ChannelType::Session)
                            )
                    })
                    .filter_map(|c| {
                        let tier = name_match(&c.name, &query.text)?;
                        (!spaced || tier <= 1).then(|| {
                            (
                                tier,
                                Suggestion::Channel {
                                    id: c.id.clone(),
                                    name: c.name.clone().into(),
                                    icon: channel_icon(c),
                                    private: c.private,
                                },
                            )
                        })
                    })
                    .collect();
                ranked.sort_by(|(ta, a), (tb, b)| {
                    let name = |s: &Suggestion| match s {
                        Suggestion::Channel { name, .. } => name.to_lowercase(),
                        _ => String::new(),
                    };
                    ta.cmp(tb).then(name(a).cmp(&name(b)))
                });
                ranked
                    .into_iter()
                    .take(CHANNEL_LIMIT)
                    .map(|(_, s)| s)
                    .collect()
            }
            Trigger::Emoji => {
                emoji_picker::search(&query.text, &snapshot.emoji.entries, EMOJI_LIMIT)
                    .into_iter()
                    .map(Suggestion::Emoji)
                    .collect()
            }
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(completion) = &mut self.completion {
            let count = completion.items.len() as isize;
            completion.selected =
                ((completion.selected as isize + delta).rem_euclid(count)) as usize;
            self.completion_scroll.scroll_to_item(completion.selected);
            cx.notify();
        }
    }

    /// A space typed after an exact name, or the closing colon after an
    /// exact shortcode, accepts that one match (`spaceId`).
    fn accept_terminator(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(completion) = &mut self.completion else {
            return false;
        };
        let (text, selection) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let query = &completion.query;
        let terminator = if query.trigger == Trigger::Emoji {
            ":"
        } else {
            " "
        };
        let typed = selection.is_empty()
            && selection.end == query.end + 1
            && text.get(query.end..query.end + 1) == Some(terminator)
            && text.get(query.start + 1..query.end) == Some(query.text.as_str());
        if !typed {
            return false;
        }
        let wanted = query.text.trim().to_lowercase();
        let exact: Vec<usize> = completion
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| match item {
                Suggestion::Person { name, .. } | Suggestion::Channel { name, .. } => {
                    name.to_lowercase() == wanted
                }
                Suggestion::Emoji(candidate) => candidate.shortcode().to_lowercase() == wanted,
            })
            .map(|(index, _)| index)
            .collect();
        let [index] = exact[..] else {
            return false;
        };
        completion.query.end += 1;
        self.accept(index, window, cx);
        true
    }

    fn accept(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let Some(item) = completion.items.get(index) else {
            return;
        };
        let range = completion.query.start..completion.query.end;
        let token = match item {
            Suggestion::Person { pubkey, name, .. } => {
                Some(InlineToken::new(pubkey.clone(), format!("@{name}")))
            }
            Suggestion::Channel { id, name, .. } => Some(
                InlineToken::new(id.clone(), channel_reference(id, name))
                    .with_label(format!("#{name}")),
            ),
            Suggestion::Emoji(_) => None,
        };
        let text = match item {
            Suggestion::Emoji(candidate) => candidate.pick(None).text(),
            Suggestion::Person { name, .. } => format!("@{name} "),
            Suggestion::Channel { id, name, .. } => format!("{} ", channel_reference(id, name)),
        };
        self.input.update(cx, |state, cx| {
            let chip = token.and_then(|token| {
                state
                    .replace_range_with_token(range.clone(), token, window, cx)
                    .ok()
            });
            if chip.is_some() {
                state.insert(" ", window, cx);
            } else {
                state.set_selected_range(range.clone(), cx);
                state.replace(text, window, cx);
            }
            state.focus(window, cx);
        });
        self.completion = None;
        cx.notify();
    }

    // ----- formatting -----

    fn format(&mut self, format: Format, window: &mut Window, cx: &mut Context<Self>) {
        let (text, selection) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let edit = format::apply(&text, selection, format);
        self.input.update(cx, |state, cx| {
            for (range, replacement) in edit.changes.iter().rev() {
                state.set_selected_range(range.clone(), cx);
                state.replace(replacement.clone(), window, cx);
            }
            state.set_selected_range(edit.selection, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn shortcut(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if !modifiers.secondary() || modifiers.function {
            return;
        }
        let key = keystroke.key.to_lowercase();
        let format = match (modifiers.shift, modifiers.alt, key.as_str()) {
            (false, false, "b") => Format::Bold,
            (false, false, "i") => Format::Italic,
            (false, false, "e") => Format::Code,
            (true, false, "x") => Format::Strike,
            (true, false, "p") => Format::Spoiler,
            (false, true, "c") => Format::CodeBlock,
            (true, false, "8" | "*") => Format::Bullet,
            (true, false, "7" | "&") => Format::Ordered,
            (true, false, "b") => Format::Quote,
            _ => return,
        };
        cx.stop_propagation();
        if !event.is_held {
            self.format(format, window, cx);
        }
    }

    // ----- attachments -----

    fn pick_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |this, cx| this.add_paths(paths, cx));
            }
        })
        .detach();
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let read = cx.background_executor().spawn(async move {
            paths
                .into_iter()
                .filter(|path| path.is_file())
                .map(|path| {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "File".into());
                    let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_UPLOAD);
                    let bytes = if too_big {
                        Err(UploadError::Size)
                    } else {
                        std::fs::read(&path).map_err(|_| UploadError::Failed)
                    };
                    (name, bytes)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let files = read.await;
            let _ = this.update(cx, |this, cx| {
                for (name, bytes) in files {
                    this.add_file(name, bytes, cx);
                }
            });
        })
        .detach();
    }

    fn add_file(
        &mut self,
        name: String,
        bytes: Result<Vec<u8>, UploadError>,
        cx: &mut Context<Self>,
    ) {
        if self.editing.is_some() {
            return;
        }
        if self.attachments.len() >= MAX_FILES {
            self.notice = Some(format!("Attach at most {MAX_FILES} files per message.").into());
            cx.notify();
            return;
        }
        self.next_attachment += 1;
        let (mime, size, thumbnail, status, bytes) = match bytes {
            Ok(bytes) => {
                let mime = upload::mime_type(&name, &bytes);
                let size = bytes.len() as u64;
                let status = if size == 0 || size > MAX_UPLOAD || size > media_byte_limit(&mime) {
                    Status::Failed(UploadError::Size.to_string().into())
                } else {
                    Status::Queued
                };
                (
                    mime.clone(),
                    size,
                    thumbnail(&mime, &bytes),
                    status,
                    Some(bytes),
                )
            }
            Err(error) => (
                "application/octet-stream".into(),
                0,
                None,
                Status::Failed(error.to_string().into()),
                None,
            ),
        };
        self.attachments.push(Pending {
            id: self.next_attachment,
            name,
            mime,
            size,
            thumbnail,
            bytes,
            status,
        });
        self.pump(cx);
        cx.notify();
    }

    /// Uploads run one at a time, in order (`prepareForSend`).
    fn pump(&mut self, cx: &mut Context<Self>) {
        if self
            .attachments
            .iter()
            .any(|item| matches!(item.status, Status::Uploading))
        {
            return;
        }
        let Some(item) = self
            .attachments
            .iter_mut()
            .find(|item| matches!(item.status, Status::Queued))
        else {
            return;
        };
        item.status = Status::Uploading;
        let (id, file) = (
            item.id,
            UploadFile {
                name: item.name.clone(),
                mime: item.mime.clone(),
                bytes: item.bytes.clone().unwrap_or_default(),
            },
        );
        let media = self.community.read(cx).session.media().clone();
        cx.spawn(async move |this, cx| {
            let result = upload::upload(&media, file).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(item) = this.attachments.iter_mut().find(|item| item.id == id) {
                    match result {
                        Ok(attachment) => {
                            item.bytes = None;
                            item.status = Status::Ready(attachment);
                        }
                        Err(error) => item.status = Status::Failed(error.to_string().into()),
                    }
                }
                this.pump(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        self.attachments.retain(|item| item.id != id);
        self.notice = None;
        self.pump(cx);
        cx.notify();
    }

    fn retry_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(item) = self.attachments.iter_mut().find(|item| item.id == id)
            && item.bytes.is_some()
        {
            item.status = Status::Queued;
        }
        self.pump(cx);
        cx.notify();
    }

    /// Images and files from the clipboard. Text pastes as usual.
    fn paste(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let mut consumed = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    consumed = true;
                    let extension = match image.format {
                        ImageFormat::Png => "png",
                        ImageFormat::Jpeg => "jpg",
                        ImageFormat::Gif => "gif",
                        ImageFormat::Webp => "webp",
                        _ => "img",
                    };
                    self.add_file(
                        format!("pasted-image.{extension}"),
                        Ok(image.bytes.clone()),
                        cx,
                    );
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    consumed = true;
                    self.add_paths(paths.paths().to_vec(), cx);
                }
                ClipboardEntry::String(_) => {}
            }
        }
        consumed
    }
}

/// What "Edit message" puts in the composer: the body with its attachment
/// Markdown (`messageEditText`).
pub fn edit_text(row: &MessageRow) -> String {
    row.source_content
        .clone()
        .unwrap_or_else(|| row.content.clone())
}

/// The text to edit, with its channel references back as chips.
fn edit_content(text: &str) -> InputContent {
    const LINK: &str = "](buzz://channel/";
    let mut content = InputContent::new(text.to_string());
    let mut from = 0;
    while let Some(open) = text[from..].find("[\\#").map(|i| from + i) {
        from = open + 3;
        let Some(link) = text[open..].find(LINK).map(|i| open + i) else {
            break;
        };
        let Some(close) = text[link..].find(')').map(|i| link + i) else {
            break;
        };
        let label = &text[open + 3..link];
        let id = &text[link + LINK.len()..close];
        let name: String = {
            let mut name = String::new();
            let mut escaped = false;
            for c in label.chars() {
                if c == '\\' && !escaped {
                    escaped = true;
                } else {
                    name.push(c);
                    escaped = false;
                }
            }
            name
        };
        let id = url::form_urlencoded::parse(format!("x={id}").as_bytes())
            .next()
            .map(|(_, id)| id.into_owned())
            .unwrap_or_default();
        if name.is_empty() || id.is_empty() || channel_reference(&id, &name) != text[open..=close] {
            continue;
        }
        let chip =
            InlineToken::new(id, text[open..=close].to_string()).with_label(format!("#{name}"));
        if let Ok(next) = content.clone().with_token(open..close + 1, chip) {
            content = next;
        }
        from = close + 1;
    }
    content
}

/// Splits the chosen chips into notified channel members (`p`) and
/// display-only references (`mention`), as a DM always does with
/// outsiders (§5d.3). The reference's "add them to the channel?" prompt
/// for outsiders is not offered here.
pub fn mentions(community: &Community, channel: &str, mentioned: &[String]) -> Mentions {
    let members = community
        .snapshot
        .channels
        .iter()
        .find(|c| c.id == channel)
        .map(|c| c.members.as_slice())
        .unwrap_or_default();
    let (recipients, references) = mentioned
        .iter()
        .cloned()
        .partition(|key| members.contains(key));
    Mentions {
        recipients,
        references,
    }
}

/// `[#name](buzz://channel/<id>)`, as `ChannelCompletion` inserts it.
fn channel_reference(id: &str, name: &str) -> String {
    let mut label = String::from("\\#");
    for c in name.chars() {
        if "\\`*_[]()#<>~&".contains(c) {
            label.push('\\');
        }
        label.push(c);
    }
    let id: String = url::form_urlencoded::byte_serialize(id.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    format!("[{label}](buzz://channel/{id})")
}

fn thumbnail(mime: &str, bytes: &[u8]) -> Option<Arc<Image>> {
    let format = match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => return None,
    };
    Some(Arc::new(Image::from_bytes(format, bytes.to_vec())))
}

/// `formatFileSize` with one decimal under 10 (§4.8: "12 KB", "1.2 MB").
fn file_size(size: u64) -> String {
    const K: f64 = 1024.;
    let size = size as f64;
    let (value, unit) = if size < K {
        return format!("{size} B");
    } else if size < K * K {
        (size / K, "KB")
    } else if size < K * K * K {
        (size / K / K, "MB")
    } else {
        (size / K / K / K, "GB")
    };
    if value < 10. {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.0} {unit}")
    }
}

/// Name match tiers: exact, prefix, word prefix, substring.
fn name_match(name: &str, query: &str) -> Option<u8> {
    let query = query.trim_start().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let name = name.to_lowercase();
    if name == query.trim_end() {
        Some(0)
    } else if name.starts_with(&query) {
        Some(1)
    } else if name
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(&query))
    {
        Some(2)
    } else if name.contains(&query) {
        Some(3)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Mention,
    Channel,
    Emoji,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Query {
    trigger: Trigger,
    /// The sigil's byte offset.
    start: usize,
    /// The caret.
    end: usize,
    text: String,
}

/// The active `@`, `#` or `:` query ending at `caret` (`mentionQuery`,
/// `channelQuery`, `emojiQuery`): the sigil at the start of the text or
/// after whitespace or `([{`, at most 160 characters back.
fn find_query(text: &str, caret: usize) -> Option<Query> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let before = &text[..caret];
    let boundary = |start: usize| {
        before[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || "([{".contains(c))
    };
    let query = |trigger, index: usize| Query {
        trigger,
        start: index,
        end: caret,
        text: before[index + 1..].to_string(),
    };
    // What the text after the current character still allows.
    let (mut channel, mut emoji) = (true, true);
    for (length, (index, c)) in before.char_indices().rev().enumerate() {
        if length > 160 || matches!(c, '\n' | '\r' | '\t') {
            return None;
        }
        match c {
            '@' if boundary(index) => return Some(query(Trigger::Mention, index)),
            '#' if channel && boundary(index) => return Some(query(Trigger::Channel, index)),
            ':' if emoji && boundary(index) && (2..=64).contains(&(caret - index - 1)) => {
                return Some(query(Trigger::Emoji, index));
            }
            '@' => return None,
            _ => {}
        }
        channel &= !"#[](){}`<>\\".contains(c);
        emoji &= c.is_ascii_alphanumeric() || "_+-".contains(c);
    }
    None
}

struct Completion {
    query: Query,
    items: Vec<Suggestion>,
    selected: usize,
}

enum Suggestion {
    Person {
        pubkey: String,
        name: SharedString,
        picture: Option<String>,
        agent: bool,
        member: bool,
        dm: bool,
    },
    Channel {
        id: String,
        name: SharedString,
        icon: IconName,
        private: bool,
    },
    Emoji(Candidate),
}

struct Pending {
    id: u64,
    name: String,
    mime: String,
    size: u64,
    thumbnail: Option<Arc<Image>>,
    /// Kept until the upload succeeds, for retries.
    bytes: Option<Vec<u8>>,
    status: Status,
}

enum Status {
    Queued,
    Uploading,
    Ready(Attachment),
    Failed(SharedString),
}

impl Status {
    fn label(&self) -> &str {
        match self {
            Status::Queued => "Queued",
            Status::Uploading => "Uploading…",
            Status::Ready(_) => "Ready",
            Status::Failed(_) => "Upload failed",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Draft {
    text: String,
    #[serde(default)]
    tokens: Vec<DraftToken>,
}

#[derive(Serialize, Deserialize)]
struct DraftToken {
    start: usize,
    end: usize,
    id: String,
    text: String,
    label: String,
}

impl From<&InputContent> for Draft {
    fn from(content: &InputContent) -> Self {
        Draft {
            text: content.text().to_string(),
            tokens: content
                .tokens()
                .iter()
                .map(|span| DraftToken {
                    start: span.range().start,
                    end: span.range().end,
                    id: span.token().id().to_string(),
                    text: span.token().text().to_string(),
                    label: span.token().label().to_string(),
                })
                .collect(),
        }
    }
}

impl Draft {
    /// The text with whichever chips still fit it.
    fn content(&self) -> InputContent {
        let mut content = InputContent::new(self.text.clone());
        for token in &self.tokens {
            let chip = InlineToken::new(token.id.clone(), token.text.clone())
                .with_label(token.label.clone());
            if let Ok(next) = content.clone().with_token(token.start..token.end, chip) {
                content = next;
            }
        }
        content
    }
}

// ---------------------------------------------------------------------------
// Rendering

impl Composer {
    fn edit_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .mt(px(-12.))
            .mx(px(-6.))
            .mb(px(-12.))
            .px(px(12.))
            .py(px(8.))
            .rounded_t(px(19.))
            .bg(c.surface_inset)
            .text_label_sm()
            .text_color(c.text_subtle)
            .child(Icon::new(IconName::PencilSimple).size(18.))
            .child(div().flex_1().child("Editing message"))
            .child(
                IconButton::new("cancel-edit", IconName::X)
                    .size(IconButtonSize::Sm)
                    .icon_size(18.)
                    .tooltip("Cancel edit")
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))),
            )
    }

    fn context_box(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = *cx.colors();
        let context = self.context.as_ref()?;
        let cancel = IconButton::new("cancel-reply", IconName::X)
            .size(IconButtonSize::Sm)
            .tooltip("Cancel reply target")
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::CancelReply)));
        Some(match context {
            ReplyContext::Parent { name, excerpt } => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(12.))
                .bg(c.surface_inset)
                .text_caption()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_color(c.text_standard)
                                .child(format!("Replying to {name}")),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_color(c.text_subtle)
                                .child(excerpt.clone()),
                        ),
                )
                .child(cancel)
                .into_any_element(),
            ReplyContext::Missing => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(12.))
                .bg(c.surface_inset)
                .text_caption()
                .text_color(c.text_subtle)
                .child(div().flex_1().child("Reply target is no longer available."))
                .child(cancel)
                .into_any_element(),
        })
    }

    fn attachment_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() || self.editing.is_some() {
            return None;
        }
        let c = *cx.colors();
        let items = self.attachments.iter().map(|item| {
            let id = item.id;
            let busy = matches!(item.status, Status::Uploading);
            let failed = match &item.status {
                Status::Failed(error) => Some(error.clone()),
                _ => None,
            };
            let remove = div()
                .id(("remove-attachment", id))
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(c.affordance_prominent)
                .text_color(c.text_inverse)
                .cursor_pointer()
                .tooltip(Tooltip::text(format!("Remove {}", item.name)))
                .map(|d| {
                    if busy {
                        d.child(Spinner::new().size(12.))
                    } else {
                        d.child(Icon::new(IconName::X).size(12.))
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(id, cx)));
            let retry = (failed.is_some() && item.bytes.is_some()).then(|| {
                IconButton::new(("retry-attachment", id), IconName::ArrowsClockwise)
                    .size(IconButtonSize::Xs)
                    .tooltip("Retry upload")
                    .on_click(cx.listener(move |this, _, _, cx| this.retry_attachment(id, cx)))
            });
            let issue = failed.clone().map(|error| {
                div()
                    .id(("attachment-issue", id))
                    .text_color(c.text_danger)
                    .tooltip(Tooltip::text(error))
                    .child(Icon::new(IconName::WarningCircle).size(14.))
            });
            let actions = div()
                .absolute()
                .top(px(2.))
                .right(px(2.))
                .flex()
                .items_center()
                .gap(px(2.))
                .p(px(2.))
                .rounded_full()
                .when(item.thumbnail.is_some(), |d| d.bg(c.surface_panel))
                .children(issue)
                .children(retry)
                .child(remove);
            match &item.thumbnail {
                Some(image) => div()
                    .id(("attachment", id))
                    .relative()
                    .flex_none()
                    .size(px(46.))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .bg(c.surface_inset)
                    .tooltip(Tooltip::text(format!(
                        "{} · {} · {}",
                        item.name,
                        file_size(item.size),
                        item.status.label()
                    )))
                    .child(
                        img(image.clone())
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .when(busy, |i| i.opacity(0.6)),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(px(12.))
                            .border_1()
                            .border_color(c.border_image),
                    )
                    .child(
                        actions
                            .top(px(-2.))
                            .right(px(-2.))
                            .bg(gpui_kit::transparent_black()),
                    )
                    .into_any_element(),
                None => div()
                    .id(("attachment", id))
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .w(px(288.))
                    .h(px(46.))
                    .p(px(2.))
                    .pr(px(48.))
                    .border_1()
                    .border_color(if failed.is_some() {
                        c.border_danger
                    } else {
                        c.border_standard
                    })
                    .rounded(px(12.))
                    .child(
                        div()
                            .size(px(40.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(9.))
                            .bg(c.affordance_subtle)
                            .child(Icon::new(IconName::FileText).size(24.)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().text_label_sm().truncate().child(item.name.clone()))
                            .child(
                                div()
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .truncate()
                                    .child(format!(
                                        "{} · {}",
                                        file_size(item.size),
                                        item.status.label()
                                    )),
                            ),
                    )
                    .child(actions)
                    .into_any_element(),
            }
        });
        Some(
            div()
                .id("composer-attachments")
                .flex()
                .gap(px(8.))
                .px(px(4.))
                .overflow_x_scroll()
                .children(items)
                .into_any_element(),
        )
    }

    fn tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        if self.formatting {
            let state = self.input.read(cx);
            let (text, selection) = (state.value().to_string(), state.selected_range());
            let button = |format: Format, cx: &mut Context<Self>| {
                IconButton::new(
                    SharedString::from(format!("format-{format:?}")),
                    format.icon(),
                )
                .size(IconButtonSize::Sm)
                .selected(format::active(&text, selection.clone(), format))
                .tooltip(format.tooltip())
                .on_click(cx.listener(move |this, _, window, cx| this.format(format, window, cx)))
            };
            return div()
                .id("format-tools")
                .flex()
                .flex_1()
                .items_center()
                .min_w_0()
                .child(
                    IconButton::new("close-format", IconName::X)
                        .size(IconButtonSize::Sm)
                        .variant(ButtonVariant::Subtle)
                        .tooltip("Close formatting")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.formatting = false;
                            this.focus(window, cx);
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .w(px(1.))
                        .h(px(20.))
                        .mx(px(4.))
                        .bg(c.border_standard)
                        .opacity(0.6),
                )
                .child(
                    div()
                        .id("format-buttons")
                        .flex()
                        .gap(px(2.))
                        .overflow_x_scroll()
                        .children(Format::ALL.into_iter().map(|format| button(format, cx))),
                )
                .into_any_element();
        }
        let editing = self.editing.is_some();
        div()
            .flex()
            .flex_1()
            .items_center()
            .gap(px(4.))
            .min_h(px(36.))
            .child(
                IconButton::new("mention", IconName::At)
                    .size(IconButtonSize::Sm)
                    .tooltip("Mention a member")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let (text, caret) = {
                            let state = this.input.read(cx);
                            (state.value().to_string(), state.selected_range().start)
                        };
                        let spaced = text[..caret]
                            .chars()
                            .next_back()
                            .is_none_or(|c| c.is_whitespace() || "([{".contains(c));
                        this.insert_text(if spaced { "@" } else { " @" }, window, cx);
                    })),
            )
            .when(!editing, |d| {
                d.child(
                    IconButton::new("attach", IconName::Paperclip)
                        .size(IconButtonSize::Sm)
                        .tooltip("Attach files")
                        .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx))),
                )
            })
            .child(
                div().relative().child(
                    IconButton::new("emoji", IconName::Smiley)
                        .size(IconButtonSize::Sm)
                        .selected(self.picker_open)
                        .tooltip("Insert emoji")
                        .on_click(cx.listener(|this, _, window, cx| {
                            // The press outside that just closed it.
                            if this
                                .picker_closed
                                .take()
                                .is_some_and(|at| at.elapsed() < Duration::from_millis(300))
                            {
                                return;
                            }
                            this.picker_open = !this.picker_open;
                            if this.picker_open {
                                this.picker.update(cx, |picker, cx| {
                                    picker.reset(window, cx);
                                    picker.focus(window, cx);
                                });
                            } else {
                                this.focus(window, cx);
                            }
                            cx.notify();
                        })),
                ),
            )
            .child(
                IconButton::new("format", IconName::TextAa)
                    .size(IconButtonSize::Sm)
                    .tooltip("Toggle formatting")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.formatting = true;
                        this.focus(window, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn completion_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let completion = self.completion.as_ref()?;
        let c = *cx.colors();
        let trigger = completion.query.trigger;
        let compact = trigger == Trigger::Emoji;
        let media = self.community.read(cx).session.media().clone();
        let rows = completion.items.iter().enumerate().map(|(index, item)| {
            let selected = index == completion.selected;
            let row = div()
                .id(("completion", index))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(if compact { 9. } else { 3. }))
                .cursor_pointer()
                .when(selected, |d| d.bg(c.affordance_popover_selected))
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    if let Some(completion) = &mut this.completion
                        && completion.selected != index
                    {
                        completion.selected = index;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| this.accept(index, window, cx)));
            match item {
                Suggestion::Person {
                    pubkey,
                    name,
                    picture,
                    agent,
                    member,
                    dm,
                } => {
                    let mut avatar = Avatar::new(name.clone()).size(AvatarSize::Default);
                    if *agent {
                        avatar = avatar.squircle();
                    }
                    if let Some(picture) = picture {
                        avatar = avatar.image(media::avatar(picture, &media));
                    }
                    let detail = if *member {
                        div()
                            .font_family(buzz_ui::tokens::font::MONO)
                            .text_color(c.text_metadata)
                            .max_w(px(96.))
                            .overflow_hidden()
                            .child(pubkey[..12].to_string())
                    } else {
                        div().text_color(c.text_subtle).child(if *dm {
                            "Not in DM · Will not be notified"
                        } else {
                            "Not in channel · Will not be notified"
                        })
                    };
                    row.h(px(48.))
                        .p(px(8.))
                        .child(avatar)
                        .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                        .child(div().flex_none().text_caption().child(detail))
                        .into_any_element()
                }
                Suggestion::Channel {
                    name,
                    icon,
                    private,
                    ..
                } => row
                    .h(px(48.))
                    .p(px(8.))
                    .child(
                        div()
                            .size(px(32.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(*icon).size(22.)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().truncate().child(name.clone()))
                            .when(*private, |d| {
                                d.child(
                                    div()
                                        .text_caption()
                                        .text_color(c.text_subtle)
                                        .child("Private channel"),
                                )
                            }),
                    )
                    .when(selected, |d| {
                        d.child(
                            div()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(4.))
                                .bg(c.surface_inset)
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child("Enter"),
                        )
                    })
                    .into_any_element(),
                Suggestion::Emoji(candidate) => {
                    let preview = match candidate {
                        Candidate::Unicode(emoji) => div()
                            .text_size(px(24.))
                            .line_height(px(28.))
                            .child(emoji.as_str())
                            .into_any_element(),
                        Candidate::Custom(emoji) => img(media::image(&emoji.url, &media))
                            .size(px(24.))
                            .object_fit(ObjectFit::Contain)
                            .into_any_element(),
                    };
                    row.h(px(40.))
                        .px(px(8.))
                        .py(px(6.))
                        .child(
                            div()
                                .size(px(28.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(preview),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(format!(":{}:", candidate.shortcode())),
                        )
                        .into_any_element()
                }
            }
        });
        let (max_width, max_height, padding) = match trigger {
            Trigger::Mention => (px(380.), px(280.), px(12.)),
            Trigger::Channel => (px(480.), px(320.), px(12.)),
            Trigger::Emoji => (px(320.), px(280.), px(6.)),
        };
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_full()
                .pb(px(4.))
                .child(
                    div()
                        .id("composer-completions")
                        .occlude()
                        .w_full()
                        .max_w(max_width)
                        .p(padding)
                        .rounded(px(16.))
                        .border_1()
                        .border_color(c.border_standard)
                        .bg(c.surface_popover)
                        .shadow(buzz_ui::tokens::shadow::sm())
                        .text_body()
                        .text_color(c.text_standard)
                        .child(
                            div()
                                .id("composer-completion-list")
                                .flex()
                                .flex_col()
                                .gap(px(if compact { 0. } else { 4. }))
                                .max_h(max_height)
                                .overflow_y_scroll()
                                .track_scroll(&self.completion_scroll)
                                .children(rows),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        // The caret can move without an edit.
        let key = {
            let state = self.input.read(cx);
            (state.value().len(), state.selected_range().end)
        };
        if self.completion_key != Some(key) {
            self.refresh_completion(cx);
        }
        if self.drag_over && !cx.has_active_drag() {
            self.drag_over = false;
        }
        let can_submit = self.can_submit(cx);
        let editing = self.editing.is_some();
        let has_attachments = !self.attachments.is_empty() && !editing;
        let weak = cx.entity().downgrade();
        let token_colors = c;
        let textarea = Textarea::new(&self.input)
            .appearance(false)
            .token(move |token: &InlineTokenContext, _, _| {
                div()
                    .flex()
                    .items_center()
                    .h(token.line_height())
                    .max_w(px(256.))
                    .px(px(3.))
                    .rounded(px(4.))
                    .bg(if token.is_selected() {
                        token_colors.affordance_subtle_hover
                    } else {
                        token_colors.affordance_subtle
                    })
                    .text_color(token_colors.text_standard)
                    .overflow_hidden()
                    .child(token.token().label().clone())
            })
            .on_paste(move |item, _, cx| {
                weak.update(cx, |this, cx| this.paste(item, cx))
                    .unwrap_or(false)
            });

        let shell = div()
            .id("composer")
            .key_context("Composer")
            .flex()
            .flex_col()
            .gap(px(if has_attachments { 8. } else { 16. }))
            .pt(px(if has_attachments { 6. } else { 12. }))
            .px(px(6.))
            .pb(px(6.))
            .rounded(px(20.))
            .border_1()
            .border_color(if self.drag_over {
                c.border_focus
            } else {
                c.border_standard
            })
            .bg(if self.drag_over {
                c.surface_inset
            } else {
                c.surface_panel
            })
            .shadow_xs()
            .when(editing, |d| d.overflow_hidden())
            .text_body()
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                if this.completion.is_some() {
                    this.move_selection(-1, cx);
                    cx.stop_propagation();
                } else if this.editing.is_none()
                    && this.attachments.is_empty()
                    && this.input.read(cx).value().is_empty()
                    && this.input.read(cx).focus_handle(cx).is_focused(window)
                {
                    cx.emit(ComposerEvent::EditLast);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if this.completion.is_some() {
                    this.move_selection(1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                if let Some(completion) = &this.completion
                    && !action.shift
                {
                    let index = completion.selected;
                    this.accept(index, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                if let Some(completion) = &this.completion {
                    let index = completion.selected;
                    this.accept(index, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if let Some(completion) = this.completion.take() {
                    this.dismissed = Some(completion.query.start);
                    cx.stop_propagation();
                    cx.notify();
                } else if this.picker_open {
                    this.picker_open = false;
                    cx.stop_propagation();
                    cx.notify();
                } else if this.formatting {
                    this.formatting = false;
                    cx.stop_propagation();
                    cx.notify();
                } else if this.editing.is_some() {
                    this.cancel_edit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.shortcut(event, window, cx)
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    let over = event.bounds.contains(&event.event.position);
                    if this.drag_over != over {
                        this.drag_over = over;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.drag_over = false;
                this.add_paths(paths.paths().to_vec(), cx);
            }))
            .when(editing, |d| d.child(self.edit_header(cx)))
            .children(self.attachment_row(cx))
            .child(div().pl(px(4.)).pt(px(4.)).child(textarea))
            .when(self.drag_over, |d| {
                d.child(
                    div()
                        .px(px(8.))
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child("Drop files to attach"),
                )
            })
            .children(self.notice.clone().map(|notice| {
                div()
                    .px(px(8.))
                    .text_caption()
                    .text_color(c.text_warning)
                    .child(notice)
            }))
            .children(self.context_box(cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(self.tools(cx))
                    .child(
                        IconButton::new("send", IconName::ArrowUp)
                            .size(IconButtonSize::Sm)
                            .when(can_submit, |b| b.variant(ButtonVariant::Prominent))
                            .disabled(!can_submit)
                            .tooltip(if editing {
                                "Save changes"
                            } else {
                                "Send message"
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            );

        let picker = self.picker_open.then(|| {
            let dismiss = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.picker_open = false;
                this.picker_closed = Some(std::time::Instant::now());
                cx.notify();
            });
            div().absolute().left_0().top_0().child(deferred(
                anchored()
                    .anchor(Anchor::BottomLeft)
                    .offset(point(px(0.), px(-4.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(emoji_picker::emoji_picker(&self.picker, dismiss)),
            ))
        });

        div()
            .relative()
            .mx(px(16.))
            .mb(px(16.))
            .children(self.completion_popup(cx))
            .children(picker)
            .child(shell)
    }
}

// ---------------------------------------------------------------------------
// Formatting

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Bold,
    Italic,
    Strike,
    Code,
    CodeBlock,
    Link,
    Bullet,
    Ordered,
    Quote,
    Spoiler,
}

impl Format {
    /// The toolbar's order (§4.8).
    const ALL: [Format; 10] = [
        Format::Bold,
        Format::Italic,
        Format::Strike,
        Format::Code,
        Format::CodeBlock,
        Format::Link,
        Format::Bullet,
        Format::Ordered,
        Format::Quote,
        Format::Spoiler,
    ];

    fn icon(self) -> IconName {
        match self {
            Format::Bold => IconName::TextB,
            Format::Italic => IconName::TextItalic,
            Format::Strike => IconName::TextStrikethrough,
            Format::Code => IconName::Code,
            Format::CodeBlock => IconName::CodeBlock,
            Format::Link => IconName::Link,
            Format::Bullet => IconName::ListBullets,
            Format::Ordered => IconName::ListNumbers,
            Format::Quote => IconName::Quotes,
            Format::Spoiler => IconName::Detective,
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Format::Bold => "Bold (Ctrl+B)",
            Format::Italic => "Italic (Ctrl+I)",
            Format::Strike => "Strikethrough (Ctrl+Shift+X)",
            Format::Code => "Code (Ctrl+E)",
            Format::CodeBlock => "Code block (Ctrl+Alt+C)",
            Format::Link => "Link",
            Format::Bullet => "Bullet list (Ctrl+Shift+8)",
            Format::Ordered => "Ordered list (Ctrl+Shift+7)",
            Format::Quote => "Quote (Ctrl+Shift+B)",
            Format::Spoiler => "Spoiler (Ctrl+Shift+P)",
        }
    }
}

/// Markdown edits for the formatting toolbar. Edits are insertions and
/// deletions in the original text's offsets, so chips elsewhere survive.
mod format {
    use super::Format;
    use std::ops::Range;

    pub struct Edit {
        /// Ascending, non-overlapping; apply from the last.
        pub changes: Vec<(Range<usize>, String)>,
        pub selection: Range<usize>,
    }

    fn marker(format: Format, text: &str, selection: &Range<usize>) -> Option<&'static str> {
        Some(match format {
            Format::Bold => "**",
            Format::Italic => {
                // `_` inside a word is literal; `*` binds there (§5d.3).
                let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
                if word(text[..selection.start].chars().next_back())
                    || word(text[selection.end..].chars().next())
                {
                    "*"
                } else {
                    "_"
                }
            }
            Format::Strike => "~~",
            Format::Code => "`",
            Format::Spoiler => "||",
            _ => return None,
        })
    }

    fn line_start(text: &str, at: usize) -> usize {
        text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(text: &str, at: usize) -> usize {
        text[at..].find('\n').map_or(text.len(), |i| at + i)
    }

    /// The selected lines' start offsets and the length of their list or
    /// quote prefix, if they have this format's.
    fn lines(text: &str, selection: &Range<usize>, format: Format) -> Vec<(usize, Option<usize>)> {
        let start = line_start(text, selection.start);
        let end = line_end(text, selection.end.max(selection.start));
        let mut out = Vec::new();
        let mut at = start;
        for line in text[start..end].split('\n') {
            let prefix = match format {
                Format::Bullet => ["- ", "* "]
                    .iter()
                    .find(|p| line.starts_with(**p))
                    .map(|p| p.len()),
                Format::Quote => line
                    .starts_with("> ")
                    .then_some(2)
                    .or(line.starts_with('>').then_some(1)),
                Format::Ordered => {
                    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
                    (digits > 0 && line[digits..].starts_with(". ")).then_some(digits + 2)
                }
                _ => None,
            };
            out.push((at, prefix));
            at += line.len() + 1;
        }
        out
    }

    pub fn active(text: &str, selection: Range<usize>, format: Format) -> bool {
        if selection.end > text.len() {
            return false;
        }
        match format {
            Format::Bullet | Format::Ordered | Format::Quote => lines(text, &selection, format)
                .iter()
                .all(|(_, prefix)| prefix.is_some()),
            Format::CodeBlock => {
                text[..selection.start].ends_with("```\n")
                    && text[selection.end..].starts_with("\n```")
            }
            Format::Link => false,
            _ => {
                let Some(m) = marker(format, text, &selection) else {
                    return false;
                };
                text[..selection.start].ends_with(m) && text[selection.end..].starts_with(m)
            }
        }
    }

    pub fn apply(text: &str, selection: Range<usize>, format: Format) -> Edit {
        let selection = selection.start.min(text.len())..selection.end.min(text.len());
        match format {
            Format::Bullet | Format::Ordered | Format::Quote => {
                let lines = lines(text, &selection, format);
                let remove = lines.iter().all(|(_, prefix)| prefix.is_some());
                let mut changes = Vec::new();
                let mut shift_start = 0isize;
                let mut shift_end = 0isize;
                for (index, (at, prefix)) in lines.iter().enumerate() {
                    let delta = if remove {
                        let length = prefix.expect("every line has the prefix");
                        changes.push((*at..*at + length, String::new()));
                        -(length as isize)
                    } else {
                        let insert = match format {
                            Format::Bullet => "- ".to_string(),
                            Format::Quote => "> ".to_string(),
                            _ => format!("{}. ", index + 1),
                        };
                        let length = insert.len() as isize;
                        changes.push((*at..*at, insert));
                        length
                    };
                    if *at <= selection.start {
                        shift_start += delta;
                    }
                    if *at <= selection.end {
                        shift_end += delta;
                    }
                }
                let start = (selection.start as isize + shift_start).max(0) as usize;
                let end = (selection.end as isize + shift_end).max(start as isize) as usize;
                Edit {
                    changes,
                    selection: start..end,
                }
            }
            Format::CodeBlock => {
                if active(text, selection.clone(), format) {
                    return Edit {
                        changes: vec![
                            (selection.start - 4..selection.start, String::new()),
                            (selection.end..selection.end + 4, String::new()),
                        ],
                        selection: selection.start - 4..selection.end - 4,
                    };
                }
                let open = if selection.start == 0 || text[..selection.start].ends_with('\n') {
                    "```\n"
                } else {
                    "\n```\n"
                };
                let close =
                    if selection.end == text.len() || text[selection.end..].starts_with('\n') {
                        "\n```"
                    } else {
                        "\n```\n"
                    };
                Edit {
                    selection: selection.start + open.len()..selection.end + open.len(),
                    changes: vec![
                        (selection.start..selection.start, open.into()),
                        (selection.end..selection.end, close.into()),
                    ],
                }
            }
            Format::Link => {
                const URL: &str = "https://";
                let label = selection.end - selection.start;
                let url_at = selection.start + 1 + label + 2;
                Edit {
                    changes: vec![
                        (selection.start..selection.start, "[".into()),
                        (selection.end..selection.end, format!("]({URL})")),
                    ],
                    selection: if label == 0 {
                        selection.start + 1..selection.start + 1
                    } else {
                        url_at..url_at + URL.len()
                    },
                }
            }
            _ => {
                let m = marker(format, text, &selection).expect("an inline format");
                let n = m.len();
                if active(text, selection.clone(), format) {
                    return Edit {
                        changes: vec![
                            (selection.start - n..selection.start, String::new()),
                            (selection.end..selection.end + n, String::new()),
                        ],
                        selection: selection.start - n..selection.end - n,
                    };
                }
                let inner = &text[selection.clone()];
                if inner.len() >= 2 * n && inner.starts_with(m) && inner.ends_with(m) {
                    return Edit {
                        changes: vec![
                            (selection.start..selection.start + n, String::new()),
                            (selection.end - n..selection.end, String::new()),
                        ],
                        selection: selection.start..selection.end - 2 * n,
                    };
                }
                Edit {
                    changes: vec![
                        (selection.start..selection.start, m.into()),
                        (selection.end..selection.end, m.into()),
                    ],
                    selection: selection.start + n..selection.end + n,
                }
            }
        }
    }

    #[cfg(test)]
    pub fn run(text: &str, selection: Range<usize>, format: Format) -> (String, Range<usize>) {
        let edit = apply(text, selection, format);
        let mut out = text.to_string();
        for (range, replacement) in edit.changes.iter().rev() {
            out.replace_range(range.clone(), replacement);
        }
        (out, edit.selection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn query(text: &str) -> Option<(Trigger, String)> {
        find_query(text, text.len()).map(|q| (q.trigger, q.text))
    }

    #[test]
    fn finds_completion_queries_like_the_reference() {
        assert_eq!(query("hi @al"), Some((Trigger::Mention, "al".into())));
        assert_eq!(query("@"), Some((Trigger::Mention, String::new())));
        assert_eq!(
            query("(@Alice Sm"),
            Some((Trigger::Mention, "Alice Sm".into()))
        );
        assert_eq!(query("mail@host"), None);
        assert_eq!(query("see #gen"), Some((Trigger::Channel, "gen".into())));
        assert_eq!(query("a#b"), None);
        assert_eq!(query(":smi"), Some((Trigger::Emoji, "smi".into())));
        assert_eq!(query("ok :s"), None, "two characters at least");
        assert_eq!(query("at 12:30"), None);
        assert_eq!(query("@al\nnext"), None);
        let text = "x @bo";
        assert_eq!(find_query(text, text.len()).unwrap().start, 2);
    }

    #[test]
    fn edits_channel_references_as_chips() {
        let link = channel_reference("b8fb-1", "random");
        let text = format!("about {link} now");
        let content = edit_content(&text);
        assert_eq!(content.tokens().len(), 1);
        let span = &content.tokens()[0];
        assert_eq!(span.token().id().as_ref(), "b8fb-1");
        assert_eq!(span.token().label().as_ref(), "#random");
        assert_eq!(&text[span.range()], link);
        assert!(edit_content("[\\#x](https://x)").tokens().is_empty());
    }

    #[test]
    fn ranks_names() {
        assert_eq!(name_match("Alice", ""), Some(0));
        assert_eq!(name_match("Alice", "alice"), Some(0));
        assert_eq!(name_match("Alice", "al"), Some(1));
        assert_eq!(name_match("Mary Alice", "ali"), Some(2));
        assert_eq!(name_match("Malice", "lic"), Some(3));
        assert_eq!(name_match("Bob", "x"), None);
    }

    #[test]
    fn references_channels_as_markdown_links() {
        assert_eq!(
            channel_reference("0e9d-1", "general_chat"),
            "[\\#general\\_chat](buzz://channel/0e9d-1)"
        );
        assert_eq!(file_size(12 * 1024), "12 KB");
        assert_eq!(file_size(1_258_291), "1.2 MB");
    }

    #[test]
    fn wraps_and_unwraps_inline_formats() {
        use format::run;
        assert_eq!(
            run("say hi", 4..6, Format::Bold),
            ("say **hi**".into(), 6..8)
        );
        assert_eq!(
            run("say **hi**", 6..8, Format::Bold),
            ("say hi".into(), 4..6)
        );
        assert_eq!(
            run("say **hi**", 4..10, Format::Bold),
            ("say hi".into(), 4..6)
        );
        assert_eq!(run("", 0..0, Format::Code), ("``".into(), 1..1));
        assert_eq!(run("a b", 2..3, Format::Italic), ("a _b_".into(), 3..4));
        assert_eq!(run("ab", 1..2, Format::Italic), ("a*b*".into(), 2..3));
        assert_eq!(run("x", 0..1, Format::Strike), ("~~x~~".into(), 2..3));
        assert_eq!(run("x", 0..1, Format::Spoiler), ("||x||".into(), 2..3));
        assert_eq!(
            run("see docs", 4..8, Format::Link),
            ("see [docs](https://)".into(), 11..19)
        );
    }

    #[test]
    fn prefixes_and_unprefixes_lines() {
        use format::run;
        assert_eq!(run("a\nb", 0..3, Format::Bullet), ("- a\n- b".into(), 2..7));
        assert_eq!(run("- a\n- b", 2..7, Format::Bullet), ("a\nb".into(), 0..3));
        assert_eq!(
            run("a\nb", 0..3, Format::Ordered),
            ("1. a\n2. b".into(), 3..9)
        );
        assert_eq!(
            run("x\nquote", 3..3, Format::Quote),
            ("x\n> quote".into(), 5..5)
        );
        assert_eq!(
            run("code", 0..4, Format::CodeBlock),
            ("```\ncode\n```".into(), 4..8)
        );
        assert_eq!(
            run("```\ncode\n```", 4..8, Format::CodeBlock),
            ("code".into(), 0..4)
        );
        assert_eq!(
            run("see code here", 4..8, Format::CodeBlock),
            ("see \n```\ncode\n```\n here".into(), 9..13)
        );
    }
}
