//! Audio and video decoding with GStreamer, which Ubuntu ships with the
//! codecs attachments use (MP4/H.264/AAC, WebM, Ogg/Opus, MP3, WAV).
//!
//! Media bytes arrive whole through the authenticated [`Media`] client, so
//! a `playbin` reads them from memory through a random-access `appsrc`.
//! Video frames come out of an `appsink` as BGRA, the layout GPUI's
//! `RenderImage` holds. Everything here is synchronous; the UI polls.
//!
//! [`Media`]: buzz_relay_client::Media

use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::*;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Bytes pushed per `need-data` request at most.
const CHUNK: usize = 256 * 1024;
/// How long a poster frame may take to decode.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) fn init() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|error| error.to_string()))
        .clone()
}

/// One decoded video frame, tightly packed BGRA.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Audio,
    Video,
    /// Video without sound, paused on its first frame.
    Poster,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// Prerolled: duration and seeking are available.
    Ready,
    Ended,
    Failed(String),
}

type Slot = Arc<Mutex<Option<Frame>>>;

pub struct Engine {
    playbin: gst::Element,
    bus: gst::Bus,
    frame: Option<Slot>,
    ready: bool,
    pending_seek: Option<f64>,
    rate: f64,
}

impl Engine {
    /// A paused pipeline over `bytes`. Errors (a missing decoder, a
    /// corrupt file) arrive later as [`Event::Failed`].
    pub fn new(bytes: Arc<[u8]>, kind: Kind) -> Result<Self, String> {
        init()?;
        let playbin = gst::ElementFactory::make("playbin")
            .property("uri", "appsrc://")
            .build()
            .map_err(|error| error.to_string())?;
        playbin.connect("source-setup", false, move |values| {
            if let Some(source) = values
                .get(1)
                .and_then(|value| value.get::<gst::Element>().ok())
                .and_then(|element| element.downcast::<gst_app::AppSrc>().ok())
            {
                feed(&source, bytes.clone());
            }
            None
        });
        playbin.set_property_from_str(
            "flags",
            match kind {
                Kind::Audio => "audio+soft-volume",
                Kind::Video => "video+audio+soft-volume",
                Kind::Poster => "video",
            },
        );
        let frame = (kind != Kind::Audio).then(|| {
            let slot = Slot::default();
            playbin.set_property("video-sink", video_sink(slot.clone()));
            slot
        });
        if kind != Kind::Poster {
            playbin.set_property("audio-sink", audio_sink());
        }
        let bus = playbin.bus().ok_or("The player has no bus")?;
        playbin
            .set_state(gst::State::Paused)
            .map_err(|_| "This media cannot be played".to_string())?;
        Ok(Self {
            playbin,
            bus,
            frame,
            ready: false,
            pending_seek: None,
            rate: 1.,
        })
    }

    /// Drains the pipeline's messages.
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(message) = self.bus.pop() {
            if let Some(event) = self.handle(&message) {
                events.push(event);
            }
        }
        events
    }

    fn handle(&mut self, message: &gst::Message) -> Option<Event> {
        use gst::MessageView;
        match message.view() {
            MessageView::Eos(_) => Some(Event::Ended),
            MessageView::Error(error) => {
                let detail = error.debug().map(|d| d.to_string()).unwrap_or_default();
                tracing::warn!(error = %error.error(), %detail, "media playback failed");
                Some(Event::Failed(error.error().to_string()))
            }
            MessageView::StateChanged(change)
                if !self.ready
                    && message.src() == Some(self.playbin.upcast_ref())
                    && change.current() >= gst::State::Paused =>
            {
                self.ready = true;
                // A speed chosen before preroll applies through a seek too.
                let pending = self.pending_seek.take();
                if pending.is_some() || self.rate != 1. {
                    self.seek(pending.unwrap_or(0.));
                }
                Some(Event::Ready)
            }
            _ => None,
        }
    }

    pub fn play(&self) {
        let _ = self.playbin.set_state(gst::State::Playing);
    }

    pub fn pause(&self) {
        let _ = self.playbin.set_state(gst::State::Paused);
    }

    pub fn position(&self) -> Option<f64> {
        self.playbin
            .query_position::<gst::ClockTime>()
            .map(gst::ClockTime::seconds_f64)
    }

    pub fn duration(&self) -> Option<f64> {
        self.playbin
            .query_duration::<gst::ClockTime>()
            .map(gst::ClockTime::seconds_f64)
            .filter(|seconds| seconds.is_finite() && *seconds > 0.)
    }

    pub fn seek(&mut self, seconds: f64) {
        if !self.ready {
            self.pending_seek = Some(seconds);
            return;
        }
        let at = gst::ClockTime::from_seconds_f64(seconds.max(0.));
        let flags = gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE;
        let _ = self.playbin.seek(
            self.rate,
            flags,
            gst::SeekType::Set,
            at,
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
    }

    /// Playback speed, kept across seeks.
    pub fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
        if self.ready
            && let Some(position) = self.position()
        {
            self.seek(position);
        }
    }

    /// Linear 0..=1.
    pub fn set_volume(&self, volume: f64) {
        self.playbin.set_property("volume", volume.clamp(0., 1.));
    }

    pub fn set_muted(&self, muted: bool) {
        self.playbin.set_property("mute", muted);
    }

    /// The newest frame not taken yet.
    pub fn take_frame(&self) -> Option<Frame> {
        self.frame.as_ref()?.lock().ok()?.take()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.playbin.set_state(gst::State::Null);
    }
}

/// What a video looks like before it plays: its first frame and length.
pub struct Probe {
    pub frame: Option<Frame>,
    pub duration: Option<f64>,
}

/// Decodes the first frame of a video. Blocks; run it off the UI thread.
pub fn probe(bytes: Arc<[u8]>) -> Result<Probe, String> {
    let mut engine = Engine::new(bytes, Kind::Poster)?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    while !engine.ready {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("Timed out decoding the video".into());
        }
        let wait = gst::ClockTime::from_mseconds(left.as_millis().min(200) as u64);
        if let Some(message) = engine.bus.timed_pop(wait) {
            match engine.handle(&message) {
                Some(Event::Failed(error)) => return Err(error),
                Some(Event::Ended) => break,
                _ => {}
            }
        }
    }
    Ok(Probe {
        frame: engine.take_frame(),
        duration: engine.duration(),
    })
}

/// A byte range of the shared body, so pushing never copies it.
struct Chunk(Arc<[u8]>, Range<usize>);

impl AsRef<[u8]> for Chunk {
    fn as_ref(&self) -> &[u8] {
        &self.0[self.1.clone()]
    }
}

/// Serves `bytes` to a random-access `appsrc`, so demuxers can seek (MP4
/// files often keep their index at the end).
fn feed(source: &gst_app::AppSrc, bytes: Arc<[u8]>) {
    let size = bytes.len();
    let offset = Arc::new(AtomicU64::new(0));
    source.set_stream_type(gst_app::AppStreamType::RandomAccess);
    source.set_size(size as i64);
    source.set_format(gst::Format::Bytes);
    let seek_offset = offset.clone();
    source.set_callbacks(
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |source, length| {
                let start = (offset.load(Ordering::SeqCst) as usize).min(size);
                if start >= size {
                    let _ = source.end_of_stream();
                    return;
                }
                let want = if length == 0 || length == u32::MAX {
                    CHUNK
                } else {
                    (length as usize).min(CHUNK)
                };
                let end = (start + want).min(size);
                let mut buffer = gst::Buffer::from_slice(Chunk(bytes.clone(), start..end));
                if let Some(buffer) = buffer.get_mut() {
                    buffer.set_offset(start as u64);
                }
                offset.store(end as u64, Ordering::SeqCst);
                let _ = source.push_buffer(buffer);
            })
            .seek_data(move |_, position| {
                seek_offset.store(position, Ordering::SeqCst);
                true
            })
            .build(),
    );
}

fn video_sink(slot: Slot) -> gst_app::AppSink {
    let caps = gst_video::VideoCapsBuilder::new()
        .format(gst_video::VideoFormat::Bgra)
        .pixel_aspect_ratio(gst::Fraction::new(1, 1))
        .build();
    let sink = gst_app::AppSink::builder()
        .caps(&caps)
        .max_buffers(1)
        .drop(true)
        .build();
    let preroll = slot.clone();
    sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_preroll(move |sink| {
                let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Eos)?;
                store(&preroll, &sample);
                Ok(gst::FlowSuccess::Ok)
            })
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                store(&slot, &sample);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    sink
}

fn store(slot: &Slot, sample: &gst::Sample) {
    if let Some(frame) = frame(sample)
        && let Ok(mut slot) = slot.lock()
    {
        *slot = Some(frame);
    }
}

fn frame(sample: &gst::Sample) -> Option<Frame> {
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(sample.buffer()?, &info).ok()?;
    let (width, height) = (frame.width(), frame.height());
    let stride = *frame.plane_stride().first()? as usize;
    let data = frame.plane_data(0).ok()?;
    let row = width as usize * 4;
    let mut bgra = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        bgra.extend_from_slice(data.get(y * stride..y * stride + row)?);
    }
    Some(Frame {
        width,
        height,
        bgra,
    })
}

/// The desktop's audio output, or a clock-synced null sink when there is
/// none, so playback still advances (and video still plays) without sound.
fn audio_sink() -> gst::Element {
    if let Ok(sink) = gst::ElementFactory::make("autoaudiosink").build()
        && sink.set_state(gst::State::Ready).is_ok()
    {
        let _ = sink.set_state(gst::State::Null);
        return sink;
    }
    gst::ElementFactory::make("fakesink")
        .property("sync", true)
        .build()
        .expect("fakesink is in GStreamer core")
}
