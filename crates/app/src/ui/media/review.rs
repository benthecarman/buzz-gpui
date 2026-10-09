//! The media review viewer (`MediaReviewViewer.tsx`, `ImageReviewStage.tsx`):
//! a dark full-window dialog with the image gallery or the video player.
//!
//! One viewer is open at a time, app-wide, so any row can open it; the
//! workspace draws it above everything through [`review_overlay`].

use super::video::{SKIP_SECONDS, VideoPlayer};
use super::{Remote, decoded, natural_size};
use buzz_relay_client::Media;
use buzz_relay_client::fold::{Attachment, AttachmentKind};
use buzz_ui::{
    ActiveTheme, Appearance, Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize,
    IconName, PanelHeader, ThemeScope, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;

const MIN_ZOOM: f32 = 0.5;
const BASE_MAX_ZOOM: f32 = 4.;
const ZOOM_STEP: f32 = 0.25;
const ZOOM_PRESETS: [f32; 4] = [0.5, 1., 1.5, 2.];
/// Pointer travel beyond this is a pan, not a click.
const CLICK_SLOP: f32 = 4.;
/// `.darkReviewBackdrop` padding, and the viewer's largest size.
const BACKDROP_PADDING: f32 = 40.;
const MAX_SIZE: (f32, f32) = (1520., 980.);

struct Host {
    review: Option<Entity<Review>>,
    /// Focus to give back on close.
    restore: Option<FocusHandle>,
}

struct HostGlobal(Entity<Host>);

impl Global for HostGlobal {}

fn host(cx: &mut App) -> Entity<Host> {
    if let Some(host) = cx.try_global::<HostGlobal>() {
        return host.0.clone();
    }
    let host = cx.new(|_| Host {
        review: None,
        restore: None,
    });
    cx.set_global(HostGlobal(host.clone()));
    host
}

/// Opens `url` from a message's `attachments`: an image with the message's
/// other images as its gallery, or a video in the player.
pub fn open_review(
    attachments: &[Attachment],
    url: &str,
    media: &Media,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(selected) = attachments.iter().find(|a| a.url == url) else {
        return;
    };
    let items: Vec<Attachment> = if selected.kind == AttachmentKind::Image {
        attachments
            .iter()
            .filter(|a| a.kind == AttachmentKind::Image)
            .cloned()
            .collect()
    } else {
        vec![selected.clone()]
    };
    let index = items.iter().position(|a| a.url == url).unwrap_or(0);
    let restore = window.focused(cx);
    let media = media.clone();
    let review = cx.new(|cx| Review::new(items, index, media, cx));
    let focus = review.read(cx).focus.clone();
    focus.focus(window, cx);
    host(cx).update(cx, |host, cx| {
        host.review = Some(review);
        host.restore = restore;
        cx.notify();
    });
}

/// The open viewer, for the workspace to draw over the window.
pub fn review_overlay(cx: &mut App) -> Option<impl IntoElement + use<>> {
    let review = host(cx).read(cx).review.clone()?;
    Some(
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(point(px(0.), px(0.)))
                .child(ThemeScope::new(Appearance::Dark, move |_, _| review)),
        )
        .with_priority(2),
    )
}

/// Closes the viewer. Its last video frame leaves the sprite atlas, which
/// only the window can do.
fn close(window: &mut Window, cx: &mut App) {
    let (review, restore) = host(cx).update(cx, |host, cx| {
        cx.notify();
        (host.review.take(), host.restore.take())
    });
    if let Some(video) = review.and_then(|review| review.read(cx).video.clone()) {
        for image in video.update(cx, |video, _| video.take_frames()) {
            let _ = window.drop_image(image);
        }
    }
    if let Some(focus) = restore {
        focus.focus(window, cx);
    }
}

/// A press on the image stage, which ends as a click (zoom) or a pan.
struct Press {
    origin: Point<Pixels>,
    offset: Point<f32>,
    click: bool,
}

pub struct Review {
    focus: FocusHandle,
    media: Media,
    items: Vec<Attachment>,
    index: usize,
    video: Option<Entity<VideoPlayer>>,
    zoom: f32,
    /// Image offset from the stage centre, in pixels.
    offset: Point<f32>,
    press: Option<Press>,
    /// The stage's bounds as last drawn.
    stage: Rc<Cell<Bounds<Pixels>>>,
}

impl Review {
    fn new(items: Vec<Attachment>, index: usize, media: Media, cx: &mut Context<Self>) -> Self {
        let video = items
            .get(index)
            .filter(|a| a.kind == AttachmentKind::Video)
            .cloned()
            .map(|attachment| {
                let media = media.clone();
                cx.new(|cx| VideoPlayer::new(attachment, media, cx))
            });
        if let Some(video) = &video {
            cx.observe(video, |_, _, cx| cx.notify()).detach();
        }
        Self {
            focus: cx.focus_handle(),
            media,
            items,
            index,
            video,
            zoom: 1.,
            offset: point(0., 0.),
            press: None,
            stage: Rc::default(),
        }
    }

    fn selected(&self) -> Option<&Attachment> {
        self.items.get(self.index)
    }

    fn expanded(&self, cx: &App) -> bool {
        self.video
            .as_ref()
            .is_some_and(|video| video.read(cx).expanded)
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
            return;
        }
        match (keystroke.key.as_str(), &self.video) {
            ("escape", Some(video)) if video.read(cx).expanded => {
                video.update(cx, |video, cx| video.toggle_expanded(cx));
            }
            // Closing reads this view, which is busy handling the key.
            ("escape", _) => window.defer(cx, close),
            ("left", Some(video)) => video.update(cx, |v, cx| v.seek_by(-SKIP_SECONDS, cx)),
            ("right", Some(video)) => video.update(cx, |v, cx| v.seek_by(SKIP_SECONDS, cx)),
            ("space", Some(video)) if !event.is_held => video.update(cx, |v, cx| v.toggle(cx)),
            ("left", None) if self.index > 0 => self.choose(self.index - 1, cx),
            ("right", None) if self.index + 1 < self.items.len() => self.choose(self.index + 1, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.items.len() && index != self.index {
            self.index = index;
            self.zoom = 1.;
            self.offset = point(0., 0.);
            self.press = None;
            cx.notify();
        }
    }

    /// Display scale of the fitted image; 1 is its own pixels.
    fn fit(&self, natural: Size<f32>) -> Option<f32> {
        let stage = self.stage.get().size;
        let (width, height) = (f32::from(stage.width), f32::from(stage.height));
        (natural.width > 0. && natural.height > 0. && width > 0. && height > 0.)
            .then(|| (width / natural.width).min(height / natural.height))
    }

    fn max_zoom(&self, natural: Option<Size<f32>>) -> f32 {
        natural
            .and_then(|n| self.fit(n))
            .map_or(BASE_MAX_ZOOM, |fit| BASE_MAX_ZOOM.max(2. / fit))
    }

    /// Keeps the image covering the stage wherever it is larger than it.
    fn clamp_offset(
        &self,
        zoom: f32,
        offset: Point<f32>,
        natural: Option<Size<f32>>,
    ) -> Point<f32> {
        let Some((natural, fit)) = natural.and_then(|n| Some((n, self.fit(n)?))) else {
            return point(0., 0.);
        };
        let stage = self.stage.get().size;
        let limit_x = ((natural.width * fit * zoom - f32::from(stage.width)) / 2.).max(0.);
        let limit_y = ((natural.height * fit * zoom - f32::from(stage.height)) / 2.).max(0.);
        point(
            offset.x.clamp(-limit_x, limit_x),
            offset.y.clamp(-limit_y, limit_y),
        )
    }

    /// `zoomTo`: zooms about `anchor` (from the stage centre), or about the
    /// current offset.
    fn zoom_to(
        &mut self,
        value: f32,
        anchor: Option<Point<f32>>,
        natural: Option<Size<f32>>,
        cx: &mut Context<Self>,
    ) {
        if !value.is_finite() {
            return;
        }
        let zoom = value.clamp(MIN_ZOOM, self.max_zoom(natural));
        let ratio = zoom / self.zoom;
        let offset = match anchor {
            Some(anchor) => point(
                anchor.x - (anchor.x - self.offset.x) * ratio,
                anchor.y - (anchor.y - self.offset.y) * ratio,
            ),
            None => self.offset,
        };
        self.zoom = zoom;
        self.offset = self.clamp_offset(zoom, offset, natural);
        cx.notify();
    }

    /// From a window position to an offset from the stage centre.
    fn stage_point(&self, position: Point<Pixels>) -> Point<f32> {
        let stage = self.stage.get();
        let center = stage.center();
        point(
            f32::from(position.x - center.x),
            f32::from(position.y - center.y),
        )
    }

    fn natural(&self, window: &mut Window, cx: &mut App) -> Option<Size<f32>> {
        let selected = self.selected()?;
        natural_size(&selected.url, &self.media, window, cx).or_else(|| {
            selected
                .dimensions
                .map(|d| size(d.width as f32, d.height as f32))
        })
    }

    fn image_stage(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let c = *cx.colors();
        let Some(selected) = self.selected().cloned() else {
            return unavailable("Image unavailable").into_any_element();
        };
        let remote = Remote {
            url: selected.url.clone().into(),
            fallback: None,
            media: self.media.clone(),
        };
        let image = decoded(&remote, window, cx);
        let natural = self.natural(window, cx);
        let fit = natural.and_then(|n| self.fit(n));
        let stage_size = self.stage.get().size;
        let max_zoom = self.max_zoom(natural);
        let pannable = self.zoom > 1.;
        let next_zoom = ZOOM_PRESETS
            .into_iter()
            .find(|preset| *preset > self.zoom)
            .unwrap_or(MIN_ZOOM);
        let count = self.items.len();
        let picture = match (image, natural.zip(fit)) {
            (Some(Err(_)), _) => unavailable("Image unavailable").into_any_element(),
            (Some(Ok(image)), Some((natural, fit))) => {
                let (width, height) = (
                    natural.width * fit * self.zoom,
                    natural.height * fit * self.zoom,
                );
                let left = (f32::from(stage_size.width) - width) / 2. + self.offset.x;
                let top = (f32::from(stage_size.height) - height) / 2. + self.offset.y;
                img(image)
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .into_any_element()
            }
            _ => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_body_sm()
                .text_color(c.text_subtle)
                .child("Loading media…")
                .into_any_element(),
        };
        let relay = self.media.is_relay_media(&selected.url);
        let stage_bounds = self.stage.clone();
        let download = {
            let (url, name, media) = (
                selected.url.clone(),
                selected.name.clone(),
                self.media.clone(),
            );
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                super::download(url.clone(), name.clone(), media.clone(), cx)
            }
        };
        let browser_url = selected.url.clone();
        div()
            .id("image-stage")
            .group("review-stage")
            .relative()
            .size_full()
            .overflow_hidden()
            .cursor(if self.press.is_some() && pannable {
                CursorStyle::ClosedHand
            } else if pannable {
                CursorStyle::OpenHand
            } else {
                CursorStyle::PointingHand
            })
            .child(
                canvas(
                    move |bounds, window, _| {
                        if stage_bounds.replace(bounds) != bounds {
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0()
                .size_full(),
            )
            .child(picture)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.press = Some(Press {
                        origin: event.position,
                        offset: this.offset,
                        click: true,
                    });
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                let Some(press) = this.press.as_mut() else {
                    return;
                };
                if !event.dragging() {
                    this.press = None;
                    cx.notify();
                    return;
                }
                let delta = event.position - press.origin;
                let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                if dx.hypot(dy) > CLICK_SLOP {
                    press.click = false;
                }
                if this.zoom > 1. {
                    let offset = point(press.offset.x + dx, press.offset.y + dy);
                    this.offset = this.clamp_offset(this.zoom, offset, natural);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    let Some(press) = this.press.take() else {
                        return;
                    };
                    cx.notify();
                    if !press.click {
                        return;
                    }
                    // Click zoom: fit goes to actual pixels for downscaled
                    // images, and at least 2x otherwise; any other zoom
                    // returns to fit.
                    if this.zoom != 1. {
                        this.zoom_to(1., None, natural, cx);
                    } else {
                        let target = fit.map_or(2., |fit| (1. / fit).max(2.));
                        let anchor = this.stage_point(event.position);
                        this.zoom_to(target, Some(anchor), natural, cx);
                    }
                }),
            )
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, window, cx| {
                    let delta = event.delta.pixel_delta(window.line_height());
                    let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
                    if event.modifiers.control {
                        let anchor = this.stage_point(event.position);
                        this.zoom_to(this.zoom * (dy * 0.01).exp(), Some(anchor), natural, cx);
                    } else if this.zoom > 1. {
                        let offset = point(this.offset.x + dx, this.offset.y + dy);
                        this.offset = this.clamp_offset(this.zoom, offset, natural);
                        cx.notify();
                    }
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(16.))
                    .flex()
                    .justify_center()
                    .opacity(0.)
                    .group_hover("review-stage", |s| s.opacity(1.))
                    .child(
                        div()
                            .id("image-controls")
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_center()
                            .gap(px(12.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(32.))
                            .bg(c.bg_media_glass)
                            .border_1()
                            .border_color(c.border_media_glass)
                            .text_color(c.text_on_media)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .when(count > 1, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(4.))
                                        .child(
                                            IconButton::new("previous", IconName::CaretLeft)
                                                .size(IconButtonSize::Sm)
                                                .tooltip("Previous image")
                                                .disabled(self.index == 0)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if this.index > 0 {
                                                        this.choose(this.index - 1, cx);
                                                    }
                                                })),
                                        )
                                        .child(
                                            div()
                                                .min_w(px(45.))
                                                .flex()
                                                .justify_center()
                                                .text_caption()
                                                .font_features(super::tabular())
                                                .child(format!("{} / {count}", self.index + 1)),
                                        )
                                        .child(
                                            IconButton::new("next", IconName::CaretRight)
                                                .size(IconButtonSize::Sm)
                                                .tooltip("Next image")
                                                .disabled(self.index + 1 >= count)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.choose(this.index + 1, cx);
                                                })),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(
                                        IconButton::new("zoom-out", IconName::Minus)
                                            .size(IconButtonSize::Sm)
                                            .tooltip("Zoom out")
                                            .disabled(self.zoom <= MIN_ZOOM)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.zoom_to(
                                                    this.zoom - ZOOM_STEP,
                                                    None,
                                                    natural,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .child(
                                        Button::new(
                                            "zoom",
                                            format!("{}%", (self.zoom * 100.).round()),
                                        )
                                        .variant(ButtonVariant::Ghost)
                                        .size(ButtonSize::Sm)
                                        .tooltip(format!("Zoom to {}%", next_zoom * 100.))
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.zoom_to(next_zoom, None, natural, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        IconButton::new("zoom-in", IconName::Plus)
                                            .size(IconButtonSize::Sm)
                                            .tooltip("Zoom in")
                                            .disabled(self.zoom >= max_zoom)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.zoom_to(
                                                    this.zoom + ZOOM_STEP,
                                                    None,
                                                    natural,
                                                    cx,
                                                )
                                            })),
                                    ),
                            )
                            .when(relay, |this| {
                                this.child(
                                    IconButton::new("download", IconName::Download)
                                        .size(IconButtonSize::Sm)
                                        .tooltip("Download image")
                                        .on_click(download),
                                )
                            })
                            .when(!relay, |this| {
                                this.child(
                                    IconButton::new("browser", IconName::ArrowSquareOut)
                                        .size(IconButtonSize::Sm)
                                        .tooltip("Open image in browser")
                                        .on_click(move |_, _, cx| cx.open_url(&browser_url)),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

fn unavailable(text: &'static str) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_body_sm()
        .child(text)
}

impl Focusable for Review {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Review {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = *cx.colors();
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let expanded = self.expanded(cx);
        let (width, height) = if expanded {
            (vw, vh)
        } else {
            (
                (vw - 2. * BACKDROP_PADDING).clamp(0., MAX_SIZE.0),
                (vh - 2. * BACKDROP_PADDING).clamp(0., MAX_SIZE.1),
            )
        };
        let title: SharedString = match (self.selected(), &self.video) {
            (Some(video), Some(_)) => video.name.clone().unwrap_or_else(|| "Video".into()).into(),
            _ => "Image".into(),
        };
        let stage = match &self.video {
            Some(video) => video.clone().into_any_element(),
            None => self.image_stage(window, cx),
        };
        div()
            .id("media-review")
            .track_focus(&self.focus)
            .key_context("MediaReview")
            .on_key_down(cx.listener(Self::key))
            .occlude()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(c.bg_scrim)
            .text_color(c.text_standard)
            .on_mouse_down(MouseButton::Left, |_, window, cx| close(window, cx))
            .child(
                div()
                    .id("review-frame")
                    .w(px(width))
                    .h(px(height))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .bg(c.surface_panel)
                    .when(!expanded, |this| {
                        this.rounded(px(16.))
                            .border_1()
                            .border_color(c.affordance_subtle_hover)
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(!expanded, |this| {
                        this.child(
                            PanelHeader::new().title(title).action(
                                IconButton::new("close", IconName::X)
                                    .size(IconButtonSize::Sm)
                                    .tooltip("Close fullscreen viewer")
                                    .on_click(|_, window, cx| close(window, cx)),
                            ),
                        )
                    })
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .bg(c.bg_media_canvas)
                            .child(stage),
                    ),
            )
    }
}
