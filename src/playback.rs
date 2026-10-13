//! Playback controller: what's playing, its metadata, the queue and stream selection.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use common::history::Tracker;
use common::recovery::Recovery;
use gtk::glib::subclass::Signal;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::api::{self, Item, VideoDetails, VideoItem};
use crate::auth;
use crate::extractor::{self, StreamInfo};
use crate::player::decoders::{self, DecoderSupport};
use crate::player::formats::{self, Prefs, QualityOption, Selection};
use crate::player::{self, LoadRequest, PlayState, Player, Source};
use crate::runtime;
use crate::settings;

#[derive(Debug, Clone)]
pub struct Queue {
    pub playlist_id: String,
    pub title: String,
    pub items: Vec<VideoItem>,
    pub index: usize,
}

// ---------------------------------------------------------------------------
// Local resume positions
// ---------------------------------------------------------------------------

fn positions_path() -> std::path::PathBuf {
    crate::config::data_dir().join("positions.json")
}

thread_local! {
    static POSITIONS: RefCell<Option<Vec<(String, f64)>>> = const { RefCell::new(None) };
}

fn with_positions<R>(f: impl FnOnce(&mut Vec<(String, f64)>) -> R) -> R {
    POSITIONS.with(|p| {
        let mut p = p.borrow_mut();
        let list = p.get_or_insert_with(|| {
            std::fs::read(positions_path())
                .ok()
                .and_then(|d| serde_json::from_slice(&d).ok())
                .unwrap_or_default()
        });
        f(list)
    })
}

pub fn saved_position(id: &str) -> Option<f64> {
    with_positions(|l| l.iter().find(|(i, _)| i == id).map(|(_, p)| *p))
}

fn save_position(id: &str, pos: f64, duration: f64) {
    let keep = duration > 120.0 && pos > 15.0 && pos < duration - 20.0;
    let changed = with_positions(|l| {
        let before = l.len();
        l.retain(|(i, _)| i != id);
        if keep {
            l.insert(0, (id.to_owned(), pos));
            l.truncate(400);
        }
        keep || l.len() != before
    });
    if changed {
        with_positions(|l| {
            if let Ok(data) = serde_json::to_vec(l) {
                let _ = std::fs::write(positions_path(), data);
            }
        });
    }
}

// ---------------------------------------------------------------------------

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::Playback)]
    pub struct Playback {
        pub player: Player,
        #[property(get)]
        pub video_id: RefCell<String>,
        #[property(get)]
        pub title: RefCell<String>,
        #[property(get)]
        pub channel_name: RefCell<String>,
        #[property(get)]
        pub thumbnail: RefCell<String>,
        /// A video is open (watch view / mini player have something to show).
        #[property(get)]
        pub active: Cell<bool>,
        #[property(get)]
        pub loading_streams: Cell<bool>,
        #[property(get)]
        pub error: RefCell<String>,
        #[property(get)]
        pub audio_only: Cell<bool>,
        #[property(get)]
        pub offline: Cell<bool>,

        pub item: RefCell<Option<VideoItem>>,
        pub details: RefCell<Option<VideoDetails>>,
        pub info: RefCell<Option<Arc<StreamInfo>>>,
        pub selection: RefCell<Option<Selection>>,
        pub quality: Cell<Option<u32>>,
        pub queue: RefCell<Option<Queue>>,
        pub generation: Cell<u64>,
        pub support: RefCell<Option<DecoderSupport>>,
        pub tracking: RefCell<Option<Tracker>>,
        pub last_saved: Cell<f64>,
        pub recovery: Recovery,
        pub played: RefCell<Vec<String>>,
        pub local_file: RefCell<Option<String>>,
    }

    impl Default for Playback {
        fn default() -> Self {
            Self {
                player: Player::new(player::CONFIG),
                video_id: Default::default(),
                title: Default::default(),
                channel_name: Default::default(),
                thumbnail: Default::default(),
                active: Default::default(),
                loading_streams: Default::default(),
                error: Default::default(),
                audio_only: Default::default(),
                offline: Default::default(),
                item: Default::default(),
                details: Default::default(),
                info: Default::default(),
                selection: Default::default(),
                quality: Default::default(),
                queue: Default::default(),
                generation: Default::default(),
                support: Default::default(),
                tracking: Default::default(),
                last_saved: Default::default(),
                recovery: Default::default(),
                played: Default::default(),
                local_file: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Playback {
        const NAME: &'static str = "SlopPlayback";
        type Type = super::Playback;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Playback {
        fn signals() -> &'static [Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<Signal>> = std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("video-changed").build(),
                    Signal::builder("details-loaded").build(),
                    Signal::builder("streams-loaded").build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            let s = settings::get();
            self.player.set_volume(s.volume);
            self.player.set_muted(s.muted);
            self.player.set_rate(s.speed);

            self.player.connect_eos(glib::clone!(
                #[weak]
                obj,
                move |_| obj.on_eos()
            ));
            self.player.connect_error(glib::clone!(
                #[weak]
                obj,
                move |_, msg, transient| obj.on_error(msg, transient)
            ));
            self.recovery.connect_back_online(glib::clone!(
                #[weak]
                obj,
                move || obj.on_network_available()
            ));
            self.player.connect_position_notify(glib::clone!(
                #[weak]
                obj,
                move |_| obj.on_tick()
            ));
            self.player.connect_state_notify(glib::clone!(
                #[weak]
                obj,
                move |p| {
                    if matches!(p.state(), PlayState::Paused | PlayState::Ended) {
                        obj.flush_progress(p.state() == PlayState::Ended);
                    }
                    if p.state() == PlayState::Playing {
                        obj.imp().recovery.reset();
                    }
                }
            ));
            self.player.connect_volume_notify(|p| settings::update(|s| s.volume = p.volume()));
            self.player.connect_muted_notify(|p| settings::update(|s| s.muted = p.muted()));
        }
    }
}

glib::wrapper! {
    pub struct Playback(ObjectSubclass<imp::Playback>);
}

impl Default for Playback {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Playback {
    pub fn player(&self) -> Player {
        self.imp().player.clone()
    }

    pub fn current_item(&self) -> Option<VideoItem> {
        self.imp().item.borrow().clone()
    }

    pub fn details(&self) -> Option<VideoDetails> {
        self.imp().details.borrow().clone()
    }

    pub fn stream_info(&self) -> Option<Arc<StreamInfo>> {
        self.imp().info.borrow().clone()
    }

    pub fn selection(&self) -> Option<Selection> {
        self.imp().selection.borrow().clone()
    }

    pub fn queue(&self) -> Option<Queue> {
        self.imp().queue.borrow().clone()
    }

    fn support(&self) -> DecoderSupport {
        self.imp()
            .support
            .borrow_mut()
            .get_or_insert_with(decoders::probe)
            .clone()
    }

    /// Call after changing decoder settings.
    pub fn refresh_decoders(&self) {
        decoders::set_hardware_decoding(settings::get().hw_decode);
        self.imp().support.replace(None);
    }

    fn prefs(&self) -> Prefs {
        let s = settings::get();
        Prefs {
            codec: s.codec,
            max_height: s.max_height,
            allow_60fps: s.allow_60fps,
            hw_decode: s.hw_decode,
            audio_only: self.imp().audio_only.get(),
        }
    }

    pub fn quality_options(&self) -> Vec<QualityOption> {
        match self.stream_info() {
            Some(info) => formats::quality_options(&info, &self.support(), &self.prefs()),
            None => Vec::new(),
        }
    }

    pub fn quality_override(&self) -> Option<u32> {
        self.imp().quality.get()
    }

    fn set_meta(&self, item: &VideoItem) {
        let imp = self.imp();
        imp.video_id.replace(item.id.clone());
        imp.title.replace(item.title.clone());
        imp.channel_name
            .replace(item.channel_name.clone().unwrap_or_default());
        imp.thumbnail.replace(item.thumbnail_url(640));
        self.notify_video_id();
        self.notify_title();
        self.notify_channel_name();
        self.notify_thumbnail();
    }

    fn set_error(&self, e: &str) {
        if e.is_empty() {
            self.imp().recovery.cancel();
        }
        self.imp().error.replace(e.to_owned());
        self.notify_error();
    }

    /// Show a failure. `network` marks failures that loading again can fix once
    /// the network is back.
    fn fail(&self, msg: &str, network: bool) {
        let offline = network && !Recovery::online();
        self.set_error(if offline {
            "No internet connection. Playback resumes when you're back online."
        } else {
            msg
        });
        if network {
            self.imp().recovery.wait_for_network();
        } else {
            self.imp().recovery.cancel();
        }
    }

    /// Showing an error instead of playing.
    pub fn failed(&self) -> bool {
        !self.imp().error.borrow().is_empty()
    }

    fn set_loading(&self, l: bool) {
        if self.imp().loading_streams.get() != l {
            self.imp().loading_streams.set(l);
            self.notify_loading_streams();
        }
    }

    /// Open a video. `playlist` attaches a queue; `start` overrides the resume position.
    pub fn open(&self, item: VideoItem, playlist: Option<String>, start: Option<f64>) {
        let imp = self.imp();
        self.flush_progress(false);
        imp.player.stop();
        let ticket = imp.generation.get() + 1;
        imp.generation.set(ticket);
        imp.details.replace(None);
        imp.info.replace(None);
        imp.selection.replace(None);
        imp.quality.set(None);
        imp.tracking.replace(None);
        imp.recovery.reset();
        imp.local_file.replace(None);
        imp.offline.set(false);
        self.notify_offline();
        if playlist.is_none() {
            imp.queue.replace(None);
        } else if let Some(q) = imp.queue.borrow_mut().as_mut() {
            if Some(&q.playlist_id) == playlist.as_ref() {
                if let Some(i) = q.items.iter().position(|v| v.id == item.id) {
                    q.index = i;
                }
            }
        }
        {
            let mut played = imp.played.borrow_mut();
            played.retain(|i| i != &item.id);
            played.push(item.id.clone());
            if played.len() > 200 {
                played.remove(0);
            }
        }
        imp.item.replace(Some(item.clone()));
        self.set_meta(&item);
        self.set_error("");
        if !imp.active.get() {
            imp.active.set(true);
            self.notify_active();
        }
        self.emit_by_name::<()>("video-changed", &[]);

        self.load_details(playlist);
        let start = start.or_else(|| {
            settings::get()
                .resume_playback
                .then(|| saved_position(&item.id))
                .flatten()
        });
        self.load_streams(start, true);
    }

    /// Play a downloaded file.
    pub fn open_file(&self, item: VideoItem, path: &std::path::Path) {
        self.open_offline_inner(item.clone(), path);
    }

    fn open_offline_inner(&self, item: VideoItem, path: &std::path::Path) {
        let imp = self.imp();
        self.flush_progress(false);
        imp.player.stop();
        let ticket = imp.generation.get() + 1;
        imp.generation.set(ticket);
        imp.details.replace(None);
        imp.info.replace(None);
        imp.selection.replace(None);
        imp.queue.replace(None);
        imp.tracking.replace(None);
        let uri = glib::filename_to_uri(path, None).map(|u| u.to_string()).unwrap_or_default();
        imp.local_file.replace(Some(uri.clone()));
        imp.offline.set(true);
        self.notify_offline();
        imp.item.replace(Some(item.clone()));
        self.set_meta(&item);
        self.set_error("");
        if !imp.active.get() {
            imp.active.set(true);
            self.notify_active();
        }
        self.emit_by_name::<()>("video-changed", &[]);
        let start = settings::get().resume_playback.then(|| saved_position(&item.id)).flatten();
        imp.player.load(LoadRequest {
            source: Source::Single { uri },
            headers: Vec::new(),
            start,
            autoplay: true,
        });
        self.emit_by_name::<()>("streams-loaded", &[]);
        // Details are a bonus when online.
        let id = item.id.clone();
        let this = self.downgrade();
        glib::spawn_future_local(async move {
            let res = runtime::run(async move { api::client().video(&id, None).await }).await;
            if let (Some(this), Ok(d)) = (this.upgrade(), res) {
                if this.imp().generation.get() == ticket {
                    this.apply_details(d);
                }
            }
        });
    }

    /// Metadata, comments, related videos.
    fn load_details(&self, playlist: Option<String>) {
        let Some(id) = self.current_item().map(|i| i.id) else { return };
        let ticket = self.imp().generation.get();
        let this = self.downgrade();
        glib::spawn_future_local(async move {
            let id2 = id.clone();
            let res = runtime::run(async move { api::client().video(&id2, playlist.as_deref()).await }).await;
            let Some(this) = this.upgrade() else { return };
            if this.imp().generation.get() != ticket {
                return;
            }
            match res {
                Ok(d) => this.apply_details(d),
                Err(e) => {
                    log::warn!("details for {id} failed: {e:#}");
                    if this.imp().info.borrow().is_none() && !this.imp().loading_streams.get() && !this.failed() {
                        this.fail(&e.to_string(), true);
                    }
                }
            }
        });
    }

    fn apply_details(&self, d: VideoDetails) {
        let imp = self.imp();
        // Fill in metadata the card didn't have (e.g. when opened from a link).
        {
            let mut item = imp.item.borrow_mut();
            if let Some(item) = item.as_mut() {
                if item.title.is_empty() {
                    item.title = d.title.clone();
                }
                if item.channel_name.is_none() {
                    item.channel_name = Some(d.channel.name.clone());
                    item.channel_id = Some(d.channel.id.clone());
                }
            }
        }
        if let Some(item) = imp.item.borrow().clone() {
            self.set_meta(&item);
        }
        if let Some(pl) = &d.playlist {
            if !pl.items.is_empty() {
                let index = pl
                    .current_index
                    .or_else(|| pl.items.iter().position(|v| v.id == d.id))
                    .unwrap_or(0);
                imp.queue.replace(Some(Queue {
                    playlist_id: pl.id.clone(),
                    title: pl.title.clone(),
                    items: pl.items.clone(),
                    index,
                }));
            }
        }
        imp.details.replace(Some(d));
        self.emit_by_name::<()>("details-loaded", &[]);
    }

    fn cookie_file() -> Option<std::path::PathBuf> {
        let s = settings::get();
        if !s.use_account_for_streams {
            return None;
        }
        let session = common::session::get().filter(|s| s.is_signed_in())?;
        auth::write_cookie_file(&session, "video").ok()
    }

    fn load_streams(&self, start: Option<f64>, autoplay: bool) {
        let imp = self.imp();
        let Some(item) = imp.item.borrow().clone() else { return };
        let ticket = imp.generation.get();
        self.set_loading(true);
        let id = item.id.clone();
        let cookies = Self::cookie_file();
        let this = self.downgrade();
        glib::spawn_future_local(async move {
            let id2 = id.clone();
            let req = extractor::Request::video(cookies);
            let res = runtime::run(async move { extractor::extractor().extract(&id2, &req).await }).await;
            let Some(this) = this.upgrade() else { return };
            if this.imp().generation.get() != ticket {
                return;
            }
            this.set_loading(false);
            match res {
                Ok(info) => {
                    this.imp().info.replace(Some(info.clone()));
                    this.start_playback(start, autoplay);
                    this.setup_tracking(&info);
                }
                Err(e) => {
                    log::warn!("stream extraction for {id} failed: {e:#}");
                    if e.downcast_ref::<extractor::NetworkError>().is_some() {
                        this.fail("Can't reach YouTube. Check your connection.", true);
                    } else {
                        this.fail(&e.to_string(), false);
                    }
                }
            }
        });
    }

    fn start_playback(&self, start: Option<f64>, autoplay: bool) {
        let imp = self.imp();
        let Some(info) = self.stream_info() else { return };
        let sel = match formats::select(&info, &self.support(), &self.prefs(), imp.quality.get()) {
            Ok(s) => s,
            Err(e) => {
                self.fail(&e.to_string(), false);
                return;
            }
        };
        // Whatever failed before, this is a fresh start.
        self.set_error("");
        let (source, headers) = match &sel {
            Selection::Separate { video, audio } => {
                // MP4 streams can only seek through a DASH manifest (range requests via
                // the sidx index); WebM seeks fine as plain HTTP using its Cues.
                let use_dash = formats::is_mp4(video) && formats::is_mp4(audio);
                let manifest = if use_dash { player::dash::write_manifest(&info, video, audio) } else { None };
                let source = match manifest {
                    Some(path) => Source::Single {
                        uri: glib::filename_to_uri(&path, None)
                            .map(|u| u.to_string())
                            .unwrap_or_default(),
                    },
                    None => {
                        if use_dash {
                            log::info!("no DASH ranges for {}, using plain HTTP streams", info.id);
                        }
                        Source::Separate {
                            video: video.url.clone(),
                            audio: audio.url.clone(),
                        }
                    }
                };
                (source, player::headers_from_map(&video.http_headers))
            }
            Selection::Muxed(f) | Selection::AudioOnly(f) => (
                Source::Single { uri: f.url.clone() },
                player::headers_from_map(&f.http_headers),
            ),
            Selection::Manifest { url, headers } => (Source::Single { uri: url.clone() }, headers.clone()),
        };
        if let Some(v) = sel.video_format() {
            log::info!(
                "playing {} {}p{} via format {} + {}",
                v.vcodec.as_deref().unwrap_or("?"),
                v.height.unwrap_or(0),
                v.fps.map(|f| f.round() as u32).unwrap_or(0),
                v.format_id,
                sel.audio_format().map(|a| a.format_id.as_str()).unwrap_or("-")
            );
        }
        imp.selection.replace(Some(sel));
        imp.player.load(LoadRequest {
            source,
            headers,
            start,
            autoplay,
        });
        self.emit_by_name::<()>("streams-loaded", &[]);
    }

    /// Switch quality (None = automatic) keeping the position.
    pub fn set_quality(&self, height: Option<u32>) {
        let imp = self.imp();
        imp.quality.set(height);
        if imp.local_file.borrow().is_some() || self.stream_info().is_none() {
            return;
        }
        let pos = imp.player.position();
        let playing = imp.player.is_playing_intent();
        self.start_playback(Some(pos), playing);
    }

    pub fn set_audio_only(&self, audio_only: bool) {
        let imp = self.imp();
        if imp.audio_only.get() == audio_only {
            return;
        }
        imp.audio_only.set(audio_only);
        self.notify_audio_only();
        if imp.local_file.borrow().is_none() && self.stream_info().is_some() {
            let pos = imp.player.position();
            let playing = imp.player.is_playing_intent();
            self.start_playback(Some(pos), playing);
        }
    }

    /// Load the current video again (fresh stream URLs), from where it stopped.
    pub fn retry(&self) {
        self.retry_from(self.imp().player.position());
    }

    fn retry_from(&self, pos: f64) {
        let imp = self.imp();
        self.set_error("");
        if let Some(uri) = imp.local_file.borrow().clone() {
            imp.player.load(LoadRequest {
                source: Source::Single { uri },
                headers: Vec::new(),
                start: Some(pos),
                autoplay: true,
            });
            return;
        }
        let Some(item) = self.current_item() else { return };
        extractor::extractor().invalidate(&item.id);
        if imp.details.borrow().is_none() {
            let playlist = imp.queue.borrow().as_ref().map(|q| q.playlist_id.clone());
            self.load_details(playlist);
        }
        self.load_streams(Some(pos), true);
    }

    /// Play/pause; after a failure, tries loading again instead.
    pub fn toggle(&self) {
        if self.failed() {
            self.retry();
        } else {
            self.imp().player.toggle();
        }
    }

    pub fn play(&self) {
        if self.failed() {
            self.retry();
        } else {
            self.imp().player.play();
        }
    }

    pub fn seek(&self, t: f64) {
        if self.failed() {
            self.retry_from(t);
            return;
        }
        self.imp().player.seek(t, true);
        self.imp().player.play();
    }

    pub fn stop(&self) {
        let imp = self.imp();
        self.flush_progress(false);
        imp.generation.set(imp.generation.get() + 1);
        imp.player.stop();
        imp.item.replace(None);
        imp.details.replace(None);
        imp.info.replace(None);
        imp.queue.replace(None);
        imp.local_file.replace(None);
        imp.active.set(false);
        self.notify_active();
        self.set_loading(false);
        self.set_error("");
        self.emit_by_name::<()>("video-changed", &[]);
    }

    // -----------------------------------------------------------------------
    // Queue / autoplay
    // -----------------------------------------------------------------------

    fn next_item(&self) -> Option<(VideoItem, Option<String>)> {
        let imp = self.imp();
        if let Some(q) = imp.queue.borrow().as_ref() {
            if let Some(next) = q.items.get(q.index + 1) {
                return Some((next.clone(), Some(q.playlist_id.clone())));
            }
            return None;
        }
        let played = imp.played.borrow();
        let details = imp.details.borrow();
        details.as_ref()?.related.iter().find_map(|i| match i {
            Item::Video(v) if !played.contains(&v.id) && !v.live && !v.upcoming => Some((v.clone(), None)),
            _ => None,
        })
    }

    pub fn has_next(&self) -> bool {
        self.next_item().is_some()
    }

    pub fn has_previous(&self) -> bool {
        self.imp().queue.borrow().as_ref().is_some_and(|q| q.index > 0)
    }

    pub fn next(&self) -> bool {
        match self.next_item() {
            Some((item, pl)) => {
                self.open(item, pl, None);
                true
            }
            None => false,
        }
    }

    pub fn previous(&self) -> bool {
        let prev = self
            .imp()
            .queue
            .borrow()
            .as_ref()
            .and_then(|q| q.index.checked_sub(1).and_then(|i| q.items.get(i).cloned()).map(|v| (v, q.playlist_id.clone())));
        match prev {
            Some((item, pl)) => {
                self.open(item, Some(pl), Some(0.0));
                true
            }
            None => {
                self.imp().player.seek(0.0, true);
                false
            }
        }
    }

    fn on_eos(&self) {
        if let Some(item) = self.current_item() {
            save_position(&item.id, 0.0, 0.0);
        }
        let in_queue = self.imp().queue.borrow().is_some();
        if in_queue || settings::get().autoplay {
            self.next();
        }
    }

    fn on_error(&self, msg: String, transient: bool) {
        let imp = self.imp();
        let first_line = msg.lines().next().unwrap_or("Playback failed").to_owned();
        let network = transient && imp.local_file.borrow().is_none();
        // Expired URLs, flaky CDN responses (e.g. a server ignoring range
        // requests) and dropped connections usually go away with fresh URLs
        // from a new extraction. Offline, wait for the network instead.
        if network && imp.recovery.retry_now() {
            log::info!("stream failed ({first_line}), extracting again");
            self.retry();
            return;
        }
        self.fail(&first_line, network);
    }

    /// The network is back after a failure that waited for it.
    fn on_network_available(&self) {
        let imp = self.imp();
        if self.failed() && imp.active.get() {
            log::info!("network is back, loading {} again", imp.video_id.borrow());
            self.retry();
        }
    }

    // -----------------------------------------------------------------------
    // Progress: local resume positions + YouTube watch history
    // -----------------------------------------------------------------------

    fn setup_tracking(&self, info: &StreamInfo) {
        let s = settings::get();
        if !s.send_watch_history || !api::client().is_signed_in() {
            return;
        }
        let Some(t) = info.tracking.as_ref() else { return };
        let tracker = Tracker::new(api::client(), t, self.imp().player.position());
        self.imp().tracking.replace(Some(tracker));
    }

    fn on_tick(&self) {
        let imp = self.imp();
        let player = &imp.player;
        if player.state() != PlayState::Playing {
            return;
        }
        let pos = player.position();
        if (pos - imp.last_saved.get()).abs() >= 10.0 {
            imp.last_saved.set(pos);
            if let Some(item) = self.current_item() {
                save_position(&item.id, pos, player.duration());
            }
        }
        if let Some(t) = imp.tracking.borrow_mut().as_mut() {
            t.tick(pos, player.duration());
        }
    }

    /// Persist the position and report a final watch-time segment.
    fn flush_progress(&self, ended: bool) {
        let imp = self.imp();
        let Some(item) = self.current_item() else { return };
        let player = &imp.player;
        let pos = if ended { player.duration() } else { player.position() };
        if ended {
            save_position(&item.id, 0.0, 0.0);
        } else if player.duration() > 0.0 {
            save_position(&item.id, pos, player.duration());
        }
        if let Some(t) = imp.tracking.borrow_mut().as_mut() {
            t.flush(pos, player.duration());
        }
    }
}
