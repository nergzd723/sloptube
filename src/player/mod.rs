//! GStreamer-based video player.
//!
//! Video goes to `gtk4paintablesink`, which accepts DMABuf frames straight from
//! VA-API/V4L2 decoders, so frames reach GTK (and the compositor) without CPU
//! copies. MP4 streams are played through a small DASH manifest (see `dash`),
//! WebM ones as two plain HTTP streams (`uri` + `suburi`).
//!
//! Threading: the sink occasionally needs the GTK main thread (e.g. to query
//! DMABuf formats while a decoder negotiates). Anything that waits for the
//! streaming threads — state changes, flushing seeks — therefore runs on a
//! dedicated control thread, never on the main thread.

pub mod dash;
pub mod decoders;
pub mod formats;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use gst::prelude::*;
use gtk::glib::subclass::Signal;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "SlopPlayState")]
pub enum PlayState {
    #[default]
    Idle,
    Loading,
    Buffering,
    Paused,
    Playing,
    Ended,
    Error,
}

#[derive(Debug, Clone)]
pub enum Source {
    /// Separate video and audio streams.
    Separate { video: String, audio: String },
    /// One URI with everything (DASH manifest, muxed file, HLS, local file, audio only).
    Single { uri: String },
}

#[derive(Debug, Clone)]
pub struct LoadRequest {
    pub source: Source,
    pub headers: Vec<(String, String)>,
    pub start: Option<f64>,
    pub autoplay: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub video_decoder: Option<String>,
    pub audio_decoder: Option<String>,
    pub hardware: bool,
    pub dmabuf: bool,
    pub format: String,
    pub width: i32,
    pub height: i32,
    pub framerate: String,
    pub rendered: u64,
    pub dropped: u64,
}

// ---------------------------------------------------------------------------
// Control thread
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send>;

fn ctl_sender() -> &'static Mutex<mpsc::Sender<Job>> {
    static SENDER: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("sloptube-gst".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job();
                }
            })
            .expect("cannot start the playback control thread");
        Mutex::new(tx)
    })
}

/// Run a job on the control thread (in submission order).
fn ctl(job: impl FnOnce() + Send + 'static) {
    let _ = ctl_sender().lock().unwrap().send(Box::new(job));
}

/// Run a job on the control thread and get its result asynchronously.
fn ctl_await<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> tokio::sync::oneshot::Receiver<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    ctl(move || {
        let _ = tx.send(job());
    });
    rx
}

const LOAD_MARKER: &str = "sloptube-load";

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::Player)]
    pub struct Player {
        pub playbin: RefCell<Option<gst::Element>>,
        pub sink: RefCell<Option<gst::Element>>,
        #[property(get)]
        pub paintable: RefCell<Option<gdk::Paintable>>,
        #[property(get, builder(PlayState::default()))]
        pub state: Cell<PlayState>,
        /// Current position in seconds.
        #[property(get)]
        pub position: Cell<f64>,
        /// Duration in seconds (0 if unknown/live).
        #[property(get)]
        pub duration: Cell<f64>,
        #[property(get)]
        pub buffering: Cell<i32>,
        /// Perceptual volume 0..1 (cubic mapping to linear gain).
        #[property(get, set = Self::set_volume, minimum = 0.0, maximum = 1.0, default = 1.0)]
        pub volume: Cell<f64>,
        #[property(get, set = Self::set_muted)]
        pub muted: Cell<bool>,
        #[property(get, set = Self::set_rate, minimum = 0.25, maximum = 4.0, default = 1.0)]
        pub rate: Cell<f64>,
        #[property(get)]
        pub video_width: Cell<i32>,
        #[property(get)]
        pub video_height: Cell<i32>,
        #[property(get)]
        pub is_live: Cell<bool>,

        pub target_playing: Cell<bool>,
        pub is_buffering: Cell<bool>,
        pub prerolled: Cell<bool>,
        pub seeking: Cell<bool>,
        pub pending_seek: Cell<Option<f64>>,
        /// Generation of the current load; bus messages from older loads are ignored.
        pub load_gen: Cell<u64>,
        pub accepting: Cell<bool>,
        pub headers: Arc<Mutex<Vec<(String, String)>>>,
        pub bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
        pub tick: RefCell<Option<glib::SourceId>>,
        /// When the last seek was issued (for latency logging).
        pub seek_started: Arc<Mutex<Option<std::time::Instant>>>,
    }

    impl Default for Player {
        fn default() -> Self {
            Self {
                playbin: Default::default(),
                sink: Default::default(),
                paintable: Default::default(),
                state: Default::default(),
                position: Default::default(),
                duration: Default::default(),
                buffering: Cell::new(100),
                volume: Cell::new(1.0),
                muted: Default::default(),
                rate: Cell::new(1.0),
                video_width: Default::default(),
                video_height: Default::default(),
                is_live: Default::default(),
                target_playing: Default::default(),
                is_buffering: Default::default(),
                prerolled: Default::default(),
                seeking: Default::default(),
                pending_seek: Default::default(),
                load_gen: Default::default(),
                accepting: Default::default(),
                headers: Default::default(),
                bus_watch: Default::default(),
                tick: Default::default(),
                seek_started: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Player {
        const NAME: &'static str = "SlopPlayer";
        type Type = super::Player;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Player {
        fn signals() -> &'static [Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<Signal>> = std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("error").param_types([String::static_type()]).build(),
                    Signal::builder("eos").build(),
                    Signal::builder("seeked").build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            if let Err(e) = self.build_pipeline() {
                log::error!("failed to build the playback pipeline: {e:#}");
            }
        }

        fn dispose(&self) {
            if let Some(p) = self.playbin.borrow().clone() {
                ctl(move || {
                    let _ = p.set_state(gst::State::Null);
                });
            }
            self.bus_watch.take();
            if let Some(t) = self.tick.take() {
                t.remove();
            }
        }
    }

    impl Player {
        fn build_pipeline(&self) -> anyhow::Result<()> {
            let playbin = gst::ElementFactory::make("playbin3").name("sloptube-player").build()?;
            // Created here, on the main thread: the sink makes its paintable right away.
            let sink = gst::ElementFactory::make("gtk4paintablesink").build()?;
            let paintable = sink.property::<gdk::Paintable>("paintable");
            let seek_started = self.seek_started.clone();
            if let Some(pad) = sink.static_pad("sink") {
                pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                    if let Some(t) = seek_started.lock().unwrap().take() {
                        let pts = info.buffer().and_then(|b| b.pts()).map(|p| p.seconds_f64()).unwrap_or(-1.0);
                        log::debug!("first video frame {:.0} ms after seek (pts {pts:.2})", t.elapsed().as_secs_f64() * 1000.0);
                    }
                    gst::PadProbeReturn::Ok
                });
            }
            playbin.set_property("video-sink", &sink);
            // Only what we need: no subtitle rendering, no software colour balance or
            // deinterlacing (those would force frames out of DMABuf memory).
            playbin.set_property_from_str("flags", "video+audio+soft-volume");
            if let Ok(st) = gst::ElementFactory::make("scaletempo").build() {
                playbin.set_property("audio-filter", &st);
            }
            // Keep a few seconds buffered without hoarding memory.
            playbin.set_property("buffer-duration", 8_000_000_000i64);
            playbin.set_property("buffer-size", 16 * 1024 * 1024i32);

            // Start quickly but keep a healthy buffer once playing (DASH/HLS).
            playbin.connect("element-setup", false, |args| {
                let Ok(el) = args[1].get::<gst::Element>() else {
                    return None;
                };
                let is_adaptive = el
                    .factory()
                    .is_some_and(|f| f.name().starts_with("dashdemux") || f.name().starts_with("hlsdemux"));
                if is_adaptive {
                    for (prop, ns) in [
                        ("max-buffering-time", 30_000_000_000u64),
                        ("high-watermark-time", 2_500_000_000u64),
                        ("low-watermark-time", 800_000_000u64),
                    ] {
                        if el.has_property(prop) {
                            el.set_property(prop, ns);
                        }
                    }
                }
                None
            });

            let headers = self.headers.clone();
            playbin.connect("source-setup", false, move |args| {
                let Ok(src) = args[1].get::<gst::Element>() else {
                    return None;
                };
                let headers = headers.lock().unwrap().clone();
                configure_source(&src, &headers);
                None
            });

            let bus = playbin.bus().expect("pipeline without bus");
            let obj = self.obj();
            let watch = bus.add_watch_local(glib::clone!(
                #[weak]
                obj,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move |_, msg| {
                    obj.imp().handle_message(msg);
                    glib::ControlFlow::Continue
                }
            ))?;
            self.bus_watch.replace(Some(watch));

            let tick = glib::timeout_add_local(
                std::time::Duration::from_millis(250),
                glib::clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    glib::ControlFlow::Break,
                    move || {
                        obj.imp().update_position();
                        glib::ControlFlow::Continue
                    }
                ),
            );
            self.tick.replace(Some(tick));

            self.paintable.replace(Some(paintable));
            self.sink.replace(Some(sink));
            self.playbin.replace(Some(playbin));
            Ok(())
        }

        pub fn playbin(&self) -> Option<gst::Element> {
            self.playbin.borrow().clone()
        }

        pub fn set_play_state(&self, state: PlayState) {
            if self.state.get() != state {
                self.state.set(state);
                self.obj().notify_state();
            }
        }

        /// Change the pipeline state on the control thread.
        pub fn set_pipeline_state(&self, state: gst::State) {
            if let Some(p) = self.playbin() {
                ctl(move || {
                    let _ = p.set_state(state);
                });
            }
        }

        fn set_volume(&self, v: f64) {
            let v = v.clamp(0.0, 1.0);
            self.volume.set(v);
            if let Some(p) = self.playbin() {
                p.set_property("volume", v * v * v);
            }
        }

        fn set_muted(&self, m: bool) {
            self.muted.set(m);
            if let Some(p) = self.playbin() {
                p.set_property("mute", m);
            }
        }

        fn set_rate(&self, r: f64) {
            let r = r.clamp(0.25, 4.0);
            if (self.rate.get() - r).abs() < f64::EPSILON {
                return;
            }
            self.rate.set(r);
            if self.prerolled.get() && !self.is_live.get() {
                self.do_seek(self.position.get(), true);
            }
        }

        pub fn update_position(&self) {
            if !self.prerolled.get() || self.seeking.get() {
                return;
            }
            let Some(p) = self.playbin() else { return };
            if let Some(pos) = p.query_position::<gst::ClockTime>() {
                let secs = pos.nseconds() as f64 / 1e9;
                if (secs - self.position.get()).abs() > 0.01 {
                    self.position.set(secs);
                    self.obj().notify_position();
                }
            }
            if self.duration.get() <= 0.0 {
                self.update_duration();
            }
        }

        fn update_duration(&self) {
            let Some(p) = self.playbin() else { return };
            if let Some(d) = p.query_duration::<gst::ClockTime>() {
                let secs = d.nseconds() as f64 / 1e9;
                if (secs - self.duration.get()).abs() > 0.01 {
                    self.duration.set(secs);
                    self.obj().notify_duration();
                }
            }
        }

        fn update_video_size(&self) {
            let Some(sink) = self.sink.borrow().clone() else { return };
            let Some(caps) = sink.static_pad("sink").and_then(|p| p.current_caps()) else {
                return;
            };
            let Some(s) = caps.structure(0) else { return };
            let (w, h) = (s.get::<i32>("width").unwrap_or(0), s.get::<i32>("height").unwrap_or(0));
            if w != self.video_width.get() || h != self.video_height.get() {
                self.video_width.set(w);
                self.video_height.set(h);
                self.obj().notify_video_width();
                self.obj().notify_video_height();
            }
        }

        /// Move the pipeline towards the state the user asked for.
        pub fn sync_state(&self) {
            if !self.prerolled.get() {
                return;
            }
            let want_playing = self.target_playing.get() && !self.is_buffering.get() && !self.seeking.get();
            self.set_pipeline_state(if want_playing { gst::State::Playing } else { gst::State::Paused });
            if self.is_buffering.get() && self.target_playing.get() {
                self.set_play_state(PlayState::Buffering);
            } else if !self.target_playing.get() && self.state.get() != PlayState::Ended {
                self.set_play_state(PlayState::Paused);
            }
        }

        pub fn do_seek(&self, pos: f64, accurate: bool) {
            let Some(p) = self.playbin() else { return };
            if !self.prerolled.get() {
                self.pending_seek.set(Some(pos));
                return;
            }
            let mut flags = gst::SeekFlags::FLUSH;
            flags |= if accurate {
                gst::SeekFlags::ACCURATE
            } else {
                gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_NEAREST
            };
            let pos = pos.max(0.0);
            let target = gst::ClockTime::from_nseconds((pos * 1e9) as u64);
            let rate = self.rate.get();
            let seek = gst::event::Seek::new(
                rate,
                flags,
                gst::SeekType::Set,
                target,
                gst::SeekType::None,
                gst::ClockTime::NONE,
            );
            let decoder = self.video_decoder().filter(needs_decoder_reset);
            *self.seek_started.lock().unwrap() = Some(std::time::Instant::now());
            self.seeking.set(true);
            self.position.set(pos);
            self.obj().notify_position();
            if self.state.get() == PlayState::Ended {
                self.set_play_state(PlayState::Paused);
            }
            let res = ctl_await(move || match decoder {
                Some(dec) => seek_with_decoder_reset(&p, &dec, seek),
                None => p.send_event(seek),
            });
            let obj = self.obj().downgrade();
            let load_gen = self.load_gen.get();
            glib::spawn_future_local(async move {
                let ok = res.await.unwrap_or(false);
                let Some(obj) = obj.upgrade() else { return };
                let imp = obj.imp();
                if !ok && imp.load_gen.get() == load_gen {
                    log::warn!("seek to {pos:.1}s failed (rate {rate})");
                    imp.seeking.set(false);
                }
            });
        }

        fn video_decoder(&self) -> Option<gst::Element> {
            let playbin = self.playbin()?;
            let bin = playbin.downcast_ref::<gst::Bin>()?;
            bin.iterate_recurse().into_iter().flatten().find(|el| {
                el.factory().is_some_and(|f| {
                    let klass = f.metadata(gst::ELEMENT_METADATA_KLASS).unwrap_or("");
                    klass.contains("Decoder") && klass.contains("Video")
                })
            })
        }

        fn handle_message(&self, msg: &gst::Message) {
            use gst::MessageView;
            let Some(playbin) = self.playbin() else { return };
            if let MessageView::Application(app) = msg.view() {
                if let Some(s) = app.structure().filter(|s| s.name() == LOAD_MARKER) {
                    let marker = s.get::<u64>("gen").ok();
                    log::debug!("load marker {marker:?} (current {})", self.load_gen.get());
                    if marker == Some(self.load_gen.get()) {
                        self.accepting.set(true);
                    }
                }
                return;
            }
            // Anything posted before the current load reached the pipeline is stale.
            if !self.accepting.get() {
                log::debug!("ignoring stale {:?} from {}", msg.type_(), msg.src().map(|s| s.name().to_string()).unwrap_or_default());
                return;
            }
            match msg.view() {
                MessageView::Eos(_) => {
                    self.target_playing.set(false);
                    self.update_position();
                    self.set_play_state(PlayState::Ended);
                    self.obj().emit_by_name::<()>("eos", &[]);
                }
                MessageView::Error(err) => {
                    let src = msg
                        .src()
                        .map(|s| s.path_string().to_string())
                        .unwrap_or_default();
                    log::error!("playback error from {src}: {} ({:?})", err.error(), err.debug());
                    self.set_pipeline_state(gst::State::Null);
                    self.accepting.set(false);
                    self.prerolled.set(false);
                    self.target_playing.set(false);
                    self.set_play_state(PlayState::Error);
                    let detail = err.debug().map(|d| d.to_string()).unwrap_or_default();
                    let message = format!("{}\n{detail}", err.error());
                    self.obj().emit_by_name::<()>("error", &[&message]);
                }
                MessageView::Warning(w) => {
                    let text = w.error().to_string();
                    if !text.contains("too many buffers") {
                        log::warn!("playback warning: {text}");
                    }
                }
                MessageView::Buffering(b) => {
                    if self.is_live.get() {
                        return;
                    }
                    let percent = b.percent();
                    if percent != self.buffering.get() {
                        self.buffering.set(percent);
                        self.obj().notify_buffering();
                    }
                    let buffering = percent < 100;
                    if buffering != self.is_buffering.get() {
                        self.is_buffering.set(buffering);
                        self.sync_state();
                    }
                }
                MessageView::StateChanged(sc) => {
                    if msg.src() != Some(playbin.upcast_ref()) {
                        return;
                    }
                    match sc.current() {
                        gst::State::Playing => {
                            self.update_video_size();
                            self.set_play_state(PlayState::Playing);
                        }
                        gst::State::Paused if sc.pending() == gst::State::VoidPending => {
                            if self.prerolled.get()
                                && !self.is_buffering.get()
                                && !self.target_playing.get()
                                && self.state.get() != PlayState::Ended
                            {
                                self.set_play_state(PlayState::Paused);
                            }
                        }
                        _ => {}
                    }
                }
                MessageView::AsyncDone(_) => {
                    log::debug!("async-done (prerolled before: {})", self.prerolled.get());
                    let first = !self.prerolled.get();
                    self.prerolled.set(true);
                    self.seeking.set(false);
                    self.update_duration();
                    self.update_video_size();
                    if let Some(pos) = self.pending_seek.take() {
                        self.do_seek(pos, true);
                        return;
                    }
                    if !first {
                        self.update_position();
                        self.obj().emit_by_name::<()>("seeked", &[]);
                    }
                    if !self.target_playing.get() && self.state.get() != PlayState::Ended {
                        self.set_play_state(PlayState::Paused);
                    }
                    self.sync_state();
                }
                MessageView::DurationChanged(_) => self.update_duration(),
                MessageView::Latency(_) => {
                    if let Ok(bin) = playbin.clone().downcast::<gst::Bin>() {
                        ctl(move || {
                            let _ = bin.recalculate_latency();
                        });
                    }
                }
                MessageView::ClockLost(_) => {
                    self.set_pipeline_state(gst::State::Paused);
                    self.sync_state();
                }
                MessageView::RequestState(rs) => {
                    self.set_pipeline_state(rs.requested_state());
                }
                _ => {}
            }
        }
    }
}

/// V4L2 stateful decoders (Qualcomm Iris/Venus, ...) can stop producing frames
/// after a flush. Re-opening the device around seeks avoids it; stateless
/// (v4l2sl*), VA-API and software decoders don't need this.
fn needs_decoder_reset(dec: &gst::Element) -> bool {
    if std::env::var_os("SLOPTUBE_NO_DECODER_RESET").is_some() {
        return false;
    }
    dec.factory().is_some_and(|f| {
        let n = f.name();
        n.starts_with("v4l2") && !n.starts_with("v4l2sl")
    })
}

/// Seek while the video decoder is closed, then bring it back fresh (control thread).
///
/// Order matters:
/// 1. hold new data at the decoder's input;
/// 2. flush the video sink — the decoder's output thread may be parked in a
///    prerolled sink, and closing the decoder would wait for it forever;
/// 3. close the decoder and seek, both from its input (the video demuxer) and
///    through the pipeline (audio); the events share a seqnum, so a demuxer
///    that feeds both streams (DASH) only acts once;
/// 4. reopen the decoder, end the sink's flush, and release the data.
fn seek_with_decoder_reset(pipeline: &gst::Element, dec: &gst::Element, seek: gst::Event) -> bool {
    let Some(peer) = dec.static_pad("sink").and_then(|p| p.peer()) else {
        return pipeline.send_event(seek);
    };
    let src = dec.static_pad("src");
    let block = peer.add_probe(gst::PadProbeType::BLOCK | gst::PadProbeType::BUFFER, |_, _| {
        gst::PadProbeReturn::Ok
    });
    if let Some(src) = &src {
        let _ = src.push_event(gst::event::FlushStart::new());
    }
    let _ = dec.set_state(gst::State::Null);
    let via_video = peer.send_event(seek.clone());
    let via_pipeline = pipeline.send_event(seek);
    let _ = dec.sync_state_with_parent();
    if let Some(src) = &src {
        let _ = src.push_event(gst::event::FlushStop::new(true));
    }
    if let Some(id) = block {
        peer.remove_probe(id);
    }
    log::debug!("seek with decoder reset (video path: {via_video}, pipeline: {via_pipeline})");
    via_video || via_pipeline
}

fn configure_source(src: &gst::Element, headers: &[(String, String)]) {
    let ua = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        .map(|(_, v)| v.as_str())
        .unwrap_or(crate::api::client::USER_AGENT);
    if src.has_property("user-agent") {
        src.set_property("user-agent", ua);
    }
    if src.has_property("extra-headers") {
        let mut s = gst::Structure::builder("extra-headers");
        for (k, v) in headers {
            if !k.eq_ignore_ascii_case("user-agent") {
                s = s.field(k.as_str(), v.as_str());
            }
        }
        src.set_property("extra-headers", s.build());
    }
    if src.has_property("retries") {
        src.set_property("retries", 5i32);
    }
    if src.has_property("timeout") {
        src.set_property("timeout", 20u32);
    }
}

glib::wrapper! {
    pub struct Player(ObjectSubclass<imp::Player>);
}

impl Default for Player {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Player {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(&self, req: LoadRequest) {
        let imp = self.imp();
        let Some(playbin) = imp.playbin() else {
            self.emit_by_name::<()>("error", &[&"GStreamer is not available".to_string()]);
            return;
        };
        let generation = imp.load_gen.get() + 1;
        imp.load_gen.set(generation);
        imp.accepting.set(false);
        imp.prerolled.set(false);
        imp.seeking.set(false);
        imp.is_buffering.set(false);
        imp.target_playing.set(req.autoplay);
        imp.pending_seek.set(req.start.filter(|s| *s > 1.0));
        imp.position.set(req.start.unwrap_or(0.0));
        imp.duration.set(0.0);
        imp.buffering.set(100);
        self.notify_position();
        self.notify_duration();
        *imp.headers.lock().unwrap() = req.headers;
        imp.set_play_state(PlayState::Loading);

        let volume = imp.volume.get().powi(3);
        let muted = imp.muted.get();
        let source = req.source;
        let res = ctl_await(move || {
            let _ = playbin.set_state(gst::State::Null);
            match &source {
                Source::Separate { video, audio } => {
                    playbin.set_property("uri", video);
                    playbin.set_property("suburi", audio);
                }
                Source::Single { uri } => {
                    playbin.set_property("uri", uri);
                    playbin.set_property("suburi", None::<&str>);
                }
            }
            playbin.set_property("volume", volume);
            playbin.set_property("mute", muted);
            let _ = playbin.set_state(gst::State::Ready);
            // Marks the start of this load on the bus (see `accepting`).
            let marker = gst::Structure::builder(LOAD_MARKER).field("gen", generation).build();
            let _ = playbin.post_message(gst::message::Application::new(marker));
            playbin.set_state(gst::State::Paused)
        });
        let obj = self.downgrade();
        let autoplay = req.autoplay;
        glib::spawn_future_local(async move {
            let res = res.await;
            let Some(obj) = obj.upgrade() else { return };
            let imp = obj.imp();
            if imp.load_gen.get() != generation {
                return;
            }
            match res {
                Ok(Ok(gst::StateChangeSuccess::NoPreroll)) => {
                    imp.is_live.set(true);
                    obj.notify_is_live();
                    imp.prerolled.set(true);
                    if autoplay {
                        imp.set_pipeline_state(gst::State::Playing);
                    }
                }
                Ok(Ok(_)) => {
                    if imp.is_live.get() {
                        imp.is_live.set(false);
                        obj.notify_is_live();
                    }
                }
                Ok(Err(e)) => {
                    log::error!("cannot start playback: {e}");
                    imp.set_play_state(PlayState::Error);
                    obj.emit_by_name::<()>("error", &[&"Could not start playback".to_string()]);
                }
                Err(_) => {}
            }
        });
    }

    pub fn play(&self) {
        let imp = self.imp();
        if imp.state.get() == PlayState::Ended {
            imp.target_playing.set(true);
            imp.do_seek(0.0, true);
            return;
        }
        if matches!(imp.state.get(), PlayState::Idle | PlayState::Error) {
            return;
        }
        imp.target_playing.set(true);
        if imp.is_live.get() && imp.prerolled.get() {
            imp.set_pipeline_state(gst::State::Playing);
            return;
        }
        imp.sync_state();
    }

    pub fn pause(&self) {
        let imp = self.imp();
        imp.target_playing.set(false);
        if imp.prerolled.get() {
            imp.set_pipeline_state(gst::State::Paused);
        }
        if !matches!(imp.state.get(), PlayState::Idle | PlayState::Error | PlayState::Ended) {
            imp.set_play_state(PlayState::Paused);
        }
    }

    pub fn toggle(&self) {
        if self.is_playing_intent() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Whether playback is running or about to (loading/buffering with autoplay).
    pub fn is_playing_intent(&self) -> bool {
        self.imp().target_playing.get()
    }

    pub fn stop(&self) {
        let imp = self.imp();
        imp.load_gen.set(imp.load_gen.get() + 1);
        imp.accepting.set(false);
        imp.target_playing.set(false);
        imp.prerolled.set(false);
        imp.seeking.set(false);
        imp.set_pipeline_state(gst::State::Null);
        imp.position.set(0.0);
        imp.duration.set(0.0);
        self.notify_position();
        self.notify_duration();
        imp.set_play_state(PlayState::Idle);
    }

    /// Stop and wait (up to `timeout`) until the pipeline is down, while still
    /// servicing the main loop. Used when quitting.
    pub fn shutdown(&self, timeout: std::time::Duration) {
        let Some(p) = self.imp().playbin() else { return };
        self.imp().accepting.set(false);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        ctl(move || {
            let _ = p.set_state(gst::State::Null);
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let ctx = glib::MainContext::default();
        let deadline = std::time::Instant::now() + timeout;
        while !done.load(std::sync::atomic::Ordering::SeqCst) && std::time::Instant::now() < deadline {
            ctx.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub fn seek(&self, pos: f64, accurate: bool) {
        let d = self.duration();
        let pos = if d > 0.0 { pos.clamp(0.0, (d - 0.5).max(0.0)) } else { pos.max(0.0) };
        self.imp().do_seek(pos, accurate);
    }

    pub fn seek_relative(&self, delta: f64) {
        self.seek(self.position() + delta, true);
    }

    pub fn stats(&self) -> Stats {
        let imp = self.imp();
        let mut st = Stats::default();
        let Some(playbin) = imp.playbin() else { return st };
        if let Some(bin) = playbin.downcast_ref::<gst::Bin>() {
            for el in bin.iterate_recurse().into_iter().flatten() {
                let Some(f) = el.factory() else { continue };
                let klass = f.metadata(gst::ELEMENT_METADATA_KLASS).unwrap_or("");
                if klass.contains("Decoder") && klass.contains("Video") {
                    st.video_decoder = Some(f.name().to_string());
                    st.hardware = klass.contains("Hardware");
                } else if klass.contains("Decoder") && klass.contains("Audio") {
                    st.audio_decoder = Some(f.name().to_string());
                }
            }
        }
        if let Some(sink) = imp.sink.borrow().as_ref() {
            if let Some(caps) = sink.static_pad("sink").and_then(|p| p.current_caps()) {
                st.dmabuf = caps.features(0).is_some_and(|f| f.contains("memory:DMABuf"));
                if let Some(s) = caps.structure(0) {
                    st.width = s.get::<i32>("width").unwrap_or(0);
                    st.height = s.get::<i32>("height").unwrap_or(0);
                    st.format = s
                        .get::<String>("drm-format")
                        .or_else(|_| s.get::<String>("format"))
                        .unwrap_or_default();
                    if let Ok(fr) = s.get::<gst::Fraction>("framerate") {
                        if fr.denom() > 0 {
                            st.framerate = format!("{:.2}", fr.numer() as f64 / fr.denom() as f64);
                        }
                    }
                }
            }
            if let Some(stats) = sink.property::<Option<gst::Structure>>("stats") {
                st.rendered = stats.get::<u64>("rendered").unwrap_or(0);
                st.dropped = stats.get::<u64>("dropped").unwrap_or(0);
            }
        }
        st
    }

    pub fn connect_error<F: Fn(&Self, String) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("error", false, move |args| {
            let obj = args[0].get::<Self>().unwrap();
            let msg = args[1].get::<String>().unwrap_or_default();
            f(&obj, msg);
            None
        })
    }

    pub fn connect_eos<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("eos", false, move |args| {
            f(&args[0].get::<Self>().unwrap());
            None
        })
    }

    pub fn connect_seeked<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("seeked", false, move |args| {
            f(&args[0].get::<Self>().unwrap());
            None
        })
    }
}

/// Keep only the headers a media server cares about.
pub fn headers_from_map(map: &HashMap<String, String>) -> Vec<(String, String)> {
    map.iter()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            k == "user-agent" || k == "accept-language" || k == "referer" || k == "origin"
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
