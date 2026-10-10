//! The media viewer's comments sidebar (`MediaReviewViewer.tsx`): the
//! message's thread replies, every page loaded, with a composer that
//! replies to the thread. Comments on a video can carry the current time
//! (`⏱ m:ss — text`, `media-timecode.ts`).

use super::super::composer::{Composer, ComposerEvent, Submission};
use super::super::markdown::Directory;
use super::super::message::{self, Layout, Rows};
use crate::community::Community;
use crate::mirror::mirror;
use buzz_relay_client::session::ThreadView;
use buzz_relay_client::{EventId, ThreadSnapshot, ThreadStatus};
use buzz_ui::{ActiveTheme, Checkbox, Icon, IconName, PanelHeader, TypeStyled};
use gpui_kit::component::input::TextareaState;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;
use std::sync::Arc;

/// `formatMediaTimecode`: `m:ss`, whole seconds.
pub fn timecode(seconds: f64) -> String {
    let whole = if seconds.is_finite() {
        seconds.max(0.).floor() as u64
    } else {
        0
    };
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// `⏱ <m:ss> — <text>`.
pub fn with_timecode(seconds: f64, text: &str) -> String {
    format!("⏱ {} — {}", timecode(seconds), text)
}

/// Reads the video's current time.
pub type VideoTime = Rc<dyn Fn(&App) -> f64>;

pub struct Comments {
    community: Entity<Community>,
    channel: String,
    root: EventId,
    view: ThreadView,
    snapshot: Arc<ThreadSnapshot>,
    composer: Entity<Composer>,
    input: Entity<TextareaState>,
    /// Video only: the time a new comment is stamped with, when asked.
    video_time: Option<VideoTime>,
    at_frame: bool,
    error: Option<SharedString>,
    list: ScrollHandle,
    _watch: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Comments {
    pub fn new(
        community: Entity<Community>,
        channel: String,
        root: EventId,
        video_time: Option<VideoTime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view = community.read(cx).session.open_thread(&channel, root, true);
        let snapshot = view.snapshot.borrow().clone();
        let watch = mirror(view.snapshot.clone(), cx, |this: &mut Self, value| {
            this.snapshot = value;
            // `useMediaComments` reads every page.
            if this.snapshot.can_load_more && this.snapshot.status != ThreadStatus::Loading {
                this.view.load_more();
            }
        });
        let composer = cx.new(|cx| {
            Composer::new(
                community.clone(),
                channel.clone(),
                Some(root),
                "Reply to thread".into(),
                window,
                cx,
            )
        });
        let input = composer.read(cx).input();
        let subscriptions = vec![
            cx.observe(&community, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &ComposerEvent, window, cx| {
                    if let ComposerEvent::Submit(submission) = event {
                        this.send(submission.clone(), window, cx);
                    }
                },
            ),
        ];
        Self {
            community,
            channel,
            root,
            view,
            snapshot,
            composer,
            input,
            video_time,
            at_frame: true,
            error: None,
            list: ScrollHandle::new(),
            _watch: watch,
            _subscriptions: subscriptions,
        }
    }

    /// Open the sidebar at first: there are comments, or reading failed.
    pub fn has_comments(&self) -> bool {
        !self.snapshot.replies.is_empty() || self.snapshot.error.is_some()
    }

    /// Typing in the composer must not drive the viewer's keys.
    pub fn typing(&self, window: &Window, cx: &App) -> bool {
        self.input
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
    }

    fn send(&mut self, mut submission: Submission, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(time) = self.video_time.as_ref().filter(|_| self.at_frame) {
            submission.content = with_timecode(time(cx), &submission.content);
        }
        let session = self.community.read(cx).session.clone();
        // The message may itself be a reply: answer its thread's root.
        let root = self
            .snapshot
            .root
            .as_ref()
            .map_or(self.root, |root| root.id);
        let channel = self.channel.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = session
                .send(
                    &channel,
                    &submission.content,
                    submission.mentions.clone(),
                    submission.attachments.clone(),
                    Some((root, root)),
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        this.error = None;
                        this.composer
                            .update(cx, |composer, cx| composer.sent(window, cx));
                        this.list.scroll_to_bottom();
                    }
                    Err(error) => {
                        this.error = Some(error.into());
                        this.composer.update(cx, |composer, cx| composer.failed(cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for Comments {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let snapshot = self.snapshot.clone();
        let resolved = snapshot.root.is_some();
        let count = snapshot.replies.len();
        let community = self.community.read(cx);
        let rows = Rc::new(Rows::new(
            community.session.viewer(),
            Directory::of(community, None, cx),
        ));
        let note = |text: SharedString| {
            div()
                .px(px(16.))
                .py(px(8.))
                .text_caption()
                .text_color(c.text_subtle)
                .child(text)
        };
        let status: Option<SharedString> = if let Some(error) = &snapshot.error {
            Some(error.clone().into())
        } else if !resolved {
            Some(match snapshot.status {
                ThreadStatus::Loading | ThreadStatus::Idle => "Loading comments…".into(),
                _ => "Original message unavailable.".into(),
            })
        } else if count == 0 {
            Some("No comments yet. Add the first below.".into())
        } else {
            None
        };
        let items = message::items(&snapshot.replies);
        let list = div()
            .id("media-comments-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.list)
            .px(px(12.))
            .py(px(8.))
            .children(
                items
                    .iter()
                    .map(|item| message::render_in(Layout::Nested, item, &rows, cx)),
            )
            .children(status.map(note))
            .when(snapshot.limited, |d| {
                d.child(note("Thread history limit reached.".into()))
            });
        let video = self.video_time.is_some();
        div()
            .id("media-comments")
            .flex()
            .flex_col()
            .size_full()
            .text_body_sm()
            .text_color(c.text_standard)
            .child(
                PanelHeader::new().leading(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(Icon::new(IconName::ChatCircle).size(20.))
                        .child(div().text_label().child("Comments"))
                        .when(resolved, |d| {
                            d.child(
                                div()
                                    .px(px(8.))
                                    .text_caption()
                                    .text_color(c.text_subtle)
                                    .child(count.to_string()),
                            )
                        }),
                ),
            )
            .child(list)
            .children(self.error.clone().map(|error| {
                div()
                    .px(px(16.))
                    .pb(px(4.))
                    .text_caption()
                    .text_color(c.text_danger)
                    .child(error)
            }))
            .when(video, |d| {
                let time = self.video_time.as_ref().map_or(0., |time| time(cx));
                let weak = cx.entity().downgrade();
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .px(px(16.))
                        .pb(px(4.))
                        .child(
                            Checkbox::new("comment-at-frame", self.at_frame)
                                .label("Comment at current frame")
                                .on_change(move |checked, _, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.at_frame = checked;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            div()
                                .font_family(buzz_ui::tokens::font::MONO)
                                .text_caption()
                                .text_color(c.text_subtle)
                                .child(timecode(time)),
                        ),
                )
            })
            .child(div().mx(px(-4.)).child(self.composer.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn stamps_comments_with_the_video_time() {
        assert_eq!(timecode(0.), "0:00");
        assert_eq!(timecode(83.9), "1:23");
        assert_eq!(with_timecode(5.2, "Nice cut"), "⏱ 0:05 — Nice cut");
    }
}
