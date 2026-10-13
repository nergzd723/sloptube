//! What is playing: the queue, radio continuations, shuffle and repeat,
//! stream prefetching for instant skips and gapless playback, retries, and
//! the queue saved between sessions.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::OnceLock;

use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::api::{self, Track, WatchTarget};
use crate::config;
use crate::extractor::{self, extractor};
use crate::library::Library;
use crate::player::{self, LoadRequest, NextStream, PlayState, Player, Source};
use crate::runtime;
use crate::settings::{self, Repeat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, glib::Enum)]
#[enum_type(name = "StmRepeatMode")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl From<Repeat> for RepeatMode {
    fn from(r: Repeat) -> Self {
        match r {
            Repeat::Off => RepeatMode::Off,
            Repeat::All => RepeatMode::All,
            Repeat::One => RepeatMode::One,
        }
    }
}

impl From<RepeatMode> for Repeat {
    fn from(r: RepeatMode) -> Self {
        match r {
            RepeatMode::Off => Repeat::Off,
            RepeatMode::All => Repeat::All,
            RepeatMode::One => Repeat::One,
        }
    }
}

/// Lyrics and related-content browse ids of a track.
#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub lyrics: Option<String>,
    pub related: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Session {
    tracks: Vec<Track>,
    current: usize,
    position: f64,
    #[serde(default)]
    continuation: Option<String>,
    #[serde(default)]
    title: String,
}

fn session_path() -> std::path::PathBuf {
    config::data_dir().join("queue.json")
}

fn looks_expired(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    ["403", "410", "forbidden", "not found", "could not read", "timed out", "timeout", "connection", "reset"]
        .iter()
        .any(|k| m.contains(k))
}

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::Playback)]
    pub struct Playback {
        pub player: Player,
        pub queue: gio::ListStore,
        /// Something is queued (playing, paused or about to play).
        #[property(get)]
        pub active: Cell<bool>,
        /// Index of the current track in the queue, -1 if none.
        #[property(get)]
        pub current: Cell<i32>,
        #[property(get)]
        pub title: RefCell<String>,
        #[property(get)]
        pub artist: RefCell<String>,
        #[property(get)]
        pub album: RefCell<String>,
        /// Artwork URL (large).
        #[property(get)]
        pub art: RefCell<String>,
        #[property(get)]
        pub video_id: RefCell<String>,
        /// The stream of the current track is being resolved.
        #[property(get)]
        pub loading: Cell<bool>,
        #[property(get, set = Self::set_shuffle_value)]
        pub shuffle: Cell<bool>,
        #[property(get, set = Self::set_repeat_value, builder(RepeatMode::default()))]
        pub repeat: Cell<RepeatMode>,
        /// What the queue was started from ("Random Access Memories", "Radio", ...).
        #[property(get)]
        pub queue_title: RefCell<String>,

        pub unshuffled: RefCell<Option<Vec<Track>>>,
        pub continuation: RefCell<Option<String>>,
        /// Bumped whenever the current track (re)starts; stale async results check it.
        pub generation: Cell<u64>,
        /// Bumped whenever a new queue starts.
        pub queue_gen: Cell<u64>,
        pub retries: Cell<u32>,
        pub failures: Cell<u32>,
        pub prepared: RefCell<Option<(u64, String)>>,
        pub token: Cell<u64>,
        pub extending: Cell<bool>,
        pub resume_at: Cell<Option<f64>>,
        pub counted: Cell<bool>,
        pub info: RefCell<HashMap<String, TrackInfo>>,
        pub volume_save: RefCell<Option<glib::SourceId>>,
    }

    impl Default for Playback {
        fn default() -> Self {
            Self {
                player: Player::new(player::CONFIG),
                queue: gio::ListStore::new::<glib::BoxedAnyObject>(),
                active: Default::default(),
                current: Cell::new(-1),
                title: Default::default(),
                artist: Default::default(),
                album: Default::default(),
                art: Default::default(),
                video_id: Default::default(),
                loading: Default::default(),
                shuffle: Default::default(),
                repeat: Default::default(),
                queue_title: Default::default(),
                unshuffled: Default::default(),
                continuation: Default::default(),
                generation: Default::default(),
                queue_gen: Default::default(),
                retries: Default::default(),
                failures: Default::default(),
                prepared: Default::default(),
                token: Default::default(),
                extending: Default::default(),
                resume_at: Default::default(),
                counted: Default::default(),
                info: Default::default(),
                volume_save: Default::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Playback {
        const NAME: &'static str = "StmPlayback";
        type Type = super::Playback;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Playback {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("track-changed").build(),
                    Signal::builder("message").param_types([String::static_type()]).build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl Playback {
        fn set_shuffle_value(&self, on: bool) {
            if self.shuffle.get() == on {
                return;
            }
            self.shuffle.set(on);
            self.obj().reshuffle(on);
            self.obj().notify_shuffle();
        }

        fn set_repeat_value(&self, r: RepeatMode) {
            if self.repeat.get() == r {
                return;
            }
            self.repeat.set(r);
            settings::update(|s| s.repeat = r.into());
            self.obj().notify_repeat();
            self.obj().prepare_next();
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
    fn setup(&self) {
        let imp = self.imp();
        let s = settings::get();
        imp.player.set_volume(s.volume);
        imp.player.set_muted(s.muted);
        imp.repeat.set(s.repeat.into());

        let player = &imp.player;
        player.connect_eos(glib::clone!(
            #[weak(rename_to = pb)]
            self,
            move |_| pb.on_eos()
        ));
        player.connect_error(glib::clone!(
            #[weak(rename_to = pb)]
            self,
            move |_, msg, _| pb.on_error(&msg)
        ));
        player.connect_advanced(glib::clone!(
            #[weak(rename_to = pb)]
            self,
            move |_, token| pb.on_advanced(token)
        ));
        player.connect_state_notify(glib::clone!(
            #[weak(rename_to = pb)]
            self,
            move |p| {
                let imp = pb.imp();
                match p.state() {
                    PlayState::Playing => {
                        pb.set_loading(false);
                        imp.failures.set(0);
                        if !imp.counted.replace(true)
                            && let Some(t) = pb.current_track() {
                                Library::get().add_history(&t);
                                pb.report_play(&t.video_id);
                            }
                    }
                    PlayState::Paused => {
                        pb.set_loading(false);
                        pb.save_session();
                    }
                    PlayState::Error => pb.set_loading(false),
                    _ => {}
                }
            }
        ));
        let save_volume = glib::clone!(
            #[weak(rename_to = pb)]
            self,
            move |p: &Player| {
                // Volume sliders fire continuously; save once they settle.
                if let Some(id) = pb.imp().volume_save.take() {
                    id.remove();
                }
                let (v, m) = (p.volume(), p.muted());
                let id = glib::timeout_add_local_once(std::time::Duration::from_millis(600), glib::clone!(
                    #[weak]
                    pb,
                    move || {
                        pb.imp().volume_save.take();
                        settings::update(|s| {
                            s.volume = v;
                            s.muted = m;
                        });
                    }
                ));
                pb.imp().volume_save.replace(Some(id));
            }
        );
        let sv = save_volume.clone();
        player.connect_volume_notify(move |p| sv(p));
        player.connect_muted_notify(move |p| save_volume(p));
    }

    pub fn player(&self) -> Player {
        self.imp().player.clone()
    }

    /// The queue as a list model of `BoxedAnyObject(Track)`.
    pub fn queue(&self) -> gio::ListStore {
        self.imp().queue.clone()
    }

    pub fn track_at(&self, index: u32) -> Option<Track> {
        self.imp()
            .queue
            .item(index)
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|o| o.borrow::<Track>().clone())
    }

    pub fn current_track(&self) -> Option<Track> {
        let c = self.current();
        (c >= 0).then(|| self.track_at(c as u32)).flatten()
    }

    fn tracks(&self) -> Vec<Track> {
        (0..self.imp().queue.n_items()).filter_map(|i| self.track_at(i)).collect()
    }

    fn objects(tracks: Vec<Track>) -> Vec<glib::BoxedAnyObject> {
        tracks.into_iter().map(glib::BoxedAnyObject::new).collect()
    }

    fn set_tracks(&self, tracks: Vec<Track>) {
        let q = &self.imp().queue;
        q.splice(0, q.n_items(), &Self::objects(tracks));
    }

    fn set_loading(&self, loading: bool) {
        if self.imp().loading.replace(loading) != loading {
            self.notify_loading();
        }
    }

    fn set_active(&self, active: bool) {
        if self.imp().active.replace(active) != active {
            self.notify_active();
        }
    }

    fn set_queue_title(&self, title: &str) {
        self.imp().queue_title.replace(title.to_owned());
        self.notify_queue_title();
    }

    /// Move the current marker (and refresh the rows that show it).
    fn set_current(&self, index: i32) {
        let imp = self.imp();
        let old = imp.current.replace(index);
        let n = imp.queue.n_items() as i32;
        for i in [old, index] {
            if i >= 0 && i < n {
                imp.queue.items_changed(i as u32, 1, 1);
            }
        }
        if old != index {
            self.notify_current();
        }
        self.update_metadata();
    }

    fn update_metadata(&self) {
        let imp = self.imp();
        let t = self.current_track().unwrap_or_default();
        imp.title.replace(t.title.clone());
        imp.artist.replace(t.artist_names());
        imp.album.replace(t.album.as_ref().map(|a| a.name.clone()).unwrap_or_default());
        imp.art.replace(api::thumb_url(&t.thumbnails, 544).unwrap_or_default());
        imp.video_id.replace(t.video_id.clone());
        self.notify_title();
        self.notify_artist();
        self.notify_album();
        self.notify_art();
        self.notify_video_id();
        self.emit_by_name::<()>("track-changed", &[]);
    }

    fn message(&self, text: &str) {
        self.emit_by_name::<()>("message", &[&text]);
    }

    pub fn connect_track_changed(&self, f: impl Fn(&Self) + 'static) -> glib::SignalHandlerId {
        self.connect_local("track-changed", false, move |args| {
            f(&args[0].get::<Self>().unwrap());
            None
        })
    }

    pub fn connect_message(&self, f: impl Fn(&Self, &str) + 'static) -> glib::SignalHandlerId {
        self.connect_local("message", false, move |args| {
            f(&args[0].get::<Self>().unwrap(), &args[1].get::<String>().unwrap_or_default());
            None
        })
    }

    // -----------------------------------------------------------------------
    // Starting queues
    // -----------------------------------------------------------------------

    fn begin_queue(&self, mut tracks: Vec<Track>, index: usize, title: &str) {
        let imp = self.imp();
        tracks.retain(|t| !t.unavailable && !t.video_id.is_empty());
        if tracks.is_empty() {
            self.message("Nothing to play");
            return;
        }
        let index = index.min(tracks.len() - 1);
        imp.queue_gen.set(imp.queue_gen.get() + 1);
        imp.continuation.take();
        imp.extending.set(false);
        imp.unshuffled.take();
        imp.resume_at.take();
        if imp.shuffle.get() {
            imp.unshuffled.replace(Some(tracks.clone()));
            tracks[index + 1..].shuffle(&mut rand::rng());
        }
        imp.current.set(-1);
        self.set_tracks(tracks);
        self.set_queue_title(title);
        self.set_active(true);
        self.set_current(index as i32);
        self.start_current(true, None);
    }

    /// Play a song and continue with its radio, like YouTube Music does.
    pub fn play_song(&self, track: Track) {
        log::debug!("play_song {} ({})", track.video_id, track.title);
        let id = track.video_id.clone();
        self.begin_queue(vec![track], 0, "Radio");
        let queue_gen = self.imp().queue_gen.get();
        let target = WatchTarget {
            video_id: Some(id.clone()),
            ..Default::default()
        };
        let pb = self.downgrade();
        glib::spawn_future_local(async move {
            let res = runtime::run(async move { api::client().queue(&target).await }).await;
            let Some(pb) = pb.upgrade() else { return };
            let imp = pb.imp();
            if imp.queue_gen.get() != queue_gen {
                return;
            }
            match res {
                Ok(q) => {
                    imp.info.borrow_mut().insert(
                        id.clone(),
                        TrackInfo {
                            lyrics: q.lyrics.clone(),
                            related: q.related.clone(),
                        },
                    );
                    let start = q
                        .selected
                        .or_else(|| q.tracks.iter().position(|t| t.video_id == id))
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    let rest: Vec<Track> = q.tracks.into_iter().skip(start).filter(|t| t.video_id != id).collect();
                    imp.continuation.replace(q.continuation);
                    pb.append_tracks(rest);
                }
                Err(e) => log::warn!("radio for {id} failed: {e:#}"),
            }
        });
    }

    /// Keep the current song playing and replace what follows with its radio.
    pub fn radio_here(&self) {
        let Some(track) = self.current_track() else { return };
        let imp = self.imp();
        imp.queue_gen.set(imp.queue_gen.get() + 1);
        let queue_gen = imp.queue_gen.get();
        let id = track.video_id.clone();
        let target = WatchTarget {
            video_id: Some(id.clone()),
            ..Default::default()
        };
        let pb = self.downgrade();
        glib::spawn_future_local(async move {
            let res = runtime::run(async move { api::client().queue(&target).await }).await;
            let Some(pb) = pb.upgrade() else { return };
            let imp = pb.imp();
            if imp.queue_gen.get() != queue_gen || pb.video_id() != id {
                return;
            }
            match res {
                Ok(q) => {
                    let cur = pb.current().max(0) as u32;
                    let n = imp.queue.n_items();
                    imp.queue.splice(cur + 1, n - cur - 1, &[] as &[glib::BoxedAnyObject]);
                    imp.unshuffled.take();
                    if imp.shuffle.replace(false) {
                        pb.notify_shuffle();
                    }
                    let rest: Vec<Track> = q.tracks.into_iter().filter(|t| t.video_id != id).collect();
                    imp.continuation.replace(q.continuation);
                    pb.set_queue_title("Radio");
                    pb.append_tracks(rest);
                    pb.prepare_next();
                }
                Err(e) => pb.message(&format!("Couldn't start the radio: {e}")),
            }
        });
    }

    /// Play these tracks from `index` (an album, a playlist, liked songs...).
    pub fn play_tracks(&self, tracks: Vec<Track>, index: usize, title: &str) {
        self.begin_queue(tracks, index, title);
    }

    /// Play these tracks in random order.
    pub fn shuffle_tracks(&self, mut tracks: Vec<Track>, title: &str) {
        tracks.retain(|t| !t.unavailable);
        if tracks.is_empty() {
            return;
        }
        self.imp().shuffle.set(true);
        self.notify_shuffle();
        let start = rand::random_range(0..tracks.len());
        tracks.swap(0, start);
        self.begin_queue(tracks, 0, title);
    }

    /// Start a playlist, mix or radio by id.
    pub fn play_watch(&self, target: WatchTarget, title: &str) {
        let imp = self.imp();
        imp.queue_gen.set(imp.queue_gen.get() + 1);
        let queue_gen = imp.queue_gen.get();
        self.set_loading(true);
        let title = title.to_owned();
        let pb = self.downgrade();
        glib::spawn_future_local(async move {
            let res = runtime::run(async move { api::client().queue(&target).await }).await;
            let Some(pb) = pb.upgrade() else { return };
            if pb.imp().queue_gen.get() != queue_gen {
                return;
            }
            match res {
                Ok(q) if !q.tracks.is_empty() => {
                    let index = q.selected.unwrap_or(0);
                    let cont = q.continuation.clone();
                    pb.begin_queue(q.tracks, index, &title);
                    pb.imp().continuation.replace(cont);
                }
                Ok(_) => {
                    pb.set_loading(false);
                    pb.message("This playlist is empty");
                }
                Err(e) => {
                    pb.set_loading(false);
                    pb.message(&format!("Couldn't start playback: {e}"));
                }
            }
        });
    }

    /// Insert tracks right after the current one.
    pub fn play_next(&self, tracks: Vec<Track>) {
        if !self.active() || self.current() < 0 {
            self.begin_queue(tracks, 0, "");
            return;
        }
        let at = (self.current() + 1) as u32;
        if let Some(u) = self.imp().unshuffled.borrow_mut().as_mut() {
            u.extend(tracks.iter().cloned());
        }
        self.imp().queue.splice(at, 0, &Self::objects(tracks));
        self.prepare_next();
    }

    pub fn add_to_queue(&self, tracks: Vec<Track>) {
        if !self.active() || self.current() < 0 {
            self.begin_queue(tracks, 0, "");
            return;
        }
        self.append_tracks(tracks);
    }

    fn append_tracks(&self, tracks: Vec<Track>) {
        let imp = self.imp();
        let was_last = self.current() + 1 >= imp.queue.n_items() as i32;
        if let Some(u) = imp.unshuffled.borrow_mut().as_mut() {
            u.extend(tracks.iter().cloned());
        }
        imp.queue.extend_from_slice(&Self::objects(tracks));
        if was_last {
            self.prepare_next();
        }
        self.save_session();
    }

    pub fn remove_at(&self, index: u32) {
        let imp = self.imp();
        let cur = self.current();
        if index as i32 == cur || index >= imp.queue.n_items() {
            return;
        }
        if let (Some(t), Some(u)) = (self.track_at(index), imp.unshuffled.borrow_mut().as_mut())
            && let Some(p) = u.iter().position(|x| x.video_id == t.video_id) {
                u.remove(p);
            }
        imp.queue.remove(index);
        if (index as i32) < cur {
            imp.current.set(cur - 1);
            self.notify_current();
        }
        if index as i32 == cur + 1 {
            self.prepare_next();
        }
    }

    /// Move a queued track to another position (drag and drop in Up next).
    pub fn move_item(&self, from: u32, to: u32) {
        let imp = self.imp();
        let n = imp.queue.n_items();
        if from >= n || to >= n || from == to {
            return;
        }
        let Some(obj) = imp.queue.item(from) else { return };
        imp.queue.remove(from);
        imp.queue.insert(to, &obj);
        let cur = self.current() as u32;
        let new_cur = if from == cur {
            to
        } else if from < cur && to >= cur {
            cur - 1
        } else if from > cur && to <= cur {
            cur + 1
        } else {
            cur
        };
        if new_cur != cur {
            imp.current.set(new_cur as i32);
            self.notify_current();
        }
        self.prepare_next();
    }

    pub fn play_index(&self, index: u32) {
        if index >= self.imp().queue.n_items() {
            return;
        }
        if index as i32 == self.current() && self.imp().player.state() != PlayState::Idle {
            self.imp().player.play();
            return;
        }
        self.set_current(index as i32);
        self.start_current(true, None);
    }

    pub fn stop(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        imp.queue_gen.set(imp.queue_gen.get() + 1);
        imp.player.stop();
        imp.prepared.take();
        imp.continuation.take();
        imp.unshuffled.take();
        imp.current.set(-1);
        imp.queue.remove_all();
        self.set_loading(false);
        self.set_active(false);
        self.update_metadata();
        let _ = std::fs::remove_file(session_path());
    }

    // -----------------------------------------------------------------------
    // Transport
    // -----------------------------------------------------------------------

    /// Play/pause; a restored queue starts its stream only now.
    pub fn toggle(&self) {
        let imp = self.imp();
        match imp.player.state() {
            PlayState::Idle | PlayState::Error if self.active() && self.current() >= 0 => {
                if imp.loading.get() {
                    return;
                }
                let start = imp.resume_at.take();
                self.start_current(true, start);
            }
            _ => imp.player.toggle(),
        }
    }

    pub fn is_playing(&self) -> bool {
        let p = &self.imp().player;
        p.is_playing_intent() || self.imp().loading.get()
    }

    pub fn seek(&self, pos: f64) {
        let imp = self.imp();
        if imp.player.state() == PlayState::Idle {
            imp.resume_at.set(Some(pos));
            return;
        }
        imp.player.seek(pos, true);
    }

    fn next_index(&self, auto: bool) -> Option<u32> {
        let n = self.imp().queue.n_items();
        let cur = self.current();
        if cur < 0 || n == 0 {
            return None;
        }
        let repeat = self.repeat();
        if auto && repeat == RepeatMode::One {
            return Some(cur as u32);
        }
        if ((cur + 1) as u32) < n {
            Some((cur + 1) as u32)
        } else if repeat != RepeatMode::Off {
            Some(0)
        } else {
            None
        }
    }

    pub fn has_next(&self) -> bool {
        self.next_index(false).is_some() || self.imp().continuation.borrow().is_some()
    }

    pub fn has_previous(&self) -> bool {
        self.current() >= 0
    }

    pub fn next(&self) {
        match self.next_index(false) {
            Some(i) => {
                self.set_current(i as i32);
                self.start_current(true, None);
            }
            None => self.extend_then_advance(),
        }
    }

    pub fn previous(&self) {
        let p = &self.imp().player;
        if p.position() > 3.0 || self.current() <= 0 {
            self.seek(0.0);
            if !p.is_playing_intent() {
                self.toggle();
            }
            return;
        }
        self.set_current(self.current() - 1);
        self.start_current(true, None);
    }

    fn on_eos(&self) {
        match self.next_index(true) {
            Some(i) if i as i32 == self.current() => self.imp().player.play(),
            Some(i) => {
                self.set_current(i as i32);
                self.start_current(true, None);
            }
            None => self.extend_then_advance(),
        }
    }

    /// At the end of the queue: load more of the radio (or start one) and go on.
    fn extend_then_advance(&self) {
        let has_more = self.imp().continuation.borrow().is_some();
        if !has_more && !settings::get().autoplay {
            return;
        }
        let before = self.imp().queue.n_items();
        let pb = self.downgrade();
        self.extend(Box::new(move || {
            let Some(pb) = pb.upgrade() else { return };
            if pb.imp().queue.n_items() > before && pb.current() + 1 == before as i32 {
                pb.set_current(before as i32);
                pb.start_current(true, None);
            }
        }));
    }

    /// Load more of the queue: the radio's continuation, or with autoplay a
    /// radio of the last track.
    fn extend(&self, then: Box<dyn FnOnce()>) {
        let imp = self.imp();
        if imp.extending.replace(true) {
            return;
        }
        let token = imp.continuation.borrow().clone();
        let last = self.track_at(imp.queue.n_items().saturating_sub(1));
        let queue_gen = imp.queue_gen.get();
        let pb = self.downgrade();
        glib::spawn_future_local(async move {
            let res = match (token, last) {
                (Some(t), _) => runtime::run(async move { api::client().queue_more(&t).await }).await,
                (None, Some(last)) if settings::get().autoplay => {
                    let target = WatchTarget {
                        video_id: Some(last.video_id.clone()),
                        ..Default::default()
                    };
                    runtime::run(async move { api::client().queue(&target).await }).await
                }
                _ => Ok(Default::default()),
            };
            let Some(pb) = pb.upgrade() else { return };
            let imp = pb.imp();
            imp.extending.set(false);
            if imp.queue_gen.get() != queue_gen {
                return;
            }
            match res {
                Ok(q) => {
                    let known: std::collections::HashSet<String> = pb.tracks().into_iter().map(|t| t.video_id).collect();
                    let fresh: Vec<Track> = q.tracks.into_iter().filter(|t| !known.contains(&t.video_id)).collect();
                    imp.continuation.replace(q.continuation);
                    if !fresh.is_empty() {
                        pb.append_tracks(fresh);
                    }
                    then();
                }
                Err(e) => log::warn!("couldn't extend the queue: {e:#}"),
            }
        });
    }

    /// Keep a few tracks ahead in a radio, so the end never arrives.
    fn maybe_extend(&self) {
        let imp = self.imp();
        let remaining = imp.queue.n_items() as i32 - 1 - self.current();
        let has_more = imp.continuation.borrow().is_some();
        let autoplay = settings::get().autoplay && self.repeat() == RepeatMode::Off;
        if (has_more && remaining <= 3) || (!has_more && autoplay && remaining == 0) {
            self.extend(Box::new(|| {}));
        }
    }

    // -----------------------------------------------------------------------
    // Streams
    // -----------------------------------------------------------------------

    fn start_current(&self, autoplay: bool, start: Option<f64>) {
        let imp = self.imp();
        let Some(track) = self.current_track() else { return };
        imp.generation.set(imp.generation.get() + 1);
        let generation = imp.generation.get();
        imp.counted.set(false);
        imp.prepared.take();
        imp.player.set_next(None);
        self.set_loading(true);
        // Don't let the previous song play on while this one resolves.
        if imp.player.state() != PlayState::Idle {
            imp.player.stop();
        }
        self.save_session();
        let id = track.video_id.clone();
        let req = extractor::request();
        let pb = self.downgrade();
        glib::spawn_future_local(async move {
            let res = runtime::run(async move { extractor().extract(&id, &req).await }).await;
            let Some(pb) = pb.upgrade() else { return };
            let imp = pb.imp();
            if imp.generation.get() != generation {
                return;
            }
            let quality = settings::get().quality;
            match res.as_ref().map(|info| extractor::select(info, quality)) {
                Ok(Some(f)) => {
                    log::info!("playing {} ({})", track.video_id, f.describe());
                    imp.player.load(LoadRequest {
                        source: Source::Single { uri: f.url.clone() },
                        headers: f.headers(),
                        start,
                        autoplay,
                    });
                    if !autoplay {
                        pb.set_loading(false);
                    }
                    pb.prepare_next();
                    pb.maybe_extend();
                }
                Ok(None) => pb.failed(&track, "no playable audio stream", autoplay),
                Err(e) => pb.failed(&track, &format!("{e}"), autoplay),
            }
        });
    }

    fn failed(&self, track: &Track, why: &str, autoplay: bool) {
        let imp = self.imp();
        self.set_loading(false);
        let failures = imp.failures.get() + 1;
        imp.failures.set(failures);
        let first_line = why.lines().next().unwrap_or(why);
        self.message(&format!("Can't play “{}”: {first_line}", track.title));
        // Skip ahead, unless everything keeps failing (offline, broken yt-dlp).
        if autoplay && failures < 3 && self.next_index(false).is_some() {
            let generation = imp.generation.get();
            let pb = self.downgrade();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                if let Some(pb) = pb.upgrade()
                    && pb.imp().generation.get() == generation {
                        pb.next();
                    }
            });
        }
    }

    fn on_error(&self, msg: &str) {
        let imp = self.imp();
        let Some(track) = self.current_track() else { return };
        if looks_expired(msg) && imp.retries.get() < 2 {
            imp.retries.set(imp.retries.get() + 1);
            log::warn!("retrying {} after: {msg}", track.video_id);
            extractor().invalidate(&track.video_id);
            let pos = imp.player.position();
            self.start_current(true, (pos > 1.0).then_some(pos));
            return;
        }
        imp.retries.set(0);
        self.failed(&track, msg, true);
    }

    /// Resolve the next track's stream now, so skipping is instant and the
    /// player can continue gaplessly.
    fn prepare_next(&self) {
        let imp = self.imp();
        let Some(next) = self.next_index(true).and_then(|i| self.track_at(i)) else {
            imp.prepared.take();
            imp.player.set_next(None);
            return;
        };
        if imp.prepared.borrow().as_ref().is_some_and(|(_, id)| *id == next.video_id) {
            return;
        }
        imp.prepared.take();
        imp.player.set_next(None);
        if imp.player.state() == PlayState::Idle {
            return;
        }
        imp.token.set(imp.token.get() + 1);
        let token = imp.token.get();
        let generation = imp.generation.get();
        let id = next.video_id.clone();
        let req = extractor::request();
        let pb = self.downgrade();
        glib::spawn_future_local(async move {
            let res = runtime::run({
                let id = id.clone();
                async move { extractor().extract(&id, &req).await }
            })
            .await;
            let Some(pb) = pb.upgrade() else { return };
            let imp = pb.imp();
            if imp.generation.get() != generation || imp.token.get() != token {
                return;
            }
            let still_next = pb.next_index(true).and_then(|i| pb.track_at(i)).is_some_and(|t| t.video_id == id);
            if !still_next {
                return;
            }
            let quality = settings::get().quality;
            if let Ok(info) = res
                && let Some(f) = extractor::select(&info, quality) {
                    log::debug!("prepared the next track {id} ({})", f.describe());
                    imp.player.set_next(Some(NextStream {
                        uri: f.url.clone(),
                        headers: f.headers(),
                        token,
                    }));
                    imp.prepared.replace(Some((token, id)));
                }
        });
    }

    /// The player continued gaplessly into the prepared stream.
    fn on_advanced(&self, token: u64) {
        let imp = self.imp();
        let prepared = imp.prepared.take();
        let Some((t, id)) = prepared.filter(|(t, _)| *t == token) else {
            log::warn!("gapless switch to an unexpected stream; reloading");
            self.start_current(true, None);
            return;
        };
        let _ = t;
        let index = self
            .next_index(true)
            .filter(|i| self.track_at(*i).is_some_and(|x| x.video_id == id))
            .or_else(|| {
                let cur = self.current().max(0) as u32;
                (cur..self.imp().queue.n_items()).find(|i| self.track_at(*i).is_some_and(|x| x.video_id == id))
            });
        let Some(index) = index else {
            self.start_current(true, None);
            return;
        };
        imp.generation.set(imp.generation.get() + 1);
        imp.retries.set(0);
        imp.counted.set(false);
        if index as i32 != self.current() {
            self.set_current(index as i32);
        } else {
            self.update_metadata();
        }
        if let Some(t) = self.current_track() {
            imp.counted.set(true);
            Library::get().add_history(&t);
            self.report_play(&t.video_id);
        }
        self.save_session();
        self.prepare_next();
        self.maybe_extend();
    }

    fn reshuffle(&self, on: bool) {
        let imp = self.imp();
        let tracks = self.tracks();
        let cur = self.current();
        if tracks.is_empty() || cur < 0 {
            imp.unshuffled.take();
            return;
        }
        let cur = cur as usize;
        if on {
            imp.unshuffled.replace(Some(tracks.clone()));
            let mut rest = tracks[cur + 1..].to_vec();
            rest.shuffle(&mut rand::rng());
            let n = imp.queue.n_items();
            imp.queue.splice((cur + 1) as u32, n - (cur + 1) as u32, &Self::objects(rest));
        } else if let Some(original) = imp.unshuffled.take() {
            let id = &tracks[cur].video_id;
            if let Some(pos) = original.iter().position(|t| t.video_id == *id) {
                imp.current.set(-1);
                self.set_tracks(original);
                imp.current.set(pos as i32);
                imp.queue.items_changed(pos as u32, 1, 1);
                self.notify_current();
            }
        }
        self.prepare_next();
    }

    /// Add a play to the account's YouTube history.
    fn report_play(&self, video_id: &str) {
        if !api::client().is_signed_in() || !settings::get().send_history {
            return;
        }
        let Some(url) = extractor()
            .cached(video_id, &extractor::request())
            .and_then(|i| i.tracking.clone())
            .and_then(|t| t.playback)
        else {
            log::debug!("no tracking URL for {video_id}; not added to history");
            return;
        };
        runtime::spawn(async move {
            if let Err(e) = api::client().report_play(&url).await {
                log::warn!("adding the play to history failed: {e:#}");
            }
        });
    }

    // -----------------------------------------------------------------------
    // Lyrics and related content
    // -----------------------------------------------------------------------

    /// Where the lyrics and related content of a track are (cached).
    pub async fn track_info(&self, video_id: &str) -> anyhow::Result<TrackInfo> {
        if let Some(i) = self.imp().info.borrow().get(video_id) {
            return Ok(i.clone());
        }
        let id = video_id.to_owned();
        let q = runtime::run(async move { api::client().track_tabs(&id).await }).await?;
        let info = TrackInfo {
            lyrics: q.lyrics,
            related: q.related,
        };
        let mut cache = self.imp().info.borrow_mut();
        if cache.len() > 100 {
            cache.clear();
        }
        cache.insert(video_id.to_owned(), info.clone());
        Ok(info)
    }

    // -----------------------------------------------------------------------
    // Session
    // -----------------------------------------------------------------------

    pub fn save_session(&self) {
        let imp = self.imp();
        let cur = self.current();
        if cur < 0 || !settings::get().restore_queue {
            return;
        }
        let tracks = self.tracks();
        // Keep the file small: a window around the current track, one artwork size.
        let start = (cur as usize).saturating_sub(50);
        let end = (start + 300).min(tracks.len());
        let window: Vec<Track> = tracks[start..end]
            .iter()
            .cloned()
            .map(|mut t| {
                if t.thumbnails.len() > 1 {
                    t.thumbnails = t.thumbnails.iter().max_by_key(|x| x.width).cloned().into_iter().collect();
                }
                t
            })
            .collect();
        let pos = match imp.player.state() {
            PlayState::Idle => imp.resume_at.get().unwrap_or(0.0),
            _ => imp.player.position(),
        };
        let session = Session {
            tracks: window,
            current: cur as usize - start,
            position: pos,
            continuation: imp.continuation.borrow().clone(),
            title: imp.queue_title.borrow().clone(),
        };
        match serde_json::to_vec(&session) {
            Ok(d) => {
                if let Err(e) = config::write_atomic(&session_path(), &d) {
                    log::warn!("saving the queue failed: {e}");
                }
            }
            Err(e) => log::warn!("serializing the queue failed: {e}"),
        }
    }

    /// Bring back the last queue, paused; nothing is fetched until play is pressed.
    pub fn restore_session(&self) {
        if !settings::get().restore_queue {
            return;
        }
        let Some(s) = std::fs::read(session_path())
            .ok()
            .and_then(|d| serde_json::from_slice::<Session>(&d).ok())
        else {
            return;
        };
        if s.tracks.is_empty() || s.current >= s.tracks.len() {
            return;
        }
        let imp = self.imp();
        self.set_tracks(s.tracks);
        imp.continuation.replace(s.continuation);
        imp.resume_at.set((s.position > 1.0).then_some(s.position));
        self.set_queue_title(&s.title);
        self.set_active(true);
        self.set_current(s.current as i32);
    }
}

thread_local! {
    static PLAYBACK: Playback = glib::Object::new();
}

impl Playback {
    /// The app-wide playback controller (main thread).
    pub fn get() -> Playback {
        PLAYBACK.with(|p| p.clone())
    }
}
