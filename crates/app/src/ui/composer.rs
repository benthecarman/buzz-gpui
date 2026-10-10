//! The message composer shared by the channel page and the thread panel
//! (ui.md §4.8-4.9, `MessageComposer.tsx`, `ComposerCompletions.tsx`).
//!
//! It owns the text, mention chips, `@` `#` `:` completion, the emoji
//! picker, the formatting toolbar, attachments and the saved draft. The
//! page that hosts it decides what a submission means (a message, a
//! reply or an edit) and reports back with [`Composer::sent`] or
//! [`Composer::failed`].

mod live_markdown;

use super::emoji_picker::{self, Candidate, Dismissed, EmojiPicker, Picked};
use super::gif_picker::PickedGif;
use super::media;
use super::media::recorder::{self, Recorder};
use super::message::{self, channel_icon};
use super::message_edit;
use super::overlay;
use crate::community::{AppState, Community};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::members::can_add_members;
use buzz_relay_client::messages::media_byte_limit;
use buzz_relay_client::upload::{self, MAX_UPLOAD, UploadError, UploadFile};
use buzz_relay_client::{Attachment, ChannelSummary, ChannelType, EventId, Mentions, Partition};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, ButtonVariant, Dialog, Icon, IconButton,
    IconButtonShape, IconButtonSize, IconName, Spinner, Surface, Tooltip, TypeStyled,
};
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, InlineToken, InlineTokenContext, InputContent, InputEvent,
    MoveDown, MoveUp, Textarea, TextareaState,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use live_markdown::LiveMarkdown;
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
    /// "Retry edit": republish the edit still in the outbox.
    RetryEdit,
}

#[derive(Clone)]
pub struct Submission {
    pub content: String,
    /// Who the chosen mention chips notify (`p`) and who they only name
    /// (`mention`), first use first (§5d.3).
    pub mentions: Mentions,
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
    /// The Markdown styling of `input`'s text.
    markdown: LiveMarkdown,
    placeholder: SharedString,
    sending: bool,
    /// The host allows sending (a thread needs a loaded reply target).
    can_send: bool,
    editing: Option<Arc<MessageRow>>,
    /// The text the edit started from.
    edit_initial: Option<String>,
    /// The edit's delivery, as the host reports it.
    edit_status: message_edit::EditStatus,
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
    /// The send waiting on "Mention people outside this channel?".
    outsiders: Option<Outsiders>,
    /// A voice note being recorded or prepared, in place of the tools.
    capture: Option<Capture>,
    _capture_task: Option<Task<()>>,
    outsiders_focus: FocusHandle,
    /// The agents the message being sent mentions as recipients, `(pubkey,
    /// chip text)`, kept for the next draft ("Remember mentioned agents").
    followup: Vec<(String, String)>,
    _subscriptions: Vec<Subscription>,
}

/// Voice note capture (`VoiceCapture.tsx`).
enum Capture {
    Recording(Recorder),
    Preparing,
    Failed(SharedString),
}

/// A recorded note attached to the draft: it plays from memory under
/// `key` until it is sent.
struct VoiceDraft {
    key: String,
    duration: f64,
    waveform: Vec<u8>,
}

/// `formatVoiceNoteDuration`: whole seconds as `m:ss`.
fn voice_time(seconds: f64) -> String {
    let whole = if seconds.is_finite() {
        seconds.max(0.).floor() as u64
    } else {
        0
    };
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// A send that mentions people outside the channel, held until the user
/// invites them or sends without them (`useNonmemberMentions.tsx`).
struct Outsiders {
    /// Every chosen chip as a recipient.
    submission: Submission,
    missing: Vec<String>,
    can_add: bool,
    busy: bool,
    error: Option<SharedString>,
    /// The text the prompt is about; inviting stops if it changes.
    draft: String,
}

/// What a send with mention chips needs (`MessageComposer.tsx:1069-1101`).
#[derive(Debug, PartialEq)]
enum Admission {
    Send(Mentions),
    /// Some recipients are outside a stream or forum: ask first.
    Ask {
        missing: Vec<String>,
        can_add: bool,
    },
}

/// Recipients must be channel members. A DM (or a session) quietly turns
/// outsiders into references; any other channel asks. With the roster
/// unknown, everyone stays a recipient and the writer's check reports it.
fn admission(channel: Option<&ChannelSummary>, viewer: &str, recipients: Vec<String>) -> Admission {
    let Some(channel) = channel.filter(|c| !c.members.is_empty()) else {
        return Admission::Send(Mentions {
            recipients,
            references: Vec::new(),
        });
    };
    let (members, missing): (Vec<String>, Vec<String>) = recipients
        .iter()
        .cloned()
        .partition(|key| channel.members.contains(key));
    if missing.is_empty() {
        return Admission::Send(Mentions {
            recipients,
            references: Vec::new(),
        });
    }
    if matches!(
        channel.channel_type,
        Some(ChannelType::Dm | ChannelType::Session)
    ) {
        return Admission::Send(Mentions {
            recipients: members,
            references: missing,
        });
    }
    let member = channel.members.iter().any(|key| key == viewer);
    Admission::Ask {
        missing,
        can_add: can_add_members(channel, member),
    }
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
        let picker = cx.new(|cx| {
            let mut picker = EmojiPicker::new(community.clone(), window, cx);
            picker.enable_gifs(window, cx);
            picker
        });
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
                cx.subscribe_in(
                    &picker,
                    window,
                    |this, _, PickedGif(markdown), window, cx| {
                        this.picker_open = false;
                        this.insert_text(markdown, window, cx);
                    },
                ),
                cx.subscribe_in(&picker, window, |this, _, _: &Dismissed, window, cx| {
                    this.picker_open = false;
                    this.focus(window, cx);
                    cx.notify();
                }),
                cx.observe(&community, |_, _, cx| cx.notify()),
            ];
        cx.on_release(|this: &mut Self, cx| this.write_draft(cx))
            .detach();
        let markdown = LiveMarkdown::new(&input, cx);
        let mut composer = Self {
            community,
            channel,
            thread_root,
            input,
            markdown,
            placeholder,
            sending: false,
            can_send: true,
            editing: None,
            edit_initial: None,
            edit_status: message_edit::EditStatus::default(),
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
            outsiders: None,
            outsiders_focus: cx.focus_handle(),
            capture: None,
            _capture_task: None,
            followup: Vec::new(),
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

    #[allow(dead_code)]
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

    /// The text the current edit started from.
    pub fn edit_initial(&self) -> Option<&str> {
        self.edit_initial.as_deref()
    }

    /// Shows the edit's delivery: locked while sent, busy while sending,
    /// and "Retry edit" when it failed.
    pub fn set_edit_status(&mut self, status: message_edit::EditStatus, cx: &mut Context<Self>) {
        if self.edit_status != status {
            self.edit_status = status;
            cx.notify();
        }
    }

    /// A message under the text, such as why an edit cannot be saved.
    pub fn set_notice(&mut self, notice: Option<SharedString>, cx: &mut Context<Self>) {
        self.notice = notice;
        self.sending = false;
        cx.notify();
    }

    /// Puts `row`'s text in the composer to edit it (§4.8 edit mode). The
    /// draft waits until the edit ends. Another edit must end first.
    pub fn start_edit(
        &mut self,
        row: Arc<MessageRow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .editing
            .as_ref()
            .is_some_and(|editing| editing.id != row.id)
        {
            self.notice = Some(message_edit::BUSY.into());
            self.input.update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
            return;
        }
        if self.editing.is_none() {
            self.write_draft(cx);
            self.stash = Some(self.input.read(cx).content());
        }
        let text = {
            let community = self.community.read(cx);
            let directory = super::markdown::Directory::of(community, None, cx);
            message_edit::edit_text(&row, &directory.profiles, &directory.names)
        };
        let content = edit_content(&text);
        let end = content.text().len();
        self.edit_initial = Some(content.text().to_string());
        self.edit_status = message_edit::EditStatus::default();
        self.notice = None;
        self.editing = Some(row);
        self.completion = None;
        self.input.update(cx, |state, cx| {
            state.set_value(content, window, cx);
            state.set_selected_range(end..end, cx);
            state.set_placeholder("Edit message", window, cx);
            state.focus(window, cx);
        });
        self.restyle(cx);
        cx.notify();
    }

    pub fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit_status.busy || self.editing.take().is_none() {
            return;
        }
        self.edit_initial = None;
        self.edit_status = message_edit::EditStatus::default();
        self.notice = None;
        self.sending = false;
        let stash = self.stash.take().unwrap_or_else(|| InputContent::new(""));
        let placeholder = self.placeholder.clone();
        self.input.update(cx, |state, cx| {
            let end = stash.text().len();
            state.set_value(stash, window, cx);
            state.set_selected_range(end..end, cx);
            state.set_placeholder(placeholder, window, cx);
        });
        self.restyle(cx);
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
        for item in self.attachments.drain(..) {
            if let Some(voice) = &item.voice {
                media::forget_local(&voice.key, cx);
            }
        }
        self.completion = None;
        let followup = std::mem::take(&mut self.followup);
        let next = if crate::ui::settings::remember_mentioned_agents(cx) {
            let (text, chips) = crate::ui::settings::followup_draft(&followup);
            let mut content = InputContent::new(text);
            for (range, id, chip) in chips {
                let token = InlineToken::new(id, chip);
                if let Ok(next) = content.clone().with_token(range, token) {
                    content = next;
                }
            }
            content
        } else {
            InputContent::new(String::new())
        };
        let end = next.text().len();
        self.input.update(cx, |state, cx| {
            state.set_value(next, window, cx);
            state.set_selected_range(end..end, cx);
        });
        self.restyle(cx);
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
            && !self.edit_status.locked
            // An empty edit asks to delete the message instead.
            && (self.has_text(cx) || self.editing.is_some() || !self.attachments.is_empty())
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let state = self.input.read(cx);
        let content = state.value().trim().to_string();
        let _ = window;
        let chosen = self.recipients(cx);
        let mentioned: Vec<String> = chosen.iter().map(|(key, _)| key.clone()).collect();
        // Agents among the recipients, for the next draft.
        let profiles = self.community.read(cx).snapshot.profiles.clone();
        self.followup = chosen
            .into_iter()
            .filter(|(key, _)| profiles.get(key).is_some_and(|profile| profile.is_agent))
            .map(|(key, name)| (key, format!("@{name}")))
            .collect();
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
        let mut submission = Submission {
            content,
            mentions: Mentions {
                recipients: mentioned,
                references: Vec::new(),
            },
            attachments,
            editing: self.editing.clone(),
        };
        if self.editing.is_none() && !submission.mentions.recipients.is_empty() {
            let community = self.community.read(cx);
            let channel = community
                .snapshot
                .channels
                .iter()
                .find(|c| c.id == self.channel);
            let viewer = community.session.viewer();
            match admission(channel, viewer, submission.mentions.recipients.clone()) {
                Admission::Send(mentions) => submission.mentions = mentions,
                Admission::Ask { missing, can_add } => {
                    let draft = self.input.read(cx).value().to_string();
                    self.outsiders = Some(Outsiders {
                        submission,
                        missing,
                        can_add,
                        busy: false,
                        error: None,
                        draft,
                    });
                    self.outsiders_focus.focus(window, cx);
                    cx.notify();
                    return;
                }
            }
        }
        cx.emit(ComposerEvent::Submit(submission));
        cx.notify();
    }

    /// Pubkeys and names of the mention chips, first use first: the
    /// explicit recipients (`MentionDraft.recipients`).
    fn recipients(&self, cx: &App) -> Vec<(String, SharedString)> {
        let mut recipients: Vec<(String, SharedString)> = Vec::new();
        for span in self.input.read(cx).tokens() {
            let token = span.token();
            let id = token.id().to_string();
            if let Some(name) = token.text().strip_prefix('@')
                && !recipients.iter().any(|(key, _)| *key == id)
            {
                recipients.push((id, SharedString::from(name.to_string())));
            }
        }
        recipients
    }

    /// Turns every chip for `pubkey` back into plain `@Name` text, so the
    /// message names them without notifying them (`removeRecipient`).
    fn remove_recipient(&mut self, pubkey: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        self.input.update(cx, |state, cx| {
            let content = state.content();
            let selection = state.selected_range();
            let mut next = InputContent::new(content.text().clone());
            for span in content.tokens() {
                let token = span.token();
                if token.id() == pubkey && token.text().starts_with('@') {
                    continue;
                }
                if let Ok(with) = next.clone().with_token(span.range().clone(), token.clone()) {
                    next = with;
                }
            }
            state.set_value(next, window, cx);
            state.set_selected_range(selection, cx);
            state.focus(window, cx);
        });
        self.restyle(cx);
        // The draft keeps chips, so it must forget this one too.
        self.schedule_draft(cx);
        cx.notify();
    }

    // ----- mentions outside the channel -----

    /// Close or Escape: nothing is sent and the draft stays.
    fn close_outsiders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.outsiders.as_ref().is_none_or(|prompt| prompt.busy) {
            return;
        }
        self.outsiders = None;
        self.sending = false;
        self.focus(window, cx);
        cx.notify();
    }

    /// "Do nothing" / "Send anyway": outsiders become display-only
    /// references and are not added.
    fn send_without_outsiders(&mut self, cx: &mut Context<Self>) {
        if self.outsiders.as_ref().is_none_or(|prompt| prompt.busy) {
            return;
        }
        let Some(prompt) = self.outsiders.take() else {
            return;
        };
        let mut submission = prompt.submission;
        submission
            .mentions
            .recipients
            .retain(|key| !prompt.missing.contains(key));
        submission.mentions.references = prompt.missing;
        cx.emit(ComposerEvent::Submit(submission));
        cx.notify();
    }

    /// "Invite": adds each outsider (kind 9000) and waits for the roster,
    /// then sends with everyone notified. A failure keeps the dialog open.
    fn invite_outsiders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.outsiders.as_mut().filter(|p| p.can_add && !p.busy) else {
            return;
        };
        prompt.busy = true;
        prompt.error = None;
        let missing = prompt.missing.clone();
        let session = self.community.read(cx).session.clone();
        let channel = self.channel.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut failure = None;
            for key in &missing {
                let changed = this
                    .update(cx, |this, cx| {
                        this.outsiders.as_ref().is_none_or(|prompt| {
                            this.input.read(cx).value().as_ref() != prompt.draft.as_str()
                        })
                    })
                    .unwrap_or(true);
                if changed {
                    failure = Some("The draft changed. Close this dialog and send again.".into());
                    break;
                }
                if let Err(error) = session.add_member(&channel, key).await {
                    failure = Some(error);
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                match failure {
                    Some(error) => {
                        if let Some(prompt) = &mut this.outsiders {
                            prompt.busy = false;
                            prompt.error = Some(
                                format!(
                                    "{error} Your draft is kept. Some people may already have \
                                     been added."
                                )
                                .into(),
                            );
                        }
                    }
                    None => {
                        if let Some(prompt) = this.outsiders.take() {
                            cx.emit(ComposerEvent::Submit(prompt.submission));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.restyle(cx);
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

    /// Styles the Markdown in the text. `set_value` reports no change, so
    /// the places that call it restyle here too.
    fn restyle(&mut self, cx: &mut Context<Self>) {
        self.markdown.restyle(&self.input, cx);
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
            self.restyle(cx);
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
            // While editing, a pick is an identity link, never a recipient.
            Suggestion::Person { pubkey, name, .. } if self.editing.is_some() => {
                message_edit::mention_text(pubkey, name).map(|link| {
                    InlineToken::new(pubkey.clone(), link).with_label(format!("@{name}"))
                })
            }
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
            voice: None,
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
                        Ok(mut attachment) => {
                            item.bytes = None;
                            attachment.voice_duration = item.voice.as_ref().map(|v| v.duration);
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

    // ----- voice notes -----

    /// "Record voice note": starts the microphone at once
    /// (`useVoiceCapture`), unless a note is already attached or the
    /// message is full.
    fn start_capture(&mut self, cx: &mut Context<Self>) {
        if self.editing.is_some()
            || self.capture.is_some()
            || self.attachments.iter().any(|item| item.voice.is_some())
        {
            return;
        }
        if self.attachments.len() >= MAX_FILES {
            self.notice = Some(format!("Attach at most {MAX_FILES} files per message.").into());
            cx.notify();
            return;
        }
        self.capture = Some(match Recorder::start() {
            Ok(recorder) => Capture::Recording(recorder),
            Err(error) => Capture::Failed(error.into()),
        });
        self._capture_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let going = this.update(cx, |this, cx| {
                    let Some(Capture::Recording(recorder)) = &this.capture else {
                        return false;
                    };
                    if let Some(error) = recorder.failure() {
                        this.capture = Some(Capture::Failed(error.into()));
                    } else if recorder.seconds() >= recorder::MAX_SECONDS {
                        // The five-minute cap finishes the note.
                        this.finish_capture(cx);
                    }
                    cx.notify();
                    true
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// "Discard voice note" (or "Close" after a failure).
    fn discard_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.capture, Some(Capture::Preparing)) {
            return;
        }
        self.capture = None;
        self._capture_task = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// "Finish voice note": stops, encodes, and attaches the note.
    fn finish_capture(&mut self, cx: &mut Context<Self>) {
        let Some(Capture::Recording(recorder)) = self.capture.take() else {
            return;
        };
        if !recorder.live() {
            self.capture = Some(Capture::Recording(recorder));
            return;
        }
        self.capture = Some(Capture::Preparing);
        let prepared = cx.background_executor().spawn(async move {
            let samples = recorder.finish();
            let duration = recorder::duration(&samples).min(recorder::MAX_SECONDS);
            let waveform = recorder::waveform(&samples, recorder::WAVEFORM_BUCKETS);
            recorder::encode_mp4(&samples).map(|mp4| (mp4, duration, waveform))
        });
        self._capture_task = Some(cx.spawn(async move |this, cx| {
            let prepared = prepared.await;
            let _ = this.update(cx, |this, cx| {
                match prepared {
                    Ok((mp4, duration, waveform)) if duration > 0. => {
                        this.capture = None;
                        this.add_voice(mp4, duration, waveform, cx);
                    }
                    _ => {
                        this.capture = Some(Capture::Failed(
                            "Could not attach this voice note. Close and try again.".into(),
                        ))
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn add_voice(
        &mut self,
        mp4: Vec<u8>,
        duration: f64,
        waveform: Vec<u8>,
        cx: &mut Context<Self>,
    ) {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        self.next_attachment += 1;
        let id = self.next_attachment;
        let key = format!("voice-draft:{}:{id}", cx.entity_id());
        let bytes: Arc<[u8]> = mp4.clone().into();
        media::set_local(&key, bytes, cx);
        self.attachments.push(Pending {
            id,
            name: format!("voice-note-{millis}.mp4"),
            mime: "video/mp4".into(),
            size: mp4.len() as u64,
            thumbnail: None,
            bytes: Some(mp4),
            status: Status::Queued,
            voice: Some(VoiceDraft {
                key,
                duration,
                waveform,
            }),
        });
        self.pump(cx);
    }

    fn remove_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(voice) = self
            .attachments
            .iter()
            .find(|item| item.id == id)
            .and_then(|item| item.voice.as_ref())
        {
            media::forget_local(&voice.key, cx);
        }
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

/// The text to edit, with its identity links and channel references back
/// as chips.
fn edit_content(text: &str) -> InputContent {
    const LINK: &str = "](buzz://channel/";
    let mut content = InputContent::new(text.to_string());
    // `[\@Name](nostr:npub1…)`, as `profileMentionLink` writes them.
    let mut from = 0;
    while let Some(open) = text[from..].find("[\\@").map(|i| from + i) {
        from = open + 3;
        let Some(link) = text[open..].find("](nostr:npub1").map(|i| open + i) else {
            break;
        };
        let Some(close) = text[link..].find(')').map(|i| link + i) else {
            break;
        };
        let mut name = String::new();
        let mut escaped = false;
        for c in text[open + 1..link].chars() {
            if c == '\\' && !escaped {
                escaped = true;
            } else {
                name.push(c);
                escaped = false;
            }
        }
        let Some(key) = buzz_relay_client::profile_key(&text[link + 2..close]) else {
            continue;
        };
        let chip = InlineToken::new(key.to_hex(), text[open..=close].to_string()).with_label(name);
        if let Ok(next) = content.clone().with_token(open..close + 1, chip) {
            content = next;
        }
        from = close + 1;
    }
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
    voice: Option<VoiceDraft>,
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
                    .disabled(self.edit_status.busy)
                    .tooltip(if self.edit_status.locked {
                        "Close edit"
                    } else {
                        "Cancel edit"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))),
            )
    }

    /// "Your edit is still in the outbox." with "Retry edit".
    fn edit_retry(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.editing.is_none() || !self.edit_status.retryable {
            return None;
        }
        let c = *cx.colors();
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .text_caption()
                .text_color(c.text_subtle)
                .child("Your edit is still in the outbox.")
                .child(
                    Button::new("retry-edit", "Retry edit")
                        .size(ButtonSize::Sm)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::RetryEdit))),
                )
                .into_any_element(),
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
            if let Some(voice) = &item.voice {
                return self.voice_item(item, voice, failed, cx);
            }
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
            .child(self.leading_tools(cx))
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
            .when(!editing, |d| {
                // A plain 32px button: no hover fill, an 18px glyph.
                d.child(
                    div()
                        .id("record-voice-note")
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(32.))
                        .rounded(px(10.))
                        .text_color(c.text_subtle)
                        .cursor_pointer()
                        .tooltip(Tooltip::text("Record voice note"))
                        .child(Icon::new(IconName::Microphone).size(18.))
                        .on_click(cx.listener(|this, _, _, cx| this.start_capture(cx))),
                )
            })
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

    /// `.composerLeadingTools`: the mention button, and once chips name
    /// recipients, a pill of their avatars that remove them on click.
    fn leading_tools(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let recipients = self.recipients(cx);
        let community = self.community.read(cx);
        let media = community.session.media().clone();
        let disabled = self.sending;
        let buttons = recipients
            .iter()
            .enumerate()
            .map(|(index, (pubkey, name))| {
                let profile = community.snapshot.profiles.get(pubkey);
                let agent = profile.is_some_and(|p| p.is_agent);
                let mut avatar = Avatar::new(name.clone()).size(AvatarSize::Small);
                if agent {
                    avatar = avatar.squircle();
                }
                if let Some(picture) = profile.and_then(|p| p.picture.as_deref()) {
                    avatar = avatar.image(media::avatar(picture, &media));
                }
                let group = SharedString::from(format!("mention-recipient-{index}"));
                let key = pubkey.clone();
                div()
                    .id(("mention-recipient", index))
                    .group(group.clone())
                    .absolute()
                    .top_0()
                    .left(px(index as f32 * 22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .border_2()
                    .border_color(c.affordance_subtle)
                    .bg(c.affordance_subtle)
                    .map(|d| {
                        if agent {
                            d.rounded(px(9.))
                        } else {
                            d.rounded_full()
                        }
                    })
                    .overflow_hidden()
                    .tooltip(Tooltip::text(format!(
                        "Remove explicit mention of {name} ({})",
                        &pubkey[..8.min(pubkey.len())]
                    )))
                    .child(avatar)
                    .when(!disabled, |d| {
                        d.cursor_pointer()
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .map(|d| {
                                        if agent {
                                            d.rounded(px(7.))
                                        } else {
                                            d.rounded_full()
                                        }
                                    })
                                    .bg(c.affordance_prominent)
                                    .text_color(c.text_inverse)
                                    .opacity(0.)
                                    .group_hover(group, |s| s.opacity(1.))
                                    .child(Icon::new(IconName::X).size(16.)),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_recipient(&key, window, cx)
                            }))
                    })
            });
        let has_recipients = !recipients.is_empty();
        let count = recipients.len();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .rounded(px(12.))
            .when(has_recipients, |d| {
                d.rounded_full().px(px(2.)).bg(c.affordance_subtle)
            })
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
            .when(has_recipients, |d| {
                d.child(
                    // 28px buttons overlapping by 6px, then 6px of pill.
                    div()
                        .id("mention-recipients")
                        .relative()
                        .flex_none()
                        .w(px(count as f32 * 22. + 12.))
                        .h(px(28.))
                        .children(buttons),
                )
            })
    }

    /// A recorded note waiting to send: the voice card, 21rem wide, with
    /// its remove button on the corner (`ComposerAttachments.tsx:128-170`).
    fn voice_item(
        &self,
        item: &Pending,
        voice: &VoiceDraft,
        failed: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let id = item.id;
        let media = self.community.read(cx).session.media().clone();
        let attachment = buzz_relay_client::fold::Attachment {
            url: voice.key.clone(),
            kind: buzz_relay_client::fold::AttachmentKind::Audio,
            mime: Some(item.mime.clone()),
            size: Some(item.size),
            name: Some(item.name.clone()),
            duration: Some(voice.duration),
            waveform: Some(voice.waveform.clone()),
            voice_note: true,
            dimensions: None,
            blurhash: None,
            preview_url: None,
            insecure: false,
        };
        let busy = matches!(item.status, Status::Uploading);
        div()
            .id(("voice-draft", id))
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .w(px(336.))
            .max_w_full()
            .child(media::voice_draft(
                ("voice-draft-card", id),
                &attachment,
                &media,
                cx,
            ))
            .when_some(failed, |d, error| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_caption()
                        .text_color(c.text_danger)
                        .child(format!("Upload failed · {error}"))
                        .child(
                            IconButton::new(("retry-voice", id), IconName::ArrowsClockwise)
                                .size(IconButtonSize::Xs)
                                .tooltip("Retry upload")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.retry_attachment(id, cx)
                                    }),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .id(("remove-voice", id))
                    .absolute()
                    .top(px(-1.))
                    .right(px(-5.))
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(c.border_standard)
                    .bg(c.surface_popover)
                    .text_color(c.text_standard)
                    .cursor_pointer()
                    .tooltip(Tooltip::text("Remove voice note"))
                    .map(|d| {
                        if busy {
                            d.child(Spinner::new().size(12.))
                        } else {
                            d.child(Icon::new(IconName::X).size(16.))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(id, cx))),
            )
            .into_any_element()
    }

    /// The recorder in place of the tools (`VoiceCapture.tsx`): discard,
    /// a divider, the timer, the live waveform and the finish button.
    fn capture_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let capture = self.capture.as_ref()?;
        let c = *cx.colors();
        let discard = IconButton::new("discard-voice-note", IconName::X)
            .size(IconButtonSize::Sm)
            .icon_size(18.)
            .tooltip("Discard voice note")
            .disabled(matches!(capture, Capture::Preparing))
            .on_click(cx.listener(|this, _, window, cx| this.discard_capture(window, cx)));
        let row = div()
            .id("voice-capture")
            .flex()
            .items_center()
            .gap(px(4.))
            .min_h(px(40.))
            .child(discard)
            .child(
                div()
                    .flex_none()
                    .w(px(1.))
                    .h(px(20.))
                    .mx(px(4.))
                    .bg(c.border_standard),
            );
        if let Capture::Failed(error) = capture {
            return Some(
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_caption()
                        .text_color(c.text_danger)
                        .child(error.clone()),
                )
                .child(
                    Button::new("close-voice-note", "Close").on_click(
                        cx.listener(|this, _, window, cx| this.discard_capture(window, cx)),
                    ),
                )
                .into_any_element(),
            );
        }
        let recorder = match capture {
            Capture::Recording(recorder) => Some(recorder),
            _ => None,
        };
        let live = recorder.is_some_and(Recorder::live);
        let timer = match recorder {
            Some(recorder) if live => div().flex().child(voice_time(recorder.seconds())).child(
                div()
                    .text_color(c.text_subtle)
                    .child(format!(" / {}", voice_time(recorder::MAX_SECONDS))),
            ),
            Some(_) => div().child("Waiting for microphone…"),
            None => div().child("Preparing voice note…"),
        };
        let levels = recorder.map(|r| r.levels(256)).unwrap_or_default();
        Some(
            row.child(
                timer
                    .flex_none()
                    .text_caption()
                    .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
                    .text_color(c.text_standard),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h(px(24.))
                    .mx(px(8.))
                    .items_center()
                    .justify_end()
                    .gap(px(2.))
                    .overflow_hidden()
                    .children(levels.into_iter().map(move |level| {
                        div()
                            .flex_none()
                            .w(px(3.))
                            .h(px(recorder::bar_height(level)))
                            .rounded_full()
                            .bg(c.text_standard)
                    })),
            )
            .child(
                IconButton::new("finish-voice-note", IconName::StopFilled)
                    .size(IconButtonSize::Sm)
                    .icon_size(16.)
                    .variant(ButtonVariant::Prominent)
                    .shape(IconButtonShape::Round)
                    .tooltip("Finish voice note")
                    .disabled(!live)
                    .on_click(cx.listener(|this, _, _, cx| this.finish_capture(cx))),
            )
            .into_any_element(),
        )
    }

    /// "Mention people outside this channel?" (`useNonmemberMentions.tsx`).
    fn outsiders_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.outsiders.as_ref()?;
        let c = *cx.colors();
        let profiles = &self.community.read(cx).snapshot.profiles;
        let names = prompt
            .missing
            .iter()
            .map(|key| message::display_name(profiles, key).to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let verb = if prompt.missing.len() == 1 {
            "is"
        } else {
            "are"
        };
        let next = if prompt.can_add {
            "Invite them to the channel, or send without inviting them."
        } else {
            "You cannot add people to this channel. You can still send without inviting them."
        };
        let busy = prompt.busy;
        let dialog = Dialog::new("outsider-mentions", "Mention people outside this channel?")
            .description(format!("{names} {verb} not in this channel. {next}"))
            .on_close(cx.listener(|this, _, window, cx| this.close_outsiders(window, cx)))
            .children(
                prompt
                    .error
                    .clone()
                    .map(|error| div().text_body_sm().text_color(c.text_danger).child(error)),
            )
            .footer(
                Button::new(
                    "outsiders-skip",
                    if prompt.can_add {
                        "Do nothing"
                    } else {
                        "Send anyway"
                    },
                )
                .surface(Surface::Floating)
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.send_without_outsiders(cx))),
            )
            .when(prompt.can_add, |d| {
                d.footer(
                    Button::new(
                        "outsiders-invite",
                        if busy { "Inviting…" } else { "Invite" },
                    )
                    .prominent()
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.invite_outsiders(window, cx))),
                )
            });
        let content = div()
            .track_focus(&self.outsiders_focus)
            .key_context("OutsiderMentions")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.close_outsiders(window, cx),
                    // The default button, "Do nothing", has the focus.
                    "enter" => this.send_without_outsiders(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(dialog);
        Some(overlay::modal(window, content, None).into_any_element())
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
                            "Not in channel · Choose whether to add when you send"
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        self.markdown.follow_theme(&self.input, &c, cx);
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
            .children(self.edit_retry(cx))
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
            .children(self.capture_row(cx))
            .when(self.capture.is_none(), |d| {
                d.child(
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
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                ),
                        ),
                )
            });

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
            .children(self.outsiders_dialog(window, cx))
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

    fn channel(channel_type: ChannelType, members: &[&str]) -> ChannelSummary {
        ChannelSummary {
            id: "c".into(),
            channel_type: Some(channel_type),
            members: members.iter().map(|m| m.to_string()).collect(),
            ..Default::default()
        }
    }

    fn keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn asks_before_mentioning_outsiders_in_streams_and_forums() {
        let stream = channel(ChannelType::Stream, &["me", "bob"]);
        assert_eq!(
            admission(Some(&stream), "me", keys(&["bob"])),
            Admission::Send(Mentions {
                recipients: keys(&["bob"]),
                references: Vec::new(),
            })
        );
        assert_eq!(
            admission(Some(&stream), "me", keys(&["bob", "carol"])),
            Admission::Ask {
                missing: keys(&["carol"]),
                can_add: true,
            }
        );
        // Not a member: only "Send anyway" is offered.
        let forum = channel(ChannelType::Forum, &["bob"]);
        assert_eq!(
            admission(Some(&forum), "me", keys(&["carol"])),
            Admission::Ask {
                missing: keys(&["carol"]),
                can_add: false,
            }
        );
        let archived = ChannelSummary {
            archived: true,
            ..channel(ChannelType::Stream, &["me"])
        };
        assert_eq!(
            admission(Some(&archived), "me", keys(&["carol"])),
            Admission::Ask {
                missing: keys(&["carol"]),
                can_add: false,
            }
        );
    }

    #[test]
    fn references_outsiders_in_dms_without_asking() {
        let dm = channel(ChannelType::Dm, &["me", "bob"]);
        assert_eq!(
            admission(Some(&dm), "me", keys(&["carol", "bob"])),
            Admission::Send(Mentions {
                recipients: keys(&["bob"]),
                references: keys(&["carol"]),
            })
        );
        // An unknown roster leaves the decision to the writer's check.
        assert_eq!(
            admission(None, "me", keys(&["carol"])),
            Admission::Send(Mentions {
                recipients: keys(&["carol"]),
                references: Vec::new(),
            })
        );
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
