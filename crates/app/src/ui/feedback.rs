//! "Send feedback" (`bundled/feedback`): product feedback for the
//! deployment's private inbox, sent through the session's durable outbox.
//! The dialog follows the outbox's pending feedback, so a send survives
//! closing the dialog or the app and can be retried with the same event.

use super::modal::{Dismissed, error};
use crate::community::Community;
use crate::mirror::mirror;
use buzz_relay_client::feedback::{self, Category, DIAGNOSTICS_NAME};
use buzz_relay_client::{
    Attachment, Delivery, FeedbackState, PendingFeedback, Session, UploadError, UploadFile,
};
use buzz_ui::{
    ActiveTheme, AlertDialog, Button, ButtonVariant, Checkbox, Dialog, Surface, TextArea,
    TypeStyled,
};
use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::Path;
use std::sync::Arc;

const DESCRIPTION: &str = "Feedback text goes to this Buzz deployment's feedback inbox, not a channel. Images and optional diagnostics use ordinary media storage, not inbox-only access; people allowed to read media on this deployment may access them if they have the link. Images upload when selected; diagnostics upload when you send. Removing an attachment or closing this form does not delete uploaded files.";
const PLACEHOLDER: &str = "Tell us what went wrong, or share general feedback.";
const DIAGNOSTICS_DETAIL: &str = "Includes capture time, app version when available, platform, user agent, and language. No application logs.";
const UNAVAILABLE: &str = "Connect to a Buzz deployment to send or retry feedback.";
const DISCARD_CONFIRM: &str = "Discard this saved feedback locally? It may already have been delivered; discarding will not undo delivery or delete uploaded attachments.";

/// The primary actions beside Close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actions {
    /// Nothing is pending: the form.
    Send,
    /// The relay took it: clear it.
    Done,
    /// It failed or may not have arrived: discard or retry it.
    Recover,
    /// Still sending.
    Waiting,
}

fn actions(pending: Option<&PendingFeedback>) -> Actions {
    match pending.map(|pending| pending.delivery) {
        None => Actions::Send,
        Some(Delivery::Accepted | Delivery::Seen) => Actions::Done,
        Some(Delivery::Failed | Delivery::Unknown) => Actions::Recover,
        Some(Delivery::Sending) => Actions::Waiting,
    }
}

fn status(pending: &PendingFeedback) -> String {
    match pending.delivery {
        Delivery::Accepted | Delivery::Seen => "Feedback received by the relay.".into(),
        Delivery::Unknown => {
            "Delivery could not be confirmed. Retry the same feedback; do not submit a duplicate."
                .into()
        }
        Delivery::Failed => format!(
            "Feedback was not delivered. {}",
            pending.error.as_deref().unwrap_or("Retry to send.")
        ),
        Delivery::Sending => "Sending feedback…".into(),
    }
}

/// `en_US.UTF-8` → `en-US`, like `navigator.language`.
fn language(lang: Option<&str>) -> String {
    let tag = lang
        .and_then(|lang| lang.split(['.', '@']).next())
        .filter(|tag| !tag.is_empty() && *tag != "C" && *tag != "POSIX");
    tag.map_or_else(|| "unknown".into(), |tag| tag.replace('_', "-"))
}

/// The optional diagnostics file: no logs, only what identifies the
/// build and environment (`feedbackDiagnostics`).
fn diagnostics(captured: chrono::DateTime<chrono::Utc>, lang: Option<&str>) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    [
        "Buzz feedback diagnostics".to_owned(),
        format!(
            "captured: {}",
            captured.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        ),
        format!("app version: {version}"),
        format!("platform: {os} {}", std::env::consts::ARCH),
        format!("user agent: buzz-gpui/{version} ({os})"),
        format!("language: {}", language(lang)),
    ]
    .join("\n")
}

fn read_image(path: &Path) -> Result<UploadFile, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Refuse oversized files before reading them.
    let size = std::fs::metadata(path).map_or(0, |meta| meta.len());
    if size > 50 * 1024 * 1024 {
        return Err(feedback::IMAGE_TOO_LARGE.into());
    }
    let bytes = std::fs::read(path).map_err(|error| format!("Couldn’t read that file: {error}"))?;
    feedback::image(&name, bytes)
}

pub struct FeedbackDialog {
    community: Option<Entity<Community>>,
    state: FeedbackState,
    category: Option<Category>,
    message: Entity<TextareaState>,
    image: Option<Attachment>,
    /// Kept so a second send reuses it instead of uploading again.
    uploaded_diagnostics: Option<Attachment>,
    include_diagnostics: bool,
    busy: bool,
    error: Option<SharedString>,
    confirm_discard: bool,
    /// Bumped by every async operation and by closing; stale results
    /// are dropped.
    attempt: u64,
    _subscriptions: Vec<Subscription>,
    _watch: Option<Task<()>>,
}

impl EventEmitter<Dismissed> for FeedbackDialog {}

impl FeedbackDialog {
    pub fn new(
        community: Option<Entity<Community>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(PLACEHOLDER)
                .auto_grow(3, 8)
        });
        message.update(cx, |state, cx| state.focus(window, cx));
        let _watch = community.as_ref().map(|community| {
            let receiver = community.read(cx).session.feedback();
            mirror(receiver, cx, |this: &mut Self, state| {
                if let Some(error) = &state.error
                    && this.state.error.as_ref() != Some(error)
                {
                    this.error = Some(error.clone().into());
                }
                this.state = state;
            })
        });
        Self {
            community,
            state: FeedbackState::default(),
            category: None,
            message: message.clone(),
            image: None,
            uploaded_diagnostics: None,
            include_diagnostics: false,
            busy: false,
            error: None,
            confirm_discard: false,
            attempt: 0,
            _subscriptions: vec![cx.subscribe(&message, |_, _, _: &InputEvent, cx| cx.notify())],
            _watch,
        }
    }

    fn session(&self, cx: &App) -> Option<Arc<Session>> {
        Some(self.community.as_ref()?.read(cx).session.clone())
    }

    fn available(&self) -> bool {
        self.state.hydrated && self.state.available
    }

    fn text(&self, cx: &App) -> String {
        self.message.read(cx).value().to_string()
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.attempt += 1;
        self.busy = false;
        cx.emit(Dismissed);
    }

    fn attach_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session(cx) else {
            return;
        };
        if !self.available() || self.busy {
            return;
        }
        // Development only: a headless session has no file dialog.
        let dev_file = std::env::var_os("BUZZ_DEV_PICK_FILE").map(std::path::PathBuf::from);
        let paths = dev_file.is_none().then(|| {
            cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Attach image".into()),
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match (dev_file, paths) {
                (Some(path), _) => path,
                (None, Some(paths)) => match paths.await {
                    Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                        Some(path) => path,
                        None => return,
                    },
                    _ => return,
                },
                (None, None) => return,
            };
            let Ok(attempt) = this.update(cx, |this, cx| {
                this.attempt += 1;
                this.busy = true;
                this.error = None;
                cx.notify();
                this.attempt
            }) else {
                return;
            };
            let file = cx
                .background_executor()
                .spawn(async move { read_image(&path) })
                .await;
            let result = match file {
                Ok(file) => {
                    let mime = file.mime.clone();
                    session
                        .upload_feedback(file)
                        .await
                        .and_then(|uploaded| {
                            if uploaded.mime == mime {
                                Ok(uploaded)
                            } else {
                                Err(UploadError::Invalid)
                            }
                        })
                        .map_err(|error| error.to_string())
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                if this.attempt != attempt {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(image) => this.image = Some(image),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session(cx) else {
            return;
        };
        if !self.available() || self.state.pending.is_some() || self.busy {
            return;
        }
        let message = self.text(cx);
        let category = self.category;
        let mut attachments: Vec<Attachment> = self.image.iter().cloned().collect();
        let mut upload = None;
        if self.include_diagnostics {
            match &self.uploaded_diagnostics {
                Some(uploaded) => attachments.push(uploaded.clone()),
                None => {
                    let lang = std::env::var("LANG").ok();
                    let text = diagnostics(chrono::Utc::now(), lang.as_deref());
                    // Reserve the largest result before uploading it.
                    attachments.push(feedback::diagnostics_placeholder(
                        text.len() as u64,
                        session.origin(),
                    ));
                    upload = Some(UploadFile {
                        name: DIAGNOSTICS_NAME.into(),
                        mime: "text/plain".into(),
                        bytes: text.into_bytes(),
                    });
                }
            }
        }
        if let Err(error) = feedback::event(&message, category, &attachments, session.origin()) {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        self.attempt += 1;
        let attempt = self.attempt;
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<(), String> = async {
                if let Some(file) = upload {
                    let uploaded = session
                        .upload_feedback(file)
                        .await
                        .map_err(|error| error.to_string())?;
                    if !matches!(
                        uploaded.mime.as_str(),
                        "application/octet-stream" | "text/plain"
                    ) {
                        return Err(UploadError::Invalid.to_string());
                    }
                    let current = this
                        .update(cx, |this, _| {
                            (this.attempt == attempt)
                                .then(|| this.uploaded_diagnostics = Some(uploaded.clone()))
                        })
                        .ok()
                        .flatten();
                    if current.is_none() {
                        return Ok(());
                    }
                    if let Some(last) = attachments.last_mut() {
                        *last = uploaded;
                    }
                }
                if this
                    .read_with(cx, |this, _| this.attempt != attempt)
                    .unwrap_or(true)
                {
                    return Ok(());
                }
                session
                    .send_feedback(&message, category, &attachments)
                    .await
                    .map(|_| ())
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.attempt != attempt {
                    return;
                }
                this.busy = false;
                if let Err(error) = result {
                    this.error = Some(error.into());
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(pending)) = (self.session(cx), self.state.pending.clone()) else {
            return;
        };
        if !self.available() || self.busy {
            return;
        }
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.retry_feedback(pending.id).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Forgets the pending feedback and closes, ready for new feedback.
    fn clear_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(pending)) = (self.session(cx), self.state.pending.clone()) else {
            return;
        };
        if self.busy {
            return;
        }
        self.confirm_discard = false;
        self.busy = true;
        let attempt = self.attempt;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = session.dismiss_feedback(pending.id).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.attempt != attempt {
                    return;
                }
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.category = None;
                        this.image = None;
                        this.uploaded_diagnostics = None;
                        this.include_diagnostics = false;
                        this.error = None;
                        this.message
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.close(cx);
                    }
                    Err(error) => {
                        this.error = Some(if error.is_empty() {
                            "Could not clear feedback.".into()
                        } else {
                            error.into()
                        })
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn form(&self, cx: &mut Context<Self>) -> Div {
        let c = *cx.colors();
        let busy = self.busy;
        let available = self.available();
        let categories = Category::ALL.into_iter().map(|category| {
            let chosen = self.category == Some(category);
            Button::new(
                SharedString::from(format!("feedback-{}", category.as_str())),
                category.label(),
            )
            .variant(if chosen {
                ButtonVariant::Prominent
            } else {
                ButtonVariant::Subtle
            })
            .surface(Surface::Floating)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.category = (this.category != Some(category)).then_some(category);
                cx.notify();
            }))
        });
        let image = self.image.as_ref().map(|image| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(rems(0.5))
                .child(format!("Image uploaded: {}", image.name))
                .child(
                    Button::new("feedback-remove-image", "Remove image")
                        .surface(Surface::Floating)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.image = None;
                            cx.notify();
                        })),
                )
        });
        let weak = cx.entity().downgrade();
        let attachments = self.community.is_some().then(|| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .mt(rems(1.))
                        .mb(rems(0.5))
                        .text_label_sm()
                        .child("Attach image (optional)"),
                )
                .child(
                    div().flex().child(
                        Button::new("feedback-image", "Choose image")
                            .surface(Surface::Floating)
                            .disabled(!available || busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.attach_image(window, cx)),
                            ),
                    ),
                )
                .children(image.map(|image| image.mt(rems(0.5))))
                .child(
                    div().mt(rems(1.)).child(
                        Checkbox::new("feedback-diagnostics", self.include_diagnostics)
                            .label("Attach diagnostics")
                            .surface(Surface::Floating)
                            .disabled(!available || busy)
                            .on_change(move |checked, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.include_diagnostics = checked;
                                    cx.notify();
                                });
                            }),
                    ),
                )
                .child(div().child(DIAGNOSTICS_DETAIL))
        });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(rems(0.5))
                    .mb(rems(1.))
                    .children(categories),
            )
            .child(
                div()
                    .mb(rems(0.5))
                    .text_label_sm()
                    .text_color(c.text_standard)
                    .child("Your feedback"),
            )
            .child(
                TextArea::new(&self.message)
                    .surface(Surface::Floating)
                    .disabled(busy),
            )
            .children(attachments)
    }

    fn pending_view(&self, pending: &PendingFeedback) -> Div {
        div()
            .flex()
            .flex_col()
            .child(div().mb(rems(1.)).child(pending.content.clone()))
            .child(div().child(status(pending)))
    }
}

impl Render for FeedbackDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let busy = self.busy;
        let available = self.available();
        let pending = self.state.pending.clone();
        if self.confirm_discard {
            return AlertDialog::new("Discard locally")
                .description(DISCARD_CONFIRM)
                .action(
                    Button::new("feedback-discard-cancel", "Cancel")
                        .surface(Surface::Floating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_discard = false;
                            cx.notify();
                        })),
                )
                .action(
                    Button::new("feedback-discard-confirm", "Discard locally")
                        .destructive()
                        .on_click(
                            cx.listener(|this, _, window, cx| this.clear_pending(window, cx)),
                        ),
                )
                .into_any_element();
        }
        let close = Button::new("feedback-close", "Close")
            .surface(Surface::Floating)
            .on_click(cx.listener(|this, _, _, cx| this.close(cx)));
        let dialog = Dialog::new("feedback", "Send feedback")
            .description(DESCRIPTION)
            .on_close(cx.listener(|this, _, _, cx| this.close(cx)))
            .child(match &pending {
                Some(pending) => self.pending_view(pending),
                None => self.form(cx),
            })
            .when(!available, |this| {
                this.child(
                    div()
                        .mt(rems(1.))
                        .text_body_sm()
                        .text_color(c.text_subtle)
                        .child(UNAVAILABLE),
                )
            })
            .children(
                self.error
                    .clone()
                    .map(|message| error(message, cx).mt(rems(1.))),
            )
            .footer(close);
        match actions(pending.as_ref()) {
            Actions::Send => dialog.footer(
                Button::new("feedback-send", "Send feedback")
                    .prominent()
                    .loading(busy)
                    .disabled(!available || self.text(cx).trim().is_empty() || busy)
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            ),
            Actions::Done => dialog.footer(
                Button::new("feedback-done", "Done")
                    .prominent()
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.clear_pending(window, cx))),
            ),
            Actions::Recover => dialog
                .footer(
                    Button::new("feedback-discard", "Discard locally")
                        .surface(Surface::Floating)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_discard = true;
                            cx.notify();
                        })),
                )
                .footer(
                    Button::new("feedback-retry", "Retry same feedback")
                        .prominent()
                        .disabled(!available || busy)
                        .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx))),
                ),
            Actions::Waiting => dialog,
        }
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_relay_client::EventId;
    use core::prelude::v1::test;

    fn pending(delivery: Delivery, error: Option<&str>) -> PendingFeedback {
        PendingFeedback {
            id: EventId::from_byte_array([0; 32]),
            content: "Broken".into(),
            delivery,
            error: error.map(Into::into),
        }
    }

    #[test]
    fn actions_follow_delivery() {
        assert_eq!(actions(None), Actions::Send);
        let of = |delivery| actions(Some(&pending(delivery, None)));
        assert_eq!(of(Delivery::Sending), Actions::Waiting);
        assert_eq!(of(Delivery::Accepted), Actions::Done);
        assert_eq!(of(Delivery::Seen), Actions::Done);
        assert_eq!(of(Delivery::Failed), Actions::Recover);
        assert_eq!(of(Delivery::Unknown), Actions::Recover);
    }

    #[test]
    fn status_copy() {
        assert_eq!(
            status(&pending(Delivery::Sending, None)),
            "Sending feedback…"
        );
        assert_eq!(
            status(&pending(Delivery::Accepted, None)),
            "Feedback received by the relay."
        );
        assert_eq!(
            status(&pending(Delivery::Failed, None)),
            "Feedback was not delivered. Retry to send."
        );
        assert_eq!(
            status(&pending(
                Delivery::Failed,
                Some("Relay rejected the message (403)")
            )),
            "Feedback was not delivered. Relay rejected the message (403)"
        );
        assert!(status(&pending(Delivery::Unknown, None)).starts_with("Delivery could not"));
    }

    #[test]
    fn diagnostics_text() {
        let at = chrono::DateTime::from_timestamp(1_760_000_000, 0).unwrap();
        let text = diagnostics(at, Some("en_US.UTF-8"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Buzz feedback diagnostics");
        assert_eq!(lines[1], "captured: 2025-10-09T08:53:20.000Z");
        assert_eq!(
            lines[2],
            format!("app version: {}", env!("CARGO_PKG_VERSION"))
        );
        assert!(lines[3].starts_with("platform: "));
        assert!(lines[4].starts_with("user agent: buzz-gpui/"));
        assert_eq!(lines[5], "language: en-US");
        assert_eq!(language(Some("C")), "unknown");
        assert_eq!(language(None), "unknown");
        assert_eq!(language(Some("de_DE@euro")), "de-DE");
    }
}
