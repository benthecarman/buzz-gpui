//! Video: the inline preview (`MediaAttachment.tsx`, ui.md §4.7) and the
//! player inside the media viewer (`VideoPlayer.tsx`).

use super::engine::{self, Engine, Event, Kind};
use super::seek::{OnSeek, seek_area};
use super::{Lazy, Remote, render_image, tabular, video_time};
use crate::ui::overlay::{self, Align};
use buzz_relay_client::Media;
use buzz_relay_client::fold::Attachment;
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, IconButton, IconButtonSize, IconName, Menu,
    MenuItem, TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Frame polling while the viewer is open.
const TICK: Duration = Duration::from_millis(16);
/// Speeds the menu offers, fastest first (`speeds`).
const SPEEDS: [f64; 8] = [2., 1.75, 1.5, 1.25, 1., 0.75, 0.5, 0.25];
/// Arrow keys skip this far (`seekVideoBy`).
pub const SKIP_SECONDS: f64 = 10.;

/// A video's first frame and length, decoded once per URL for previews
/// that have no signed poster.
struct Probed {
    poster: Option<Arc<RenderImage>>,
    size: Option<Size<f32>>,
    duration: Option<f64>,
}

enum VideoProbe {}

impl Asset for VideoProbe {
    type Source = Remote;
    type Output = Result<Arc<Probed>, SharedString>;

    #[allow(clippy::manual_async_fn)]
    fn load(source: Remote, _: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            let bytes = source
                .media
                .fetch(&source.url)
                .await
                .map_err(|error| SharedString::from(error.to_string()))?;
            let (sender, receiver) = futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let _ = sender.send(engine::probe(bytes));
            });
            let probe = receiver
                .await
                .map_err(|_| SharedString::from("The video decoder stopped"))??;
            let size = probe
                .frame
                .as_ref()
                .map(|frame| size(frame.width as f32, frame.height as f32));
            Ok(Arc::new(Probed {
                poster: probe
                    .frame
                    .and_then(|frame| render_image(frame.width, frame.height, frame.bgra)),
                size,
                duration: probe.duration,
            }))
        }
    }
}

/// Opens the media viewer on a video.
pub type OpenReview = Rc<dyn Fn(&mut Window, &mut App)>;

/// The inline video preview: poster, centred play button, an expand
/// button on hover and the length in a chip. Playing opens the viewer.
pub fn video_preview(
    id: impl Into<ElementId>,
    attachment: &Attachment,
    media: &Media,
    open: OpenReview,
) -> impl IntoElement {
    let attachment = attachment.clone();
    let media = media.clone();
    Lazy::new(id, move |window, cx| {
        let c = *cx.colors();
        let dims = attachment
            .dimensions
            .map(|d| size(d.width as f32, d.height as f32));
        let probed =
            (attachment.preview_url.is_none() || dims.is_none() || attachment.duration.is_none())
                .then(|| {
                    let remote = Remote {
                        url: attachment.url.clone().into(),
                        fallback: None,
                        media: media.clone(),
                    };
                    window.use_asset::<VideoProbe>(&remote, cx)
                })
                .flatten()
                .and_then(Result::ok);
        let natural = dims.or(probed.as_ref().and_then(|p| p.size));
        let ratio = natural
            .filter(|s| s.width > 0. && s.height > 0.)
            .map_or(16. / 9., |s| s.width / s.height);
        // `min(24rem, 100%, ratio × 11.25rem)`, height from the ratio.
        let width = (ratio * 180.).min(384.);
        let duration = attachment
            .duration
            .or(probed.as_ref().and_then(|p| p.duration));
        let poster: Option<ImageSource> = match &attachment.preview_url {
            Some(url) => Some(super::image(url, &media)),
            None => probed
                .as_ref()
                .and_then(|p| p.poster.clone())
                .map(ImageSource::from),
        };
        let open_click = open.clone();
        div()
            .id("video")
            .group("video-preview")
            .relative()
            .w(px(width))
            .h(px(width / ratio))
            .max_w_full()
            .my(px(4.))
            .rounded(px(16.))
            .overflow_hidden()
            .bg(c.surface_inverse)
            .cursor_pointer()
            .on_click(move |_, window, cx| open_click(window, cx))
            .when_some(poster, |this, poster| {
                this.child(
                    img(poster)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        IconButton::new("play", IconName::PlayFilled)
                            .variant(ButtonVariant::Media)
                            .size(IconButtonSize::Md)
                            .tooltip("Play video"),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(px(8.))
                    .right(px(8.))
                    .opacity(0.)
                    .group_hover("video-preview", |s| s.opacity(1.))
                    .child(
                        IconButton::new("expand", IconName::ArrowsOut)
                            .variant(ButtonVariant::Media)
                            .size(IconButtonSize::Sm)
                            .tooltip("Open video fullscreen"),
                    ),
            )
            .when_some(duration, |this, duration| {
                this.child(
                    div()
                        .absolute()
                        .left(px(12.))
                        .bottom(px(12.))
                        .px(px(8.))
                        .py(px(4.))
                        .rounded_full()
                        .bg(c.surface_inverse)
                        .text_color(c.text_inverse)
                        .text_caption()
                        .font_features(tabular())
                        .child(video_time(duration)),
                )
            })
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(16.))
                    .border_1()
                    .border_color(c.border_image),
            )
            .into_any_element()
    })
}

enum State {
    Loading,
    Ready,
    Failed,
}

/// The viewer's video player (`VideoPlayer`): decoded frames, then a
/// media-glass control bar with play, volume, time, seek bar, length,
/// speed and an expand toggle that gives the video the whole window.
pub struct VideoPlayer {
    attachment: Attachment,
    media: Media,
    engine: Option<Engine>,
    bytes: Option<Arc<[u8]>>,
    state: State,
    playing: bool,
    ended: bool,
    position: f64,
    duration: Option<f64>,
    volume: f32,
    muted: bool,
    rate: f64,
    speed_menu: bool,
    pub expanded: bool,
    frame: Option<Arc<RenderImage>>,
    /// Frames replaced since the last draw, to free from the atlas.
    retired: Vec<Arc<RenderImage>>,
    /// The player's bounds as last drawn.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _task: Task<()>,
}

impl VideoPlayer {
    pub fn new(attachment: Attachment, media: Media, cx: &mut Context<Self>) -> Self {
        let (url, fetch_media) = (attachment.url.clone(), media.clone());
        let task = cx.spawn(async move |this, cx| {
            let loaded = match fetch_media.fetch(&url).await {
                Ok(bytes) => {
                    let decoder = bytes.clone();
                    cx.background_executor()
                        .spawn(async move { Engine::new(decoder, Kind::Video) })
                        .await
                        .map(|engine| (engine, bytes))
                }
                Err(error) => Err(error.to_string()),
            };
            if !matches!(
                this.update(cx, |this, cx| this.loaded(loaded, cx)),
                Ok(true)
            ) {
                return;
            }
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            attachment,
            media,
            engine: None,
            bytes: None,
            state: State::Loading,
            playing: true,
            ended: false,
            position: 0.,
            duration: None,
            volume: 1.,
            muted: false,
            rate: 1.,
            speed_menu: false,
            expanded: false,
            frame: None,
            retired: Vec::new(),
            bounds: Rc::default(),
            _task: task,
        }
    }

    fn loaded(
        &mut self,
        loaded: Result<(Engine, Arc<[u8]>), String>,
        cx: &mut Context<Self>,
    ) -> bool {
        cx.notify();
        match loaded {
            Ok((engine, bytes)) => {
                // `autoPlay`.
                engine.play();
                self.engine = Some(engine);
                self.bytes = Some(bytes);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "video unavailable");
                self.state = State::Failed;
                false
            }
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let mut changed = false;
        for event in engine.poll() {
            changed = true;
            match event {
                Event::Ready => self.state = State::Ready,
                Event::Ended => {
                    self.playing = false;
                    self.ended = true;
                    engine.pause();
                }
                Event::Failed(_) => {
                    self.state = State::Failed;
                    self.engine = None;
                    cx.notify();
                    return;
                }
            }
        }
        if let Some(frame) = engine.take_frame()
            && let Some(image) = render_image(frame.width, frame.height, frame.bgra)
        {
            self.retired.extend(self.frame.replace(image));
            changed = true;
        }
        if self.duration.is_none() {
            self.duration = engine.duration();
        }
        if let Some(position) = engine.position()
            && (position - self.position).abs() > 0.01
            && !self.ended
        {
            self.position = position;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        if self.playing {
            engine.pause();
        } else {
            if self.ended {
                engine.seek(0.);
                self.position = 0.;
                self.ended = false;
            }
            engine.play();
        }
        self.playing = !self.playing;
        cx.notify();
    }

    pub fn seek_to(&mut self, seconds: f64, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let seconds = match self.duration {
            Some(duration) => seconds.clamp(0., duration),
            None => seconds.max(0.),
        };
        engine.seek(seconds);
        self.position = seconds;
        self.ended = false;
        cx.notify();
    }

    pub fn seek_by(&mut self, delta: f64, cx: &mut Context<Self>) {
        self.seek_to(self.position + delta, cx);
    }

    fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0., 1.);
        self.muted = false;
        if let Some(engine) = &self.engine {
            engine.set_volume(self.volume as f64);
            engine.set_muted(false);
        }
        cx.notify();
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        if self.muted || self.volume == 0. {
            self.muted = false;
            if self.volume == 0. {
                self.volume = 1.;
            }
        } else {
            self.muted = true;
        }
        if let Some(engine) = &self.engine {
            engine.set_volume(self.volume as f64);
            engine.set_muted(self.muted);
        }
        cx.notify();
    }

    fn set_rate(&mut self, rate: f64, cx: &mut Context<Self>) {
        self.rate = rate;
        self.speed_menu = false;
        if let Some(engine) = self.engine.as_mut() {
            engine.set_rate(rate);
        }
        cx.notify();
    }

    /// Every frame still held, for the closing viewer to free.
    pub fn take_frames(&mut self) -> Vec<Arc<RenderImage>> {
        let mut frames = std::mem::take(&mut self.retired);
        frames.extend(self.frame.take());
        frames
    }

    pub fn toggle_expanded(&mut self, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        cx.notify();
    }

    /// Hands the video to the desktop's player, for codecs GStreamer here
    /// lacks.
    fn open_externally(&self, cx: &mut Context<Self>) {
        let (url, name, media, bytes) = (
            self.attachment.url.clone(),
            self.attachment.name.clone(),
            self.media.clone(),
            self.bytes.clone(),
        );
        cx.spawn(async move |_, cx| {
            let bytes = match bytes {
                Some(bytes) => bytes,
                None => match media.fetch(&url).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(%error, "could not fetch the video");
                        return;
                    }
                },
            };
            let path = std::env::temp_dir().join(external_name(name.as_deref(), &url));
            let written = cx
                .background_executor()
                .spawn({
                    let path = path.clone();
                    async move { std::fs::write(&path, &bytes) }
                })
                .await;
            match written {
                Ok(()) => cx.update(|cx| cx.open_with_system(&path)),
                Err(error) => tracing::warn!(%error, "could not save the video"),
            }
        })
        .detach();
    }

    fn controls(&self, cx: &mut Context<Self>, window: &Window) -> impl IntoElement {
        let c = *cx.colors();
        let progress = match self.duration {
            Some(duration) if duration > 0. => (self.position / duration).clamp(0., 1.) as f32,
            _ => 0.,
        };
        let player = cx.entity().downgrade();
        let seek: Option<OnSeek> = self.duration.map(|duration| {
            let player = player.clone();
            Rc::new(move |fraction: f32, _: &mut Window, cx: &mut App| {
                let _ = player.update(cx, |this, cx| this.seek_to(fraction as f64 * duration, cx));
            }) as OnSeek
        });
        let volume_seek: OnSeek = Rc::new(move |fraction: f32, _: &mut Window, cx: &mut App| {
            let _ = player.update(cx, |this, cx| this.set_volume(fraction, cx));
        });
        let audible = if self.muted { 0. } else { self.volume };
        let silent = audible == 0.;
        div()
            .id("controls")
            .absolute()
            .left(px(16.))
            .right(px(16.))
            .bottom(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(32.))
            .bg(c.bg_media_glass)
            .border_1()
            .border_color(c.border_media_glass)
            .text_color(c.text_on_media)
            .when(self.playing, |this| {
                this.opacity(0.)
                    .group_hover("video-player", |s| s.opacity(1.))
            })
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                IconButton::new(
                    "play",
                    if self.playing {
                        IconName::PauseFilled
                    } else {
                        IconName::PlayFilled
                    },
                )
                .size(IconButtonSize::Sm)
                .tooltip(if self.playing {
                    "Pause video"
                } else {
                    "Play video"
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle(cx))),
            )
            .child(
                IconButton::new(
                    "mute",
                    if silent {
                        IconName::SpeakerSlash
                    } else {
                        IconName::SpeakerHigh
                    },
                )
                .size(IconButtonSize::Sm)
                .tooltip(if silent { "Unmute video" } else { "Mute video" })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx))),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .w(px(64.))
                    .h(px(32.))
                    .child(super::audio::track_line(
                        audible,
                        c.bg_media_track,
                        c.text_standard,
                    ))
                    .child(seek_area("volume", Some(volume_seek))),
            )
            .child(
                div()
                    .flex_none()
                    .text_caption()
                    .font_features(tabular())
                    .child(video_time(self.position)),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w(px(20.))
                    .h(px(32.))
                    .child(super::audio::track_line(
                        progress,
                        c.bg_media_track,
                        c.text_standard,
                    ))
                    .child(seek_area("timeline", seek)),
            )
            .child(
                div()
                    .flex_none()
                    .text_caption()
                    .font_features(tabular())
                    .text_color(c.text_subtle)
                    .child(video_time(self.duration.unwrap_or(0.))),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .child(
                        Button::new("speed", format!("{}x", self.rate))
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Sm)
                            .tooltip(format!("Playback speed: {}x", self.rate))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.speed_menu = !this.speed_menu;
                                cx.notify();
                            })),
                    )
                    .when(self.speed_menu, |this| {
                        let menu = Menu::new().children(SPEEDS.iter().map(|speed| {
                            let speed = *speed;
                            MenuItem::new(
                                SharedString::from(format!("speed-{speed}")),
                                format!("{speed}x"),
                            )
                            .checked(speed == self.rate)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_rate(speed, cx)))
                        }));
                        let dismiss = cx.entity().downgrade();
                        this.child(overlay::dropdown(
                            window,
                            Align::End,
                            menu,
                            Rc::new(move |_, cx| {
                                let _ = dismiss.update(cx, |this, cx| {
                                    this.speed_menu = false;
                                    cx.notify();
                                });
                            }),
                        ))
                    }),
            )
            .child(
                IconButton::new(
                    "expand",
                    if self.expanded {
                        IconName::ArrowsIn
                    } else {
                        IconName::ArrowsOut
                    },
                )
                .size(IconButtonSize::Sm)
                .tooltip(if self.expanded {
                    "Exit full window"
                } else {
                    "Fill the window"
                })
                .on_click(cx.listener(|this, _, _, cx| this.toggle_expanded(cx))),
            )
    }
}

impl Render for VideoPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
        let c = *cx.colors();
        let failed = matches!(self.state, State::Failed);
        let poster = self
            .frame
            .is_none()
            .then(|| {
                self.attachment
                    .preview_url
                    .as_ref()
                    .map(|url| super::image(url, &self.media))
            })
            .flatten();
        div()
            .id("video-player")
            .group("video-player")
            .relative()
            .size_full()
            .bg(c.bg_media_canvas)
            .overflow_hidden()
            .text_color(c.text_standard)
            .when(!failed, |this| {
                this.on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                    // A click plays or pauses; a double click undoes that and
                    // skips back or ahead by the half it landed on.
                    this.toggle(cx);
                    if event.click_count() == 2
                        && let Some(position) = event.mouse_position()
                    {
                        let middle = this.bounds.get().center().x;
                        let delta = if position.x < middle {
                            -SKIP_SECONDS
                        } else {
                            SKIP_SECONDS
                        };
                        this.seek_by(delta, cx);
                    }
                }))
            })
            .child({
                let bounds = self.bounds.clone();
                canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                    .absolute()
                    .inset_0()
                    .size_full()
            })
            .when_some(self.frame.clone(), |this, frame| {
                this.child(
                    img(frame)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            })
            .when_some(poster, |this, poster| {
                this.child(
                    img(poster)
                        .absolute()
                        .inset_0()
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                )
            })
            .when(failed, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(12.))
                        .text_body_sm()
                        .child("Can't play this video here")
                        .child(
                            Button::new("open-externally", "Open externally")
                                .size(ButtonSize::Sm)
                                .icon(IconName::ArrowSquareOut)
                                .on_click(cx.listener(|this, _, _, cx| this.open_externally(cx))),
                        ),
                )
            })
            .when(
                matches!(self.state, State::Loading) && self.frame.is_none(),
                |this| {
                    this.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_body_sm()
                            .text_color(c.text_subtle)
                            .child("Loading media…"),
                    )
                },
            )
            .when(!failed, |this| this.child(self.controls(cx, window)))
    }
}

/// A temp file name keeping the attachment's extension, so the desktop
/// picks the right player.
fn external_name(name: Option<&str>, url: &str) -> String {
    let extension = name
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext)
        .or_else(|| {
            url.rsplit('/')
                .next()
                .and_then(|last| last.rsplit_once('.'))
                .map(|(_, ext)| ext)
        })
        .filter(|ext| {
            (1..=8).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .unwrap_or("mp4")
        .to_ascii_lowercase();
    let stem: String = url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    format!("buzz-video-{stem}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn names_external_copies_by_extension() {
        assert_eq!(
            external_name(
                Some("clip.WEBM"),
                "https://x/media/0123456789abcdef0123.mp4"
            ),
            "buzz-video-0123456789abcdef.webm"
        );
        assert_eq!(
            external_name(None, "https://x/media/abc.mov"),
            "buzz-video-abc.mov"
        );
        assert_eq!(
            external_name(Some("a/../b"), "https://x/"),
            "buzz-video-.mp4"
        );
    }
}
