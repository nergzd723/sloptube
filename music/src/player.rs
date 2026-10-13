//! GStreamer audio player.
//!
//! One `playbin3` with only audio enabled. State changes and flushing seeks
//! can block on the network, so they run on a dedicated control thread, never
//! on the main thread.
//!
//! Gapless playback: when the current stream's data has all been read,
//! playbin3 emits `about-to-finish` (on a streaming thread). If the controller
//! has prepared the next stream with [`Player::set_next`], its URI is handed
//! to playbin3 right there, and once the new stream reaches the sink
//! (`stream-start`) the player emits `advanced` with the stream's token.

use std::cell::{Cell, RefCell};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use gst::prelude::*;
use gtk::glib;
use gtk::glib::subclass::Signal;
use gtk::subclass::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "StmPlayState")]
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
pub struct LoadRequest {
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub start: Option<f64>,
    pub autoplay: bool,
}

/// A stream prepared for a gapless switch.
#[derive(Debug, Clone)]
pub struct NextStream {
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub token: u64,
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
            .name("stmusic-gst".into())
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

const LOAD_MARKER: &str = "stmusic-load";

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::Player)]
    pub struct Player {
        pub playbin: RefCell<Option<gst::Element>>,
        #[property(get, builder(PlayState::default()))]
        pub state: Cell<PlayState>,
        /// Position in seconds.
        #[property(get)]
        pub position: Cell<f64>,
        /// Duration in seconds (0 if unknown).
        #[property(get)]
        pub duration: Cell<f64>,
        /// Perceptual volume 0..1 (cubic mapping to linear gain).
        #[property(get, set = Self::set_volume, minimum = 0.0, maximum = 1.0, default = 1.0)]
        pub volume: Cell<f64>,
        #[property(get, set = Self::set_muted)]
        pub muted: Cell<bool>,

        pub target_playing: Cell<bool>,
        pub is_buffering: Cell<bool>,
        pub prerolled: Cell<bool>,
        pub seeking: Cell<bool>,
        pub pending_seek: Cell<Option<f64>>,
        /// Generation of the current load; bus messages from older loads are ignored.
        pub load_gen: Cell<u64>,
        pub accepting: Cell<bool>,
        pub headers: Arc<Mutex<Vec<(String, String)>>>,
        pub next: Arc<Mutex<Option<NextStream>>>,
        /// Token of the stream playbin3 switched to in `about-to-finish`.
        pub switched: Arc<Mutex<Option<u64>>>,
        pub bus_watch: RefCell<Option<gst::bus::BusWatchGuard>>,
        pub tick: RefCell<Option<glib::SourceId>>,
    }

    impl Default for Player {
        fn default() -> Self {
            Self {
                playbin: Default::default(),
                state: Default::default(),
                position: Default::default(),
                duration: Default::default(),
                volume: Cell::new(1.0),
                muted: Default::default(),
                target_playing: Default::default(),
                is_buffering: Default::default(),
                prerolled: Default::default(),
                seeking: Default::default(),
                pending_seek: Default::default(),
                load_gen: Default::default(),
                accepting: Default::default(),
                headers: Default::default(),
                next: Default::default(),
                switched: Default::default(),
                bus_watch: Default::default(),
                tick: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Player {
        const NAME: &'static str = "StmPlayer";
        type Type = super::Player;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Player {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("error").param_types([String::static_type()]).build(),
                    Signal::builder("eos").build(),
                    Signal::builder("seeked").build(),
                    Signal::builder("advanced").param_types([u64::static_type()]).build(),
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
            self.stop_tick();
        }
    }

    impl Player {
        fn build_pipeline(&self) -> anyhow::Result<()> {
            let playbin = gst::ElementFactory::make("playbin3").name("stmusic-player").build()?;
            playbin.set_property_from_str("flags", "audio+soft-volume");
            // For testing: SLOPTUBE_MUSIC_AUDIO_SINK=fakesink plays silently in real time.
            if let Ok(name) = std::env::var("SLOPTUBE_MUSIC_AUDIO_SINK") {
                match gst::ElementFactory::make(&name).build() {
                    Ok(sink) => {
                        if sink.has_property("sync") {
                            sink.set_property("sync", true);
                        }
                        playbin.set_property("audio-sink", &sink);
                    }
                    Err(e) => log::warn!("audio sink {name}: {e}"),
                }
            }
            // Music needs little buffering: ~15 s of Opus is a few hundred KB.
            playbin.set_property("buffer-duration", 15_000_000_000i64);
            playbin.set_property("buffer-size", 1024 * 1024i32);

            let headers = self.headers.clone();
            playbin.connect("source-setup", false, move |args| {
                let Ok(src) = args[1].get::<gst::Element>() else {
                    return None;
                };
                configure_source(&src, &headers.lock().unwrap());
                None
            });

            let next = self.next.clone();
            let switched = self.switched.clone();
            let headers = self.headers.clone();
            playbin.connect("about-to-finish", false, move |args| {
                let Ok(pb) = args[0].get::<gst::Element>() else {
                    return None;
                };
                if let Some(n) = next.lock().unwrap().take() {
                    log::debug!("gapless: queueing the next stream (token {})", n.token);
                    *headers.lock().unwrap() = n.headers;
                    *switched.lock().unwrap() = Some(n.token);
                    pb.set_property("uri", &n.uri);
                }
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
            self.playbin.replace(Some(playbin));
            Ok(())
        }

        pub fn playbin(&self) -> Option<gst::Element> {
            self.playbin.borrow().clone()
        }

        /// Position updates only while playing, so an idle app doesn't wake up.
        fn start_tick(&self) {
            if self.tick.borrow().is_some() {
                return;
            }
            let obj = self.obj();
            let id = glib::timeout_add_local(
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
            self.tick.replace(Some(id));
        }

        fn stop_tick(&self) {
            if let Some(t) = self.tick.take() {
                t.remove();
            }
        }

        pub fn set_play_state(&self, state: PlayState) {
            if state == PlayState::Playing {
                self.start_tick();
            } else {
                self.stop_tick();
            }
            if self.state.get() != state {
                self.state.set(state);
                self.obj().notify_state();
            }
        }

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

        pub fn do_seek(&self, pos: f64) {
            let Some(p) = self.playbin() else { return };
            if !self.prerolled.get() {
                self.pending_seek.set(Some(pos));
                return;
            }
            let pos = pos.max(0.0);
            let target = gst::ClockTime::from_nseconds((pos * 1e9) as u64);
            // A seek replaces whatever gapless switch was queued for the old position.
            self.switched.lock().unwrap().take();
            self.seeking.set(true);
            self.position.set(pos);
            self.obj().notify_position();
            if self.state.get() == PlayState::Ended {
                self.set_play_state(PlayState::Paused);
            }
            let res = ctl_await(move || {
                p.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, target)
                    .is_ok()
            });
            let obj = self.obj().downgrade();
            let load_gen = self.load_gen.get();
            glib::spawn_future_local(async move {
                let ok = res.await.unwrap_or(false);
                let Some(obj) = obj.upgrade() else { return };
                let imp = obj.imp();
                if !ok && imp.load_gen.get() == load_gen {
                    log::warn!("seek to {pos:.1}s failed");
                    imp.seeking.set(false);
                }
            });
        }

        fn handle_message(&self, msg: &gst::Message) {
            use gst::MessageView;
            let Some(playbin) = self.playbin() else { return };
            if let MessageView::Application(app) = msg.view() {
                if let Some(s) = app.structure().filter(|s| s.name() == LOAD_MARKER)
                    && s.get::<u64>("gen").ok() == Some(self.load_gen.get()) {
                        self.accepting.set(true);
                    }
                return;
            }
            // Anything posted before the current load reached the pipeline is stale.
            if !self.accepting.get() {
                return;
            }
            match msg.view() {
                MessageView::StreamStart(_) => {
                    let token = self.switched.lock().unwrap().take();
                    if let Some(token) = token {
                        log::debug!("gapless: next stream started (token {token})");
                        self.position.set(0.0);
                        self.duration.set(0.0);
                        self.obj().notify_position();
                        self.obj().notify_duration();
                        self.update_duration();
                        self.obj().emit_by_name::<()>("advanced", &[&token]);
                    }
                }
                MessageView::Eos(_) => {
                    self.target_playing.set(false);
                    self.update_position();
                    self.set_play_state(PlayState::Ended);
                    self.obj().emit_by_name::<()>("eos", &[]);
                }
                MessageView::Error(err) => {
                    let src = msg.src().map(|s| s.path_string().to_string()).unwrap_or_default();
                    log::error!("playback error from {src}: {} ({:?})", err.error(), err.debug());
                    self.set_pipeline_state(gst::State::Null);
                    self.accepting.set(false);
                    self.prerolled.set(false);
                    self.target_playing.set(false);
                    self.switched.lock().unwrap().take();
                    self.set_play_state(PlayState::Error);
                    let detail = err.debug().map(|d| d.to_string()).unwrap_or_default();
                    let message = format!("{}\n{detail}", err.error());
                    self.obj().emit_by_name::<()>("error", &[&message]);
                }
                MessageView::Buffering(b) => {
                    let buffering = b.percent() < 100;
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
                        gst::State::Playing => self.set_play_state(PlayState::Playing),
                        gst::State::Paused if sc.pending() == gst::State::VoidPending
                            && self.prerolled.get()
                                && !self.is_buffering.get()
                                && !self.target_playing.get()
                                && self.state.get() != PlayState::Ended
                            => {
                                self.set_play_state(PlayState::Paused);
                            }
                        _ => {}
                    }
                }
                MessageView::AsyncDone(_) => {
                    let first = !self.prerolled.get();
                    self.prerolled.set(true);
                    self.seeking.set(false);
                    self.update_duration();
                    if let Some(pos) = self.pending_seek.take() {
                        self.do_seek(pos);
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
                MessageView::ClockLost(_) => {
                    self.set_pipeline_state(gst::State::Paused);
                    self.sync_state();
                }
                MessageView::RequestState(rs) => self.set_pipeline_state(rs.requested_state()),
                _ => {}
            }
        }
    }
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
    pub fn load(&self, req: LoadRequest) {
        let imp = self.imp();
        let Some(playbin) = imp.playbin() else {
            self.emit_by_name::<()>("error", &[&"GStreamer's playbin3 is not available".to_string()]);
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
        imp.next.lock().unwrap().take();
        imp.switched.lock().unwrap().take();
        self.notify_position();
        self.notify_duration();
        *imp.headers.lock().unwrap() = req.headers;
        imp.set_play_state(PlayState::Loading);

        let volume = imp.volume.get().powi(3);
        let muted = imp.muted.get();
        let uri = req.uri;
        // Straight to PLAYING when there's nothing to seek to: one round trip less.
        let direct = req.autoplay && imp.pending_seek.get().is_none();
        let res = ctl_await(move || {
            let _ = playbin.set_state(gst::State::Null);
            playbin.set_property("uri", &uri);
            playbin.set_property("volume", volume);
            playbin.set_property("mute", muted);
            let _ = playbin.set_state(gst::State::Ready);
            let marker = gst::Structure::builder(LOAD_MARKER).field("gen", generation).build();
            let _ = playbin.post_message(gst::message::Application::new(marker));
            playbin.set_state(if direct { gst::State::Playing } else { gst::State::Paused })
        });
        let obj = self.downgrade();
        glib::spawn_future_local(async move {
            let res = res.await;
            let Some(obj) = obj.upgrade() else { return };
            let imp = obj.imp();
            if imp.load_gen.get() != generation {
                return;
            }
            match res {
                Ok(Ok(gst::StateChangeSuccess::NoPreroll)) => {
                    // Live: nothing to preroll.
                    imp.prerolled.set(true);
                    if imp.target_playing.get() {
                        imp.set_pipeline_state(gst::State::Playing);
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    log::error!("cannot start playback: {e}");
                    imp.set_play_state(PlayState::Error);
                    obj.emit_by_name::<()>("error", &[&"Could not start playback".to_string()]);
                }
                Err(_) => {}
            }
        });
    }

    /// Prepare (or clear) the stream to continue with gaplessly.
    pub fn set_next(&self, next: Option<NextStream>) {
        *self.imp().next.lock().unwrap() = next;
    }

    pub fn play(&self) {
        let imp = self.imp();
        if imp.state.get() == PlayState::Ended {
            imp.target_playing.set(true);
            imp.do_seek(0.0);
            return;
        }
        if matches!(imp.state.get(), PlayState::Idle | PlayState::Error) {
            return;
        }
        imp.target_playing.set(true);
        imp.sync_state();
    }

    pub fn pause(&self) {
        let imp = self.imp();
        imp.target_playing.set(false);
        imp.set_pipeline_state(gst::State::Paused);
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
        imp.next.lock().unwrap().take();
        imp.switched.lock().unwrap().take();
        imp.set_pipeline_state(gst::State::Null);
        imp.position.set(0.0);
        imp.duration.set(0.0);
        self.notify_position();
        self.notify_duration();
        imp.set_play_state(PlayState::Idle);
    }

    /// Stop and wait (up to `timeout`) until the pipeline is down. Used when quitting.
    pub fn shutdown(&self, timeout: std::time::Duration) {
        let Some(p) = self.imp().playbin() else { return };
        self.imp().accepting.set(false);
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        ctl(move || {
            let _ = p.set_state(gst::State::Null);
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let deadline = std::time::Instant::now() + timeout;
        while !done.load(std::sync::atomic::Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub fn seek(&self, pos: f64) {
        let d = self.duration();
        let pos = if d > 0.0 { pos.clamp(0.0, (d - 0.5).max(0.0)) } else { pos.max(0.0) };
        self.imp().do_seek(pos);
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

    /// The gaplessly prepared stream with this token started.
    pub fn connect_advanced<F: Fn(&Self, u64) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("advanced", false, move |args| {
            f(&args[0].get::<Self>().unwrap(), args[1].get::<u64>().unwrap_or(0));
            None
        })
    }
}
