//! Inline audio: the plain player (`AudioAttachment.tsx`) and the voice
//! note card (`bundled/voice-notes/VoiceNoteCard.tsx`).
//!
//! One clip plays at a time (`claimAudio`), so playback lives in a single
//! app-wide [`Audio`] entity. Rows read it while rendering, which makes
//! GPUI redraw them as it ticks.

use super::engine::{Engine, Event, Kind};
use super::seek::{OnSeek, seek_area};
use super::{media_time, tabular, video_time};
use buzz_relay_client::Media;
use buzz_relay_client::fold::Attachment;
use buzz_ui::{
    ActiveTheme, Button, ButtonSize, ButtonVariant, Icon, IconButton, IconButtonSize, IconName,
    TypeStyled,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

/// How often a playing clip reports its position.
const TICK: Duration = Duration::from_millis(50);
/// Voice note speeds, in the order the button cycles them.
const VOICE_RATES: [f64; 4] = [1., 1.5, 2., 0.5];
/// Voice note waveform bars: 3px wide with a 2px gap, as many as fit the
/// card's 210px track (`floor((width + 2) / 5)`).
const VOICE_BARS: usize = 42;

#[derive(Default)]
pub struct Audio {
    track: Option<Track>,
    /// Measured lengths, which win over the sender's `duration` hint.
    durations: HashMap<String, f64>,
    failed: HashSet<String>,
    rates: HashMap<String, f64>,
}

struct Track {
    url: String,
    voice: bool,
    engine: Option<Engine>,
    playing: bool,
    ended: bool,
    position: f64,
    _task: Task<()>,
}

struct AudioGlobal(Entity<Audio>);

impl Global for AudioGlobal {}

/// What a player shows for one clip.
#[derive(Clone, Copy, Default)]
struct Status {
    playing: bool,
    loading: bool,
    position: f64,
    duration: Option<f64>,
    failed: bool,
    rate: f64,
}

fn status(url: &str, tagged: Option<f64>, cx: &App) -> Status {
    let Some(audio) = cx.try_global::<AudioGlobal>() else {
        return Status {
            duration: tagged,
            rate: 1.,
            ..Status::default()
        };
    };
    let audio = audio.0.read(cx);
    let track = audio.track.as_ref().filter(|track| track.url == url);
    Status {
        playing: track.is_some_and(|track| track.playing),
        loading: track.is_some_and(|track| track.engine.is_none()),
        position: track.map_or(0., |track| track.position),
        duration: audio.durations.get(url).copied().or(tagged),
        failed: audio.failed.contains(url),
        rate: audio.rates.get(url).copied().unwrap_or(1.),
    }
}

fn audio(window: &mut Window, cx: &mut App) -> Entity<Audio> {
    if let Some(audio) = cx.try_global::<AudioGlobal>() {
        return audio.0.clone();
    }
    let audio = cx.new(|_| Audio::default());
    cx.set_global(AudioGlobal(audio.clone()));
    // Rows drawn before it existed do not watch it yet.
    window.refresh();
    audio
}

fn toggle(url: &str, voice: bool, media: &Media, window: &mut Window, cx: &mut App) {
    let (url, media) = (url.to_string(), media.clone());
    audio(window, cx).update(cx, |audio, cx| audio.toggle(url, voice, media, cx));
}

fn seek(url: &str, voice: bool, media: &Media, seconds: f64, window: &mut Window, cx: &mut App) {
    let (url, media) = (url.to_string(), media.clone());
    audio(window, cx).update(cx, |audio, cx| audio.seek(url, voice, media, seconds, cx));
}

impl Audio {
    fn toggle(&mut self, url: String, voice: bool, media: Media, cx: &mut Context<Self>) {
        match self.track.as_mut().filter(|track| track.url == url) {
            // Pressing again while it downloads cancels.
            Some(track) if track.engine.is_none() => self.track = None,
            Some(track) => {
                let engine = track.engine.as_mut().expect("loaded");
                if track.playing {
                    engine.pause();
                    track.playing = false;
                } else {
                    if track.ended {
                        engine.seek(0.);
                        track.position = 0.;
                        track.ended = false;
                    }
                    engine.play();
                    track.playing = true;
                }
            }
            None => self.start(url, voice, media, true, 0., cx),
        }
        cx.notify();
    }

    /// Seeks the playing clip, or loads a clip paused at `seconds`.
    fn seek(
        &mut self,
        url: String,
        voice: bool,
        media: Media,
        seconds: f64,
        cx: &mut Context<Self>,
    ) {
        match self.track.as_mut().filter(|track| track.url == url) {
            Some(track) => {
                track.position = seconds;
                track.ended = false;
                if let Some(engine) = track.engine.as_mut() {
                    engine.seek(seconds);
                }
            }
            None => self.start(url, voice, media, false, seconds, cx),
        }
        cx.notify();
    }

    fn cycle_rate(&mut self, url: &str, cx: &mut Context<Self>) {
        let rate = self.rates.get(url).copied().unwrap_or(1.);
        let next = VOICE_RATES
            .iter()
            .position(|r| *r == rate)
            .map_or(1., |i| VOICE_RATES[(i + 1) % VOICE_RATES.len()]);
        self.rates.insert(url.to_string(), next);
        if let Some(engine) = self
            .track
            .as_mut()
            .filter(|track| track.url == url)
            .and_then(|track| track.engine.as_mut())
        {
            engine.set_rate(next);
        }
        cx.notify();
    }

    fn start(
        &mut self,
        url: String,
        voice: bool,
        media: Media,
        play: bool,
        position: f64,
        cx: &mut Context<Self>,
    ) {
        self.failed.remove(&url);
        let task_url = url.clone();
        let task = cx.spawn(async move |this, cx| {
            let url = task_url;
            let engine = match media.fetch(&url).await {
                Ok(bytes) => {
                    cx.background_executor()
                        .spawn(async move { Engine::new(bytes, Kind::Audio) })
                        .await
                }
                Err(error) => Err(error.to_string()),
            };
            let loaded = this.update(cx, |this, cx| this.loaded(&url, engine, cx));
            if !matches!(loaded, Ok(true)) {
                return;
            }
            loop {
                cx.background_executor().timer(TICK).await;
                if !matches!(this.update(cx, |this, cx| this.tick(&url, cx)), Ok(true)) {
                    break;
                }
            }
        });
        // Dropping the previous track stops it.
        self.track = Some(Track {
            url,
            voice,
            engine: None,
            playing: play,
            ended: false,
            position,
            _task: task,
        });
    }

    fn loaded(
        &mut self,
        url: &str,
        engine: Result<Engine, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(track) = self.track.as_mut().filter(|track| track.url == url) else {
            return false;
        };
        cx.notify();
        match engine {
            Ok(mut engine) => {
                if let Some(rate) = self.rates.get(url) {
                    engine.set_rate(*rate);
                }
                if track.position > 0. {
                    engine.seek(track.position);
                }
                if track.playing {
                    engine.play();
                }
                track.engine = Some(engine);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "audio unavailable");
                self.failed.insert(url.to_string());
                self.track = None;
                false
            }
        }
    }

    /// Follows the clip; `false` once it is no longer the current one.
    fn tick(&mut self, url: &str, cx: &mut Context<Self>) -> bool {
        let Some(track) = self.track.as_mut().filter(|track| track.url == url) else {
            return false;
        };
        let Some(engine) = track.engine.as_mut() else {
            return true;
        };
        let before = (track.playing, track.position);
        for event in engine.poll() {
            match event {
                Event::Ready => {}
                Event::Ended => {
                    track.playing = false;
                    engine.pause();
                    if track.voice {
                        // The voice card resets to the start.
                        engine.seek(0.);
                        track.position = 0.;
                    } else {
                        track.ended = true;
                        track.position = engine
                            .duration()
                            .or(self.durations.get(url).copied())
                            .unwrap_or(track.position);
                    }
                }
                Event::Failed(_) => {
                    self.failed.insert(url.to_string());
                    self.track = None;
                    cx.notify();
                    return false;
                }
            }
        }
        if let Some(duration) = engine.duration() {
            self.durations.insert(url.to_string(), duration);
        }
        if track.playing
            && let Some(position) = engine.position()
        {
            track.position = position;
        }
        if before != (track.playing, track.position) {
            cx.notify();
        }
        true
    }
}

/// An inline audio player (`AudioAttachment`): play button, elapsed time,
/// seek bar and length in a pill.
pub fn audio_attachment(
    id: impl Into<ElementId>,
    attachment: &Attachment,
    media: &Media,
    cx: &App,
) -> AnyElement {
    let c = *cx.colors();
    let url = attachment.url.clone();
    let state = status(&url, attachment.duration, cx);
    if state.failed {
        return unavailable("Audio unavailable", &c).into_any_element();
    }
    let label = |verb: &str| match &attachment.name {
        Some(name) => format!("{verb} {name}"),
        None => format!("{verb} audio"),
    };
    let (play_url, play_media) = (url.clone(), media.clone());
    let seek_to: Option<OnSeek> = state.duration.map(|duration| {
        let (url, media) = (url.clone(), media.clone());
        Rc::new(move |fraction: f32, window: &mut Window, cx: &mut App| {
            seek(&url, false, &media, fraction as f64 * duration, window, cx)
        }) as OnSeek
    });
    let progress = progress(&state);
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .w(px(384.))
        .max_w_full()
        .mt(px(8.))
        .px(px(8.))
        .py(px(4.))
        .border_1()
        .border_color(c.border_standard)
        .rounded(px(32.))
        .bg(c.surface_inset)
        .text_color(c.text_standard)
        .child(
            IconButton::new(
                "play",
                if state.playing {
                    IconName::PauseFilled
                } else {
                    IconName::PlayFilled
                },
            )
            .size(IconButtonSize::Sm)
            .tooltip(label(if state.playing { "Pause" } else { "Play" }))
            .on_click(move |_, window, cx| toggle(&play_url, false, &play_media, window, cx)),
        )
        .child(
            div()
                .flex_none()
                .text_caption()
                .font_features(tabular())
                .child(video_time(state.position)),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_w(px(20.))
                .h(px(32.))
                .when(state.duration.is_none(), |this| this.opacity(0.4))
                .child(track_line(progress, c.border_prominent, c.text_standard))
                .child(seek_area("seek", seek_to)),
        )
        .child(
            div()
                .flex_none()
                .text_caption()
                .font_features(tabular())
                .text_color(c.text_subtle)
                .child(state.duration.map_or("—:—".into(), video_time)),
        )
        .into_any_element()
}

/// The voice note card (`VoiceNoteCard`): play button, waveform scrubber,
/// and the length, which turns into the speed button on hover.
pub fn voice_note(
    id: impl Into<ElementId>,
    attachment: &Attachment,
    media: &Media,
    cx: &App,
) -> AnyElement {
    let c = *cx.colors();
    let url = attachment.url.clone();
    let state = status(&url, attachment.duration, cx);
    let duration = state.duration.unwrap_or(0.);
    let heights = bar_heights(attachment.waveform.as_deref(), VOICE_BARS);
    let progress = progress(&state);
    let (play_url, play_media) = (url.clone(), media.clone());
    let seek_to: Option<OnSeek> = (duration > 0.).then(|| {
        let (url, media) = (url.clone(), media.clone());
        Rc::new(move |fraction: f32, window: &mut Window, cx: &mut App| {
            seek(&url, true, &media, fraction as f64 * duration, window, cx)
        }) as OnSeek
    });
    let bars = |color: Hsla| {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .gap(px(2.))
            .children(heights.iter().map(move |height| {
                div()
                    .flex_none()
                    .w(px(3.))
                    .h(px(*height))
                    .rounded_full()
                    .bg(color)
            }))
    };
    let (icon, label) = if state.failed {
        (IconName::WarningCircle, "Retry voice note")
    } else if state.loading {
        (IconName::CircleNotch, "Cancel loading voice note")
    } else if state.playing {
        (IconName::Pause, "Pause voice note")
    } else {
        (IconName::Play, "Play voice note")
    };
    let rate_url = url.clone();
    let rate = state.rate;
    div()
        .id(id)
        .group("voice-note")
        .relative()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(10.))
        .w(px(336.))
        .max_w_full()
        .my(px(4.))
        .px(px(10.))
        .py(px(8.))
        .border_1()
        .border_color(c.border_standard)
        .rounded(px(16.))
        .bg(c.surface_inset)
        .hover(|s| {
            s.border_color(c.border_prominent)
                .bg(c.affordance_panel_hover)
        })
        .text_color(c.text_standard)
        .child(
            IconButton::new("play", icon)
                .variant(ButtonVariant::Prominent)
                .size(IconButtonSize::Sm)
                .icon_size(18.)
                .tooltip(label)
                .on_click(move |_, window, cx| toggle(&play_url, true, &play_media, window, cx)),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .overflow_hidden()
                .child(bars(c.text_unavailable))
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .bottom_0()
                        .w(relative(progress))
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .w(px(210.))
                                .child(bars(c.text_standard)),
                        ),
                )
                .child(seek_area("seek", seek_to)),
        )
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(48.))
                .h(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_caption()
                        .font_features(tabular())
                        .text_color(c.text_subtle)
                        .group_hover("voice-note", |s| s.opacity(0.))
                        .child(media_time(if state.playing {
                            (duration - state.position).max(0.)
                        } else {
                            duration
                        })),
                )
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .opacity(0.)
                        .group_hover("voice-note", |s| s.opacity(1.))
                        .child(
                            Button::new(
                                "speed",
                                if rate == 0.5 {
                                    ".5×".to_string()
                                } else {
                                    format!("{rate}×")
                                },
                            )
                            .variant(ButtonVariant::Prominent)
                            .size(ButtonSize::Xs)
                            .full_width()
                            .tooltip("Playback speed")
                            .on_click(move |_, window, cx| {
                                audio(window, cx)
                                    .update(cx, |audio, cx| audio.cycle_rate(&rate_url, cx))
                            }),
                        ),
                ),
        )
        .when(state.failed, |this| {
            this.child(
                div()
                    .w_full()
                    .text_caption()
                    .text_color(c.text_danger)
                    .child("Audio unavailable. Retry playback."),
            )
        })
        .into_any_element()
}

fn progress(state: &Status) -> f32 {
    match state.duration {
        Some(duration) if duration > 0. => (state.position / duration).clamp(0., 1.) as f32,
        _ => 0.,
    }
}

/// A 2px track with its played part and a 2×16px thumb, centred in a
/// `relative` parent.
pub(super) fn track_line(progress: f32, rest: Hsla, played: Hsla) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(relative(0.5))
                .mt(px(-1.))
                .h(px(2.))
                .bg(rest),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .w(relative(progress))
                .top(relative(0.5))
                .mt(px(-1.))
                .h(px(2.))
                .bg(played),
        )
        .child(
            div()
                .absolute()
                .left(relative(progress))
                .ml(px(-1.))
                .top(relative(0.5))
                .mt(px(-8.))
                .w(px(2.))
                .h(px(16.))
                .rounded_full()
                .bg(played),
        )
}

/// `.attachmentUnavailable.audioUnavailable`.
fn unavailable(text: &'static str, c: &buzz_ui::Colors) -> Div {
    div().flex().mt(px(8.)).child(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .py(px(8.))
            .border_1()
            .border_color(c.border_warning)
            .rounded(px(12.))
            .bg(c.affordance_warning)
            .text_body_sm()
            .text_color(c.text_warning)
            .child(Icon::new(IconName::WarningCircle).size(16.))
            .child(text),
    )
}

/// Bar heights in pixels (`waveformPeaks` then `voiceNoteBarHeight`). The
/// signed waveform is 0..=100 levels; without one every bar is flat.
fn bar_heights(waveform: Option<&[u8]>, count: usize) -> Vec<f32> {
    let Some(samples) = waveform.filter(|w| !w.is_empty()) else {
        return vec![3.; count];
    };
    let peaks: Vec<f32> = (0..count)
        .map(|index| {
            let start = index * samples.len() / count;
            let end = ((index + 1) * samples.len() / count).max(start + 1);
            samples[start..end.min(samples.len())]
                .iter()
                .map(|s| *s as f32)
                .fold(0., f32::max)
        })
        .collect();
    let maximum = peaks.iter().copied().fold(0.001, f32::max);
    peaks
        .into_iter()
        .map(|peak| {
            let level = (peak / maximum).clamp(0.12, 1.);
            let audible = ((level - 0.16) / (1. - 0.16)).max(0.);
            3. + (audible * 17.).round()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn scales_waveform_bars_like_the_reference() {
        assert_eq!(bar_heights(None, 4), vec![3.; 4]);
        assert_eq!(
            bar_heights(Some(&[0, 50, 100, 25]), 4),
            vec![3., 10., 20., 5.]
        );
        // Fewer samples than bars repeat them; more are max-pooled.
        assert_eq!(bar_heights(Some(&[100]), 3), vec![20.; 3]);
        assert_eq!(bar_heights(Some(&[10, 100, 0, 0]), 2), vec![20., 3.]);
    }
}
