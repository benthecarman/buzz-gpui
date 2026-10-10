//! The message composer shared by the channel page and the thread panel
//! (ui.md §4.8-4.9, `MessageComposer.tsx`, `ComposerCompletions.tsx`).
//!
//! It owns the text, mention chips, `@` `#` `:` completion, the emoji
//! picker, the formatting toolbar, attachments and the saved draft. The
//! page that hosts it decides what a submission means (a message, a
//! reply or an edit) and reports back with [`Composer::sent`] or
//! [`Composer::failed`].

pub mod attachments;
mod channels;
mod format;
mod live_markdown;

use super::emoji_picker::{self, Candidate, Dismissed, EmojiPicker, Picked};
use super::gif_picker::PickedGif;
use super::media;
use super::media::recorder::{self, Recorder};
use super::message::{self, channel_icon};
use super::message_edit;
use super::overlay;
use crate::community::{AppState, Community};
use attachments::{AttachmentDraft, BackgroundSend, DraftFile, MAX_FILES, NewFile, Queue, Voice};
use buzz_relay_client::fold::MessageRow;
use buzz_relay_client::members::can_add_members;
use buzz_relay_client::upload::Source;
use buzz_relay_client::{
    Attachment, ChannelSummary, ChannelType, EventId, Mentions, Partition, PublicChannels,
    RosterStatus,
};
use buzz_ui::{
    ActiveTheme, Avatar, AvatarSize, Button, ButtonSize, ButtonVariant, Dialog, Icon, IconButton,
    IconButtonShape, IconButtonSize, IconName, Spinner, Surface, Toast, ToastTone, Tooltip,
    TypeStyled,
};
use format::Format;
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
/// Drafts are saved this long after the last keystroke.
const DRAFT_DELAY: Duration = Duration::from_millis(400);
/// Completion lists.
const MENTION_LIMIT: usize = 50;
/// The typing pause before `#` searches open channels.
const PUBLIC_SEARCH_DELAY: Duration = Duration::from_millis(180);
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
    /// The files, which upload in the background after Send
    /// (`sendInBackground`); the host publishes once they are up.
    pub upload: Option<BackgroundSend>,
    /// The message being edited, if any.
    pub editing: Option<Arc<MessageRow>>,
}

impl Submission {
    /// The files to publish: none, or those the background send uploaded.
    /// `None` when that send failed or was cancelled; its text and files
    /// are back in the draft then, so publish nothing.
    pub async fn attachments(&self) -> Option<Vec<Attachment>> {
        match &self.upload {
            Some(upload) => upload.uploaded().await,
            None => Some(Vec::new()),
        }
    }

    /// Reports how publishing went. A failed background send hands its
    /// text and files back with a notice; returns whether it did, in which
    /// case the host shows nothing more.
    pub fn settle(&self, result: &Result<(), String>, cx: &mut App) -> bool {
        match &self.upload {
            Some(upload) => {
                upload.settle(result, cx);
                true
            }
            None => false,
        }
    }
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
    public: Option<PublicSearch>,
    /// Bumped by "Retry suggestions" to search again.
    public_attempt: u64,
    /// The last open channels found, kept while the next search runs.
    recent_public: Vec<ChannelSummary>,
    /// The query start Escape dismissed, until the text moves on.
    dismissed: Option<usize>,
    /// The text length and caret the completion was computed for.
    completion_key: Option<(usize, usize)>,
    completion_scroll: ScrollHandle,
    picker: Entity<EmojiPicker>,
    picker_open: bool,
    picker_closed: Option<std::time::Instant>,
    /// The draft's files, which outlive this composer.
    files: Entity<AttachmentDraft>,
    /// The community's background sends.
    queue: Entity<Queue>,
    /// "Could not attach file", as a toast.
    attach_error: Option<SharedString>,
    /// The draft file whose preview dialog is open.
    previewing: Option<String>,
    /// Why the last send failed (`error`), under the tools.
    error: Option<SharedString>,
    /// Formats toggled at a collapsed caret, outermost first: their empty
    /// delimiters wait, hidden, around `pending_at` for text.
    pending: Vec<PendingFormat>,
    pending_at: usize,
    /// A formatting edit is being applied, so its changes are not typing.
    applying_format: bool,
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

/// A format toggled with a collapsed caret, waiting for text (ProseMirror's
/// stored marks). `removes` is a split of a span the caret was in: its
/// text will have the format off.
struct PendingFormat {
    marker: &'static str,
    kind: live_markdown::Kinds,
    removes: bool,
}

/// Voice note capture (`VoiceCapture.tsx`).
enum Capture {
    Recording(Recorder),
    Preparing,
    Failed(SharedString),
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
        let key = draft_key(&channel, thread_root);
        let files = attachments::draft(&community, &key, cx);
        attachments::view(&files, cx);
        let queue = attachments::queue(&community, cx);
        let host = cx.entity_id();
        queue.update(cx, |queue, cx| queue.add_host(host, cx));
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.observe_in(&files, window, |this, _, window, cx| {
            this.take_recovered(window, cx);
            cx.notify();
        }));
        subscriptions.push(cx.observe(&queue, |_, _, cx| cx.notify()));
        cx.on_release(move |this: &mut Self, cx| {
            this.write_draft(cx);
            let key = this.draft_key();
            attachments::leave(&this.community, &key, &this.files, cx);
            this.queue
                .update(cx, |queue, cx| queue.remove_host(host, cx));
        })
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
            public: None,
            public_attempt: 0,
            recent_public: Vec::new(),
            dismissed: None,
            completion_key: None,
            completion_scroll: ScrollHandle::new(),
            picker,
            picker_open: false,
            picker_closed: None,
            files,
            queue,
            attach_error: None,
            previewing: None,
            error: None,
            pending: Vec::new(),
            pending_at: 0,
            applying_format: false,
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

    /// The host delivered the submission: clear it. A background send
    /// cleared the composer at Send, so this does nothing then.
    pub fn sent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sending {
            return;
        }
        self.sending = false;
        self.notice = None;
        if self.editing.is_some() {
            self.cancel_edit(window, cx);
            return;
        }
        self.clear_after_send(window, cx);
    }

    /// Empties the composer after a send, leaving the follow-up draft of
    /// remembered agents.
    fn clear_after_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn has_files(&self, cx: &App) -> bool {
        !self.files.read(cx).items.is_empty()
    }

    /// A send of this conversation is still uploading.
    fn send_pending(&self, cx: &App) -> bool {
        self.queue.read(cx).pending(&self.draft_key())
    }

    fn can_submit(&self, cx: &App) -> bool {
        let files = self.files.read(cx);
        !self.sending
            && self.can_send
            && (self.editing.is_some() || (!files.blocked() && !self.send_pending(cx)))
            && self.input.read(cx).value().chars().count() <= MAX_CHARS
            && !self.edit_status.locked
            // An empty edit asks to delete the message instead.
            && (self.has_text(cx) || self.editing.is_some() || !files.items.is_empty())
    }

    /// Hands a submission to the host. With files, Send clears the composer
    /// at once and they upload in the background before the host publishes.
    fn dispatch(
        &mut self,
        mut submission: Submission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        let files = self.files.read(cx).items.clone();
        if submission.editing.is_none() && !files.is_empty() {
            let draft = self.draft_json(cx);
            self.files.update(cx, |draft, cx| draft.clear(cx));
            self.clear_after_send(window, cx);
            let followup = self.draft_json(cx);
            let owner = self.draft_key();
            submission.upload = self.queue.update(cx, |queue, cx| {
                queue.start(owner, files, draft, followup, cx)
            });
            self.sending = false;
        } else {
            self.sending = true;
        }
        cx.emit(ComposerEvent::Submit(submission));
        cx.notify();
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
        self.sending = true;
        self.completion = None;
        let mut submission = Submission {
            content,
            mentions: Mentions {
                recipients: mentioned,
                references: Vec::new(),
            },
            upload: None,
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
        self.dispatch(submission, window, cx);
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
    fn send_without_outsiders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.dispatch(submission, window, cx);
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
            let _ = this.update_in(cx, |this, window, cx| {
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
                            this.dispatch(prompt.submission, window, cx);
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
        if !self.applying_format {
            self.settle_pending(window, cx);
        }
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
        let pending = self.pending_ranges();
        self.markdown.restyle(&self.input, &pending, cx);
    }

    // ----- pending formats -----

    /// The pending delimiters around the caret: openings outermost first,
    /// then closings innermost first.
    fn pending_text(&self) -> (String, String) {
        let open: String = self.pending.iter().map(|p| p.marker).collect();
        let close: String = self.pending.iter().rev().map(|p| p.marker).collect();
        (open, close)
    }

    /// Each pending delimiter's range, to keep hidden.
    fn pending_ranges(&self) -> Vec<std::ops::Range<usize>> {
        let mut ranges = Vec::new();
        let (open, _) = self.pending_text();
        let mut at = self.pending_at.saturating_sub(open.len());
        for pending in &self.pending {
            ranges.push(at..at + pending.marker.len());
            at += pending.marker.len();
        }
        for pending in self.pending.iter().rev() {
            ranges.push(at..at + pending.marker.len());
            at += pending.marker.len();
        }
        ranges
    }

    /// Whether the pending delimiters still sit, empty, around `caret`.
    fn pending_holds(&self, text: &str, caret: usize) -> bool {
        let (open, close) = self.pending_text();
        caret >= open.len()
            && text.get(caret - open.len()..caret) == Some(open.as_str())
            && text.get(caret..caret + close.len()) == Some(close.as_str())
    }

    /// After an edit: text typed at the caret fills the pending formats,
    /// which then style it; a deletion that moved the caret off them drops
    /// them, as ProseMirror drops stored marks.
    fn settle_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_empty() {
            return;
        }
        let (text, caret) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.selected_range().end)
        };
        if self.pending_holds(&text, caret) {
            self.pending_at = caret;
            return;
        }
        let (open, close) = self.pending_text();
        let both = format!("{open}{close}");
        if text.get(caret..caret + both.len()) == Some(both.as_str()) {
            let range = caret..caret + both.len();
            self.pending.clear();
            self.applying_format = true;
            self.input.update(cx, |state, cx| {
                state.set_selected_range(range, cx);
                state.replace("", window, cx);
            });
            self.applying_format = false;
        }
        self.pending.clear();
    }

    /// The caret left the pending formats without typing: they go away.
    fn leave_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, selection) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        if self.pending.is_empty() || (selection.is_empty() && selection.end == self.pending_at) {
            return;
        }
        let at = self.pending_at;
        let holds = self.pending_holds(&text, at);
        let (open, close) = self.pending_text();
        self.pending.clear();
        if holds {
            let range = at - open.len()..at + close.len();
            let removed = range.len();
            let map = |offset: usize| {
                if offset >= range.end {
                    offset - removed
                } else {
                    offset.min(range.start)
                }
            };
            let next = map(selection.start)..map(selection.end);
            self.applying_format = true;
            self.input.update(cx, |state, cx| {
                state.set_selected_range(range, cx);
                state.replace("", window, cx);
                state.set_selected_range(next, cx);
            });
            self.applying_format = false;
        }
        self.restyle(cx);
    }

    /// The formats pressed in the toolbar: those at the caret, with the
    /// pending ones on and the ones a pending split turns off.
    fn active_formats(&self, cx: &App) -> live_markdown::Active {
        let state = self.input.read(cx);
        let (add, remove) = self.pending.iter().fold((0, 0), |(add, remove), p| {
            if p.removes {
                (add, remove | p.kind)
            } else {
                (add | p.kind, remove)
            }
        });
        let mut active = self
            .markdown
            .active(state.selected_range(), state.value().as_ref(), add);
        if remove & live_markdown::STRONG != 0 {
            active.bold = false;
        }
        if remove & live_markdown::EMPHASIS != 0 {
            active.italic = false;
        }
        if remove & live_markdown::STRIKE != 0 {
            active.strike = false;
        }
        if remove & live_markdown::CODE != 0 {
            active.code = false;
        }
        if remove & live_markdown::SPOILER != 0 {
            active.spoiler = false;
        }
        active
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
        draft_key(&self.channel, self.thread_root)
    }

    /// The draft as saved, `{text, tokens}` JSON.
    fn draft_json(&self, cx: &App) -> String {
        let content = self.input.read(cx).content();
        serde_json::to_string(&Draft::from(&content)).expect("drafts serialize")
    }

    /// A failed send handed its text back: show it, unless the composer
    /// holds something else by now (its saved draft was checked already).
    fn take_recovered(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recovered) = self.files.update(cx, |files, _| files.recovered.take()) else {
            return;
        };
        self.error = recovered.error.clone();
        if self.editing.is_some() {
            self.stash = serde_json::from_str::<Draft>(&recovered.draft)
                .ok()
                .map(|draft| draft.content());
        } else if let Ok(draft) = serde_json::from_str::<Draft>(&recovered.draft) {
            let content = draft.content();
            let end = content.text().len();
            self.input.update(cx, |state, cx| {
                state.set_value(content, window, cx);
                state.set_selected_range(end..end, cx);
            });
            self.restyle(cx);
        }
        cx.notify();
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
        let (items, status, retry) = if query.trigger == Trigger::Channel {
            self.search_public(&query.text, cx);
            self.channel_suggestions(&query, cx)
        } else {
            (self.suggestions(&query, cx), None, None)
        };
        let count = items.len() + usize::from(retry.is_some());
        if count == 0 && status.is_none() {
            self.completion = None;
            return;
        }
        let selected = match &self.completion {
            Some(old) if old.query.start == query.start && old.query.trigger == query.trigger => {
                old.selected.min(count.saturating_sub(1))
            }
            _ => 0,
        };
        self.completion = Some(Completion {
            query,
            items,
            selected,
            status,
            retry,
        });
    }

    /// Starts the open-channel lookup for `#query` after a typing pause,
    /// unless it already runs (`usePublicChannelSearch`).
    fn search_public(&mut self, query: &str, cx: &mut Context<Self>) {
        let needle = query.to_lowercase();
        let community = self.community.read(cx);
        if needle.trim().is_empty() || community.snapshot.roster != RosterStatus::Ready {
            self.public = None;
            return;
        }
        if self
            .public
            .as_ref()
            .is_some_and(|search| search.needle == needle && search.attempt == self.public_attempt)
        {
            return;
        }
        let session = community.session.clone();
        let (attempt, wanted) = (self.public_attempt, needle.clone());
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PUBLIC_SEARCH_DELAY).await;
            let result = session.find_public_channels(&wanted, false).await;
            let _ = this.update(cx, |this, cx| {
                let Some(search) = &mut this.public else {
                    return;
                };
                if search.needle != wanted || search.attempt != attempt {
                    return;
                }
                if let Ok(found) = &result {
                    this.recent_public = found.channels.clone();
                }
                search.result = Some(result);
                // Recompute the popup with the answer.
                this.completion_key = None;
                cx.notify();
            });
        });
        self.public = Some(PublicSearch {
            needle,
            attempt,
            result: None,
            _task: task,
        });
    }

    /// `#` rows, status line and retry (`ChannelCompletion`).
    fn channel_suggestions(
        &self,
        query: &Query,
        cx: &App,
    ) -> (
        Vec<Suggestion>,
        Option<SharedString>,
        Option<channels::Retry>,
    ) {
        let community = self.community.read(cx);
        let snapshot = &community.snapshot;
        let needle = query.text.to_lowercase();
        // While the next lookup runs, the last answer's rows that still
        // match stay, so open channels don't flicker on every keystroke.
        let kept: Vec<ChannelSummary>;
        let search = match &self.public {
            None => channels::Search::Idle,
            Some(PublicSearch {
                result: Some(Ok(found)),
                ..
            }) => channels::Search::Done(found),
            Some(PublicSearch {
                result: Some(Err(_)),
                ..
            }) => channels::Search::Failed,
            Some(PublicSearch { result: None, .. }) => {
                kept = self
                    .recent_public
                    .iter()
                    .filter(|c| c.name.to_lowercase().contains(needle.trim()))
                    .cloned()
                    .collect();
                channels::Search::Loading(&kept)
            }
        };
        let completion = channels::complete(
            &snapshot.channels,
            community.session.viewer(),
            &snapshot.roster,
            &query.text,
            search,
            name_match,
        );
        let items = completion
            .items
            .into_iter()
            .map(|item| Suggestion::Channel {
                icon: channel_icon(&item.channel),
                id: item.channel.id,
                name: item.channel.name.into(),
                member: item.member,
                detail: item.detail.map(SharedString::from),
            })
            .collect();
        (
            items,
            completion.status.map(SharedString::from),
            completion.retry,
        )
    }

    /// `canSelect`: a joined row must still be joined under that name, an
    /// open row still open and unjoined.
    fn can_select(&self, id: &str, name: &str, member: bool, cx: &App) -> bool {
        let community = self.community.read(cx);
        let listed = community.snapshot.channels.iter().find(|c| c.id == id);
        if member {
            return listed.is_some_and(|c| {
                c.name == name && channels::joined(c, community.session.viewer())
            });
        }
        let found = match &self.public {
            Some(PublicSearch {
                result: Some(Ok(found)),
                ..
            }) => &found.channels,
            _ => &self.recent_public,
        };
        listed.is_none_or(|c| c.read_only)
            && found
                .iter()
                .any(|c| c.id == id && c.name == name && channels::open(c))
    }

    fn retry_completion(&mut self, retry: channels::Retry, cx: &mut Context<Self>) {
        match retry {
            channels::Retry::Channels => self.community.read(cx).session.retry(),
            channels::Retry::Search => {
                self.public_attempt += 1;
                self.public = None;
            }
        }
        self.completion_key = None;
        cx.notify();
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
            // `channel_suggestions` owns `#`.
            Trigger::Channel => Vec::new(),
            Trigger::Emoji => {
                emoji_picker::search(&query.text, &snapshot.emoji.entries, EMOJI_LIMIT)
                    .into_iter()
                    .map(Suggestion::Emoji)
                    .collect()
            }
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(completion) = &mut self.completion
            && completion.count() > 0
        {
            let count = completion.count() as isize;
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
        let Some(completion) = &self.completion else {
            return;
        };
        if index == completion.items.len()
            && let Some(retry) = completion.retry
        {
            self.retry_completion(retry, cx);
            return;
        }
        if let Some(Suggestion::Channel {
            id, name, member, ..
        }) = completion.items.get(index)
            && !self.can_select(id, name, *member, cx)
        {
            return;
        }
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
        if self.sending || self.edit_status.locked {
            return;
        }
        let (text, selection) = {
            let state = self.input.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let Some(kind) = format.kind() else {
            self.leave_pending(window, cx);
            let (text, selection) = {
                let state = self.input.read(cx);
                (state.value().to_string(), state.selected_range())
            };
            let edit = format::apply(&text, selection, format);
            self.apply_edit(edit, window, cx);
            return;
        };
        // Code stays literal for every other format (`markdownRanges`).
        if kind != live_markdown::CODE
            && self.markdown.pairs(live_markdown::CODE).iter().any(|pair| {
                pair.open.start < selection.end.max(selection.start + 1)
                    && pair.close.end > selection.start
            })
        {
            return;
        }
        let active = self.active_formats(cx);
        let has = |active: &live_markdown::Active| match format {
            Format::Bold => active.bold,
            Format::Italic => active.italic,
            Format::Strike => active.strike,
            Format::Code => active.code,
            _ => active.spoiler,
        };
        if selection.is_empty() && format == Format::Spoiler && !text.trim().is_empty() {
            // A collapsed caret puts the whole draft behind a spoiler.
            self.leave_pending(window, cx);
            let state = self.input.read(cx);
            let (text, caret) = (state.value().to_string(), state.selected_range().end);
            let whole = 0..text.len();
            let all = self.markdown.active(whole.clone(), &text, 0).spoiler;
            let edit = format::toggle_range(&text, whole, format, &self.markdown.pairs(kind), all);
            let caret = format::map_offset(caret, &edit.changes);
            self.apply_edit(
                format::Edit {
                    changes: edit.changes,
                    selection: caret..caret,
                },
                window,
                cx,
            );
            return;
        }
        if selection.is_empty() {
            if let Some(index) = self.pending.iter().position(|p| p.kind == kind) {
                // Toggling a pending format again takes it back.
                let ranges = self.pending_ranges();
                let open = ranges[index].clone();
                let close = ranges[ranges.len() - 1 - index].clone();
                self.pending.remove(index);
                let caret = self.pending_at - open.len();
                self.pending_at = caret;
                self.apply_edit(
                    format::Edit {
                        changes: vec![(open, String::new()), (close, String::new())],
                        selection: caret..caret,
                    },
                    window,
                    cx,
                );
                return;
            }
            if !self.pending.is_empty() && !self.pending_holds(&text, selection.end) {
                self.pending.clear();
            }
            let inside = has(&self.markdown.active(selection.clone(), &text, 0));
            let (edit, marker) = format::pending_pair(&text, selection.end, format, inside);
            self.pending.push(PendingFormat {
                marker,
                kind,
                removes: inside,
            });
            self.pending_at = edit.selection.end;
            self.apply_edit(edit, window, cx);
            return;
        }
        self.leave_pending(window, cx);
        let edit = format::toggle_range(
            &text,
            selection,
            format,
            &self.markdown.pairs(kind),
            has(&active),
        );
        self.apply_edit(edit, window, cx);
    }

    /// Applies a formatting edit as one change to the field.
    fn apply_edit(&mut self, edit: format::Edit, window: &mut Window, cx: &mut Context<Self>) {
        self.applying_format = true;
        self.input.update(cx, |state, cx| {
            for (range, replacement) in edit.changes.iter().rev() {
                state.set_selected_range(range.clone(), cx);
                state.replace(replacement.clone(), window, cx);
            }
            state.set_selected_range(edit.selection, cx);
            state.focus(window, cx);
        });
        self.applying_format = false;
        self.restyle(cx);
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

    /// Files chosen, dropped or pasted (`attachFiles`). Refusals show the
    /// "Could not attach file" toast.
    fn add_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let read = cx
            .background_executor()
            .spawn(async move { attachments::files_from_paths(paths) });
        cx.spawn(async move |this, cx| {
            let files = read.await;
            let _ = this.update(cx, |this, cx| {
                let mut ok = Vec::new();
                for file in files {
                    match file {
                        Ok(file) => ok.push(file),
                        Err((_, error)) => {
                            this.attach_error = Some(error.into());
                            cx.notify();
                            return;
                        }
                    }
                }
                this.add_files(ok, cx);
            });
        })
        .detach();
    }

    /// Dropping files works outside edits and voice capture (`useFileDrop`).
    fn accepts_drop(&self) -> bool {
        self.editing.is_none() && self.capture.is_none() && !self.sending
    }

    fn add_files(&mut self, files: Vec<NewFile>, cx: &mut Context<Self>) {
        if files.is_empty() || self.capture.is_some() || self.sending {
            return;
        }
        if self.editing.is_some() {
            self.attach_error = Some("Finish editing before attaching new files.".into());
            cx.notify();
            return;
        }
        match attachments::add(&self.community, &self.files, files, cx) {
            Ok(()) => self.attach_error = None,
            Err(error) => self.attach_error = Some(error.into()),
        }
        cx.notify();
    }

    // ----- voice notes -----

    /// "Record voice note": starts the microphone at once
    /// (`useVoiceCapture`), unless a note is already attached or the
    /// message is full.
    fn start_capture(&mut self, cx: &mut Context<Self>) {
        let files = self.files.read(cx);
        if self.editing.is_some()
            || self.sending
            || self.capture.is_some()
            || files.items.iter().any(|item| item.voice.is_some())
        {
            return;
        }
        if files.items.len() >= MAX_FILES {
            self.attach_error =
                Some(format!("Attach at most {MAX_FILES} files per message.").into());
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
        let key = format!("voice-draft:{}", uuid::Uuid::new_v4());
        let bytes: Arc<[u8]> = mp4.into();
        media::set_local(&key, bytes.clone(), cx);
        let file = NewFile {
            name: format!("voice-note-{millis}.mp4"),
            mime: "video/mp4".into(),
            size: bytes.len() as u64,
            source: Source::Bytes(bytes),
            voice: Some(Voice {
                key,
                duration,
                waveform,
            }),
        };
        if let Err(error) = attachments::add(&self.community, &self.files, vec![file], cx) {
            self.attach_error = Some(error.into());
        }
        cx.notify();
    }

    fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        let voice = self
            .files
            .read(cx)
            .items
            .iter()
            .find(|item| item.id == id)
            .and_then(|item| item.voice.clone());
        if let Some(voice) = voice {
            media::forget_local(&voice.key, cx);
        }
        self.files.update(cx, |files, cx| files.remove(id, cx));
        cx.notify();
    }

    fn retry_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.files.update(cx, |files, cx| files.retry(id, cx));
        cx.notify();
    }

    /// Images and files from the clipboard. Text pastes as usual.
    fn paste(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let mut images = Vec::new();
        let mut paths = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let extension = match image.format {
                        ImageFormat::Png => "png",
                        ImageFormat::Jpeg => "jpg",
                        ImageFormat::Gif => "gif",
                        ImageFormat::Webp => "webp",
                        _ => "img",
                    };
                    images.push(attachments::file_from_bytes(
                        format!("image.{extension}"),
                        image.bytes.clone(),
                    ));
                }
                ClipboardEntry::ExternalPaths(external) => {
                    paths.extend(external.paths().iter().cloned());
                }
                ClipboardEntry::String(_) => {}
            }
        }
        let consumed = !images.is_empty() || !paths.is_empty();
        if !images.is_empty() {
            self.add_files(images, cx);
        }
        if !paths.is_empty() {
            self.add_paths(paths, cx);
        }
        consumed
    }
}

/// `draft:<channel>`, or `draft:<channel>:thread:<root>` for a thread.
fn draft_key(channel: &str, thread_root: Option<EventId>) -> String {
    match thread_root {
        Some(root) => format!("draft:{channel}:thread:{}", root.to_hex()),
        None => format!("draft:{channel}"),
    }
}

/// Whether a saved draft holds no text or chips.
pub(crate) fn draft_is_empty(json: &str) -> bool {
    serde_json::from_str::<Draft>(json).is_ok_and(|draft| draft.text.trim().is_empty())
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

/// The picture a draft file shows: an image's own pixels. Other files
/// show the file icon.
fn preview_source(item: &DraftFile) -> Option<Img> {
    let kind = item
        .uploaded
        .as_ref()
        .map_or(item.mime.as_str(), |a| a.mime.as_str());
    let lower = item.name.to_ascii_lowercase();
    let image = matches!(
        kind,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) || (item.uploaded.is_none()
        && [".png", ".jpg", ".jpeg", ".gif", ".webp"]
            .iter()
            .any(|ext| lower.ends_with(ext)));
    if !image {
        return None;
    }
    Some(match &item.source {
        Source::Path(path) => img(path.clone()).object_fit(ObjectFit::Cover),
        Source::Bytes(bytes) => {
            let format = match buzz_relay_client::upload::sniff_image(bytes)? {
                "image/png" => ImageFormat::Png,
                "image/jpeg" => ImageFormat::Jpeg,
                "image/gif" => ImageFormat::Gif,
                _ => ImageFormat::Webp,
            };
            img(Arc::new(Image::from_bytes(format, bytes.to_vec()))).object_fit(ObjectFit::Cover)
        }
    })
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
    /// An index into the items, then the retry row.
    selected: usize,
    /// The quiet line under the list.
    status: Option<SharedString>,
    /// "Retry suggestions", after the items.
    retry: Option<channels::Retry>,
}

impl Completion {
    /// Rows the keyboard moves through.
    fn count(&self) -> usize {
        self.items.len() + usize::from(self.retry.is_some())
    }
}

/// The open-channel lookup behind `#` completion.
struct PublicSearch {
    needle: String,
    attempt: u64,
    /// `None` while it runs.
    result: Option<Result<PublicChannels, String>>,
    _task: Task<()>,
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
        /// Joined, rather than an open channel to preview.
        member: bool,
        detail: Option<SharedString>,
    },
    Emoji(Candidate),
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

    /// The draft's files (`ComposerAttachments.tsx`): one row that scrolls
    /// sideways. Images show as 46px previews with their actions on the
    /// corner; other files, and failed ones, as 18rem chips.
    fn attachment_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let items = self.files.read(cx).items.clone();
        if items.is_empty() || self.editing.is_some() {
            return None;
        }
        let disabled = self.sending;
        let children: Vec<AnyElement> = items
            .iter()
            .map(|item| match &item.voice {
                Some(voice) => self.voice_item(item, voice, disabled, cx),
                None => self.file_item(item, disabled, cx),
            })
            .collect();
        Some(
            div()
                .id("composer-attachments")
                .flex()
                .flex_none()
                .gap(px(8.))
                .min_w_0()
                .overflow_x_scroll()
                .children(children)
                .into_any_element(),
        )
    }

    fn file_item(&self, item: &DraftFile, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let id = SharedString::from(item.id.clone());
        let size = attachments::file_size(item.uploaded.as_ref().map_or(item.size, |a| a.size));
        let status = item.status.label();
        let kind = item
            .uploaded
            .as_ref()
            .map_or(item.mime.as_str(), |a| a.mime.as_str())
            .to_string();
        let preview = preview_source(item);
        // A failed file shows its whole chip, preview and all.
        let media = preview.is_some() && item.status != attachments::Status::Error;
        let busy = item.status.busy();
        let group = SharedString::from(format!("remove-{}", item.id));
        let remove_id = id.clone();
        let remove = div()
            .id(SharedString::from(format!("remove-attachment-{id}")))
            .group(group.clone())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(10.))
            .bg(if disabled {
                c.affordance_disabled
            } else {
                c.affordance_prominent
            })
            .text_color(if disabled {
                c.text_unavailable
            } else {
                c.text_inverse
            })
            .when(!disabled, |d| d.cursor_pointer())
            .tooltip(Tooltip::text("Remove attachment"))
            .child(
                div()
                    .when(busy, |d| {
                        d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                    })
                    .child(Icon::new(IconName::X).size(12.)),
            )
            .when(busy, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .group_hover(group, |s| s.opacity(0.))
                        .child(Spinner::new().size(12.).color(c.text_inverse)),
                )
            })
            .when(!disabled, |d| {
                d.on_click(
                    cx.listener(move |this, _, _, cx| this.remove_attachment(&remove_id, cx)),
                )
            });
        let issue = item.error.clone().map(|error| {
            div()
                .id(SharedString::from(format!("attachment-issue-{id}")))
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.text_danger)
                .tooltip(Tooltip::text(error))
                .child(Icon::new(IconName::WarningCircle).size(12.))
        });
        let retry_id = id.clone();
        let retry = (item.status == attachments::Status::Error).then(|| {
            IconButton::new(
                SharedString::from(format!("retry-attachment-{id}")),
                IconName::ArrowsClockwise,
            )
            .size(IconButtonSize::Xs)
            .disabled(disabled)
            .tooltip("Retry upload")
            .on_click(cx.listener(move |this, _, _, cx| this.retry_attachment(&retry_id, cx)))
        });
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .self_start()
            .children(issue)
            .children(retry)
            .child(remove);
        let thumb = preview.map(|source| {
            let open_id = id.clone();
            div()
                .id(SharedString::from(format!("attachment-preview-{id}")))
                .relative()
                .flex_none()
                .size(px(if media { 46. } else { 40. }))
                .rounded(px(12.))
                .overflow_hidden()
                .bg(c.surface_panel)
                .cursor_pointer()
                .tooltip(Tooltip::text(format!("{} · {size} · {status}", item.name)))
                .child(source.size_full())
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(12.))
                        .border(px(0.5))
                        .border_color(c.border_standard),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.previewing = Some(open_id.to_string());
                    cx.notify();
                }))
        });
        if media {
            return div()
                .id(SharedString::from(format!("attachment-{id}")))
                .relative()
                .flex()
                .flex_none()
                .size(px(46.))
                .children(thumb)
                .child(
                    div()
                        .absolute()
                        .top(px(2.))
                        .right(px(2.))
                        .rounded_full()
                        .bg(c.surface_panel)
                        .child(actions),
                )
                .into_any_element();
        }
        let file_icon = div()
            .size(px(40.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(9.))
            .bg(c.affordance_subtle)
            .text_color(c.text_subtle)
            .child(Icon::new(IconName::FileText).size(24.));
        div()
            .id(SharedString::from(format!("attachment-{id}")))
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .w(px(288.))
            .max_w_full()
            .h(px(46.))
            .p(px(2.))
            .min_w_0()
            .rounded(px(12.))
            .border_1()
            .border_color(c.border_standard)
            .map(|d| match thumb {
                Some(thumb) => d.child(thumb),
                None => d.child(file_icon),
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .id(SharedString::from(format!("attachment-name-{id}")))
                            .text_label_sm()
                            .text_color(c.text_standard)
                            .truncate()
                            .tooltip(Tooltip::text(format!(
                                "{} · {size} · {}",
                                item.name,
                                if kind.is_empty() { "File" } else { &kind }
                            )))
                            .child(item.name.clone()),
                    )
                    .child(
                        div()
                            .text_caption()
                            .text_color(c.text_subtle)
                            .truncate()
                            .child(format!("{size} · {status}")),
                    ),
            )
            .child(actions)
            .into_any_element()
    }

    /// The community's background sends (`BackgroundUploadStatus.tsx`):
    /// the phase, a 64px bar of the bytes sent (an indeterminate sweep
    /// before there are byte counts) and Cancel, which stops the newest
    /// send and hands it back.
    fn background_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let queue = self.queue.read(cx);
        if !queue.uploading() {
            return None;
        }
        let progress = queue.progress(cx);
        let c = *cx.colors();
        let reduce = cx.reduce_motion();
        let bar = div()
            .relative()
            .flex_none()
            .w(px(64.))
            .h(px(4.))
            .rounded_full()
            .overflow_hidden()
            .bg(c.surface_inset)
            .child(match progress.fraction {
                Some(fraction) => div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(64. * fraction.clamp(0., 1.) as f32))
                    .rounded_full()
                    .bg(c.affordance_prominent)
                    .into_any_element(),
                None if reduce => div()
                    .absolute()
                    .inset_0()
                    .rounded_full()
                    .bg(c.affordance_prominent)
                    .opacity(0.4)
                    .into_any_element(),
                None => div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(px(64. / 3.))
                    .rounded_full()
                    .bg(c.affordance_prominent)
                    .with_animation(
                        "background-upload-sweep",
                        Animation::new(Duration::from_millis(1200))
                            .repeat()
                            .with_easing(ease_in_out),
                        |sweep, delta| {
                            // translateX(-100%) to translateX(300%).
                            sweep.left(px(64. / 3. * (4. * delta - 1.)))
                        },
                    )
                    .into_any_element(),
            });
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_caption()
                .text_color(c.text_subtle)
                .child(progress.phase)
                .child(bar)
                .child(
                    Button::new("cancel-background-upload", "Cancel")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Sm)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.queue.update(cx, |queue, cx| queue.cancel_newest(cx));
                        })),
                )
                .into_any_element(),
        )
    }

    /// "Could not attach file", and the queue's notices when this composer
    /// hosts them: toasts at the window's top right.
    fn toasts(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let host = cx.entity_id();
        let queue = self.queue.read(cx);
        let notices = if queue.hosts_notices(host) {
            queue.notices.clone()
        } else {
            Vec::new()
        };
        if notices.is_empty() && self.attach_error.is_none() {
            return None;
        }
        let mut cards: Vec<AnyElement> = Vec::new();
        if let Some(error) = &self.attach_error {
            cards.push(
                Toast::new("attach-error", "Could not attach file")
                    .tone(ToastTone::Error)
                    .description(error.clone())
                    .on_dismiss(cx.listener(|this, _, _, cx| {
                        this.attach_error = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        for notice in notices {
            let id = notice.id.clone();
            let toast = Toast::new(
                SharedString::from(format!("upload-notice-{}", notice.id)),
                notice.message.clone(),
            )
            .tone(ToastTone::Error);
            let toast = match notice.recovery {
                Some(job) => toast.action(
                    Button::new(
                        SharedString::from(format!("retry-recovery-{job}")),
                        "Retry failed send recovery",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queue
                            .update(cx, |queue, cx| queue.retry_recovery(&job, cx));
                    })),
                ),
                None => toast.on_dismiss(cx.listener(move |this, _, _, cx| {
                    this.queue.update(cx, |queue, cx| queue.dismiss(&id, cx));
                })),
            };
            cards.push(toast.into_any_element());
        }
        let right = window.viewport_size().width - px(16.) - px(384.);
        Some(
            deferred(
                anchored()
                    .position_mode(AnchoredPositionMode::Window)
                    .position(point(right, px(128.)))
                    .child(div().flex().flex_col().gap(px(12.)).children(cards)),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    /// "Preview <name>": the image in a dialog titled with its name.
    fn preview_dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = self.previewing.as_ref()?;
        let item = self
            .files
            .read(cx)
            .items
            .iter()
            .find(|item| &item.id == id)?
            .clone();
        let source = preview_source(&item)?;
        let dialog = Dialog::new("attachment-preview", item.name.clone())
            .on_close(cx.listener(|this, _, _, cx| {
                this.previewing = None;
                cx.notify();
            }))
            .child(
                div()
                    .w_full()
                    .h(window.viewport_size().height * 0.65)
                    .child(source.size_full().object_fit(ObjectFit::Contain)),
            );
        let content = div()
            .key_context("AttachmentPreview")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.previewing = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(dialog);
        Some(overlay::modal(window, content, None).into_any_element())
    }

    fn tools(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        if self.formatting {
            let active = self.active_formats(cx);
            let state = self.input.read(cx);
            let (text, selection) = (state.value().to_string(), state.selected_range());
            let button = |format: Format, cx: &mut Context<Self>| {
                IconButton::new(
                    SharedString::from(format!("format-{format:?}")),
                    format.icon(),
                )
                .size(IconButtonSize::Sm)
                .selected(match format {
                    Format::Bold => active.bold,
                    Format::Italic => active.italic,
                    Format::Strike => active.strike,
                    Format::Code => active.code,
                    Format::Spoiler => active.spoiler,
                    Format::Link => false,
                    _ => format::active(&text, selection.clone(), format),
                })
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
            // Disabled, not hidden, while editing (`renderLeadingTools`).
            .child(
                IconButton::new("attach", IconName::Paperclip)
                    .size(IconButtonSize::Sm)
                    .tooltip("Attach files")
                    .disabled(editing || self.sending)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_files(window, cx))),
            )
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
            // A plain 32px button: no hover fill, an 18px glyph. While
            // editing it stays but records nothing (`useVoiceCapture`).
            .child(
                div()
                    .id("record-voice-note")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded(px(10.))
                    .text_color(if self.sending {
                        c.text_unavailable
                    } else {
                        c.text_subtle
                    })
                    .when(!self.sending, |d| d.cursor_pointer())
                    .tooltip(Tooltip::text("Record voice note"))
                    .child(Icon::new(IconName::Microphone).size(18.))
                    .on_click(cx.listener(|this, _, _, cx| this.start_capture(cx))),
            )
            .child(
                IconButton::new("format", IconName::TextAa)
                    .size(IconButtonSize::Sm)
                    .tooltip("Formatting")
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
        item: &DraftFile,
        voice: &Voice,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = *cx.colors();
        let id = SharedString::from(item.id.clone());
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
        let busy = item.status.busy();
        let (remove_id, retry_id) = (id.clone(), id.clone());
        div()
            .id(SharedString::from(format!("voice-draft-{id}")))
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .w(px(336.))
            .max_w_full()
            .min_w_0()
            .child(media::voice_draft(
                SharedString::from(format!("voice-draft-card-{id}")),
                &attachment,
                &media,
                cx,
            ))
            .when_some(item.error.clone(), |d, error| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(4.))
                        .text_caption()
                        .text_color(c.text_danger)
                        .child(error)
                        .child(
                            Button::new(
                                SharedString::from(format!("retry-voice-{id}")),
                                "Retry voice note upload",
                            )
                            .disabled(disabled)
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.retry_attachment(&retry_id, cx),
                            )),
                        ),
                )
            })
            .child(
                div()
                    .id(SharedString::from(format!("remove-voice-{id}")))
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
                    .when(!disabled, |d| d.cursor_pointer())
                    .tooltip(Tooltip::text("Remove voice note"))
                    .map(|d| {
                        if busy {
                            d.child(Spinner::new().size(12.))
                        } else {
                            d.child(Icon::new(IconName::X).size(16.))
                        }
                    })
                    .when(!disabled, |d| {
                        d.on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.remove_attachment(&remove_id, cx)
                            }),
                        )
                    }),
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
                .on_click(
                    cx.listener(|this, _, window, cx| this.send_without_outsiders(window, cx)),
                ),
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
                    "enter" => this.send_without_outsiders(window, cx),
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
                    name, icon, detail, ..
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
                            .children(detail.clone().map(|detail| {
                                div()
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .truncate()
                                    .child(detail)
                            })),
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
        let retry_index = completion.items.len();
        let retry = completion.retry.map(|_| {
            div()
                .id("completion-retry")
                .flex()
                .items_center()
                .px(px(8.))
                .py(px(6.))
                .rounded(px(3.))
                .cursor_pointer()
                .when(completion.selected == retry_index, |d| {
                    d.bg(c.affordance_popover_selected)
                })
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    if let Some(completion) = &mut this.completion
                        && completion.selected != retry_index
                    {
                        completion.selected = retry_index;
                        cx.notify();
                    }
                }))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.accept(retry_index, window, cx)),
                )
                .child("Retry suggestions")
        });
        let status = completion.status.clone().map(|status| {
            div()
                .flex_none()
                .mx(px(8.))
                .my(px(4.))
                .text_caption()
                .text_color(c.text_subtle)
                .child(status)
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
                                .children(rows)
                                .children(retry),
                        )
                        .children(status),
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
            if !self.pending.is_empty() && self.completion_key.is_some_and(|(len, _)| len == key.0)
            {
                self.leave_pending(window, cx);
            }
            if self.completion_key.is_some_and(|(len, _)| len == key.0) {
                // The caret moved: the toolbar follows it.
                self.restyle(cx);
            }
            self.refresh_completion(cx);
        }
        if self.drag_over && !cx.has_active_drag() {
            self.drag_over = false;
        }
        let can_submit = self.can_submit(cx);
        let editing = self.editing.is_some();
        let has_attachments = self.has_files(cx) && !editing;
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
                    && !this.has_files(cx)
                    && this.recipients(cx).is_empty()
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
                    && completion.count() > 0
                {
                    let index = completion.selected;
                    this.accept(index, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                if let Some(completion) = &this.completion
                    && completion.count() > 0
                {
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
                    let over = this.accepts_drop() && event.bounds.contains(&event.event.position);
                    if this.drag_over != over {
                        this.drag_over = over;
                        cx.notify();
                    }
                }),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.drag_over = false;
                if this.accepts_drop() {
                    this.add_paths(paths.paths().to_vec(), cx);
                }
            }))
            .when(editing, |d| d.child(self.edit_header(cx)))
            .children(self.edit_retry(cx))
            .children(self.background_status(cx))
            .when(self.drag_over, |d| {
                d.child(
                    div()
                        .px(px(8.))
                        .text_caption()
                        .text_color(c.text_subtle)
                        .child("Drop files to attach"),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .min_w_0()
                    .pl(px(4.))
                    .pt(px(4.))
                    .children(self.attachment_row(cx))
                    .child(textarea),
            )
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
            })
            .children(self.error.clone().map(|error| {
                div()
                    .px(px(8.))
                    .text_body()
                    .text_color(c.text_standard)
                    .child(error)
            }));

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
            .children(self.toasts(window, cx))
            .children(self.outsiders_dialog(window, cx))
            .children(self.preview_dialog(window, cx))
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
    }
}
