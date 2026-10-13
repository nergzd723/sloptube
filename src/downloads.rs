//! Offline downloads via the yt-dlp CLI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};

use crate::api::{self, VideoItem};
use crate::auth;
use crate::config;
use crate::extractor;
use crate::player::decoders::{self, Codec};
use crate::player::formats;
use crate::runtime;
use crate::settings::{self, DownloadQuality};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum Status {
    #[default]
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub duration: Option<String>,
    pub thumbnail: Option<PathBuf>,
    pub path: Option<PathBuf>,
    pub audio_only: bool,
    pub status: Status,
    #[serde(skip)]
    pub progress: f64,
    #[serde(skip)]
    pub speed: Option<String>,
    #[serde(skip)]
    pub eta: Option<String>,
}

impl Entry {
    pub fn video_item(&self) -> VideoItem {
        VideoItem {
            id: self.id.clone(),
            title: self.title.clone(),
            channel_name: self.channel.clone(),
            channel_id: self.channel_id.clone(),
            duration: self.duration.clone(),
            ..Default::default()
        }
    }
}

fn index_path() -> PathBuf {
    config::data_dir().join("downloads.json")
}

fn thumbs_dir() -> PathBuf {
    let d = config::data_dir().join("thumbnails");
    let _ = std::fs::create_dir_all(&d);
    d
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Downloads {
        pub entries: RefCell<Vec<Entry>>,
        pub procs: RefCell<HashMap<String, gio::Subprocess>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Downloads {
        const NAME: &'static str = "SlopDownloads";
        type Type = super::Downloads;
    }

    impl ObjectImpl for Downloads {
        fn signals() -> &'static [Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<Signal>> = std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // id
                    Signal::builder("changed").param_types([String::static_type()]).build(),
                    // list membership changed
                    Signal::builder("list-changed").build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let mut entries: Vec<Entry> = std::fs::read(index_path())
                .ok()
                .and_then(|d| serde_json::from_slice(&d).ok())
                .unwrap_or_default();
            // Anything that was running when we quit didn't finish.
            for e in &mut entries {
                if matches!(e.status, Status::Running | Status::Queued) {
                    e.status = Status::Failed("Interrupted".into());
                }
            }
            entries.retain(|e| e.status != Status::Done || e.path.as_ref().is_some_and(|p| p.exists()));
            self.entries.replace(entries);
        }

        fn dispose(&self) {
            for (_, p) in self.procs.take() {
                p.force_exit();
            }
        }
    }
}

glib::wrapper! {
    pub struct Downloads(ObjectSubclass<imp::Downloads>);
}

thread_local! {
    static DOWNLOADS: Downloads = glib::Object::new();
}

impl Downloads {
    pub fn get() -> Downloads {
        DOWNLOADS.with(|d| d.clone())
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.imp().entries.borrow().clone()
    }

    pub fn entry(&self, id: &str) -> Option<Entry> {
        self.imp().entries.borrow().iter().find(|e| e.id == id).cloned()
    }

    pub fn active_count(&self) -> usize {
        self.imp()
            .entries
            .borrow()
            .iter()
            .filter(|e| matches!(e.status, Status::Running | Status::Queued))
            .count()
    }

    fn save(&self) {
        let entries = self.imp().entries.borrow().clone();
        if let Ok(data) = serde_json::to_vec_pretty(&entries) {
            let _ = std::fs::write(index_path(), data);
        }
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Entry)) {
        if let Some(e) = self.imp().entries.borrow_mut().iter_mut().find(|e| e.id == id) {
            f(e);
        }
        self.emit_by_name::<()>("changed", &[&id.to_owned()]);
    }

    pub fn connect_changed<F: Fn(&Self, &str) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("changed", false, move |args| {
            let obj = args[0].get::<Self>().unwrap();
            let id = args[1].get::<String>().unwrap_or_default();
            f(&obj, &id);
            None
        })
    }

    pub fn connect_list_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_local("list-changed", false, move |args| {
            f(&args[0].get::<Self>().unwrap());
            None
        })
    }

    /// yt-dlp format spec for the device's decoders.
    fn format_spec(quality: DownloadQuality) -> String {
        if quality == DownloadQuality::AudioOnly {
            return "ba[acodec^=opus]/ba/b".into();
        }
        let s = settings::get();
        let support = decoders::probe();
        let order = formats::codec_order(s.codec, &support, s.hw_decode);
        let h = quality
            .max_height()
            .map(|h| format!("[height<={h}]"))
            .unwrap_or_default();
        let mut parts: Vec<String> = order
            .iter()
            .filter_map(|c| match c {
                Codec::Av1 => Some("[vcodec^=av01]"),
                Codec::Vp9 => Some("[vcodec~='^vp0?9']"),
                Codec::H264 => Some("[vcodec^=avc1]"),
                Codec::Other => None,
            })
            .map(|c| format!("bv*{h}{c}+ba"))
            .collect();
        parts.push(format!("bv*{h}+ba"));
        parts.push(format!("b{h}"));
        parts.push("b".into());
        parts.join("/")
    }

    pub fn start(&self, item: &VideoItem) {
        let imp = self.imp();
        if imp.procs.borrow().contains_key(&item.id) {
            return;
        }
        let quality = settings::get().download_quality;
        let dir = settings::get().download_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            crate::ui::main_window().map(|w| w.toast(&format!("Cannot create {}: {e}", dir.display())));
            return;
        }
        let Some(ytdlp) = extractor::ytdlp_path() else {
            crate::ui::main_window().map(|w| w.toast("yt-dlp is not installed"));
            return;
        };

        let entry = Entry {
            id: item.id.clone(),
            title: item.title.clone(),
            channel: item.channel_name.clone(),
            channel_id: item.channel_id.clone(),
            duration: item.duration.clone(),
            thumbnail: None,
            path: None,
            audio_only: quality == DownloadQuality::AudioOnly,
            status: Status::Queued,
            ..Default::default()
        };
        {
            let mut entries = imp.entries.borrow_mut();
            entries.retain(|e| e.id != item.id);
            entries.insert(0, entry);
        }
        self.emit_by_name::<()>("list-changed", &[]);

        // Thumbnail for offline display.
        let thumb_url = item.thumbnail_url(480);
        let thumb_path = thumbs_dir().join(format!("{}.jpg", item.id));
        let this = self.downgrade();
        let id = item.id.clone();
        glib::spawn_future_local(async move {
            let path = thumb_path.clone();
            let ok = runtime::run(async move {
                let bytes = api::client::http().get(&thumb_url).send().await.ok()?.bytes().await.ok()?;
                std::fs::write(&path, &bytes).ok()
            })
            .await
            .is_some();
            if let (true, Some(this)) = (ok, this.upgrade()) {
                this.update(&id, |e| e.thumbnail = Some(thumb_path));
            }
        });

        let mut args: Vec<String> = vec![
            ytdlp.to_string_lossy().into_owned(),
            "--newline".into(),
            "--no-warnings".into(),
            "--no-playlist".into(),
            "--no-simulate".into(),
            "--progress".into(),
            "--progress-template".into(),
            "download:SLOP %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s %(progress.eta)s %(info.format_id)s".into(),
            "--print".into(),
            "after_move:SLOPFILE %(filepath)s".into(),
            "-f".into(),
            Self::format_spec(quality),
            "-o".into(),
            dir.join("%(title).150B [%(id)s].%(ext)s").to_string_lossy().into_owned(),
        ];
        if quality != DownloadQuality::AudioOnly {
            args.push("--merge-output-format".into());
            args.push("mkv".into());
        }
        if settings::get().use_account_for_streams {
            if let Some(s) = common::session::get().filter(|s| s.is_signed_in()) {
                if let Ok(p) = auth::write_cookie_file(&s, "video") {
                    args.push("--cookies".into());
                    args.push(p.to_string_lossy().into_owned());
                }
            }
        }
        args.push(format!("https://www.youtube.com/watch?v={}", item.id));

        let launcher = gio::SubprocessLauncher::new(
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
        );
        let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
        let proc = match launcher.spawn(&argv) {
            Ok(p) => p,
            Err(e) => {
                self.update(&item.id, |en| en.status = Status::Failed(e.to_string()));
                self.save();
                return;
            }
        };
        imp.procs.borrow_mut().insert(item.id.clone(), proc.clone());
        self.update(&item.id, |e| e.status = Status::Running);
        let two_phase = quality != DownloadQuality::AudioOnly;

        let this = self.downgrade();
        let id = item.id.clone();
        glib::spawn_future_local(async move {
            let stdout = gio::DataInputStream::new(&proc.stdout_pipe().unwrap());
            let stderr = gio::DataInputStream::new(&proc.stderr_pipe().unwrap());
            let last_error: std::rc::Rc<RefCell<String>> = Default::default();
            let err_sink = last_error.clone();
            glib::spawn_future_local(async move {
                while let Ok(Some(line)) = stderr.read_line_utf8_future(glib::Priority::DEFAULT).await {
                    let line = line.to_string();
                    if line.starts_with("ERROR:") {
                        err_sink.replace(line.trim_start_matches("ERROR: ").to_owned());
                    }
                }
            });
            let mut formats_seen: Vec<String> = Vec::new();
            let mut final_path: Option<PathBuf> = None;
            while let Ok(Some(line)) = stdout.read_line_utf8_future(glib::Priority::DEFAULT).await {
                let line = line.to_string();
                if let Some(rest) = line.strip_prefix("SLOPFILE ") {
                    final_path = Some(PathBuf::from(rest.trim()));
                    continue;
                }
                let Some(rest) = line.strip_prefix("SLOP ") else { continue };
                let f: Vec<&str> = rest.split_whitespace().collect();
                if f.len() < 6 {
                    continue;
                }
                let num = |s: &str| s.parse::<f64>().ok();
                let done = num(f[0]).unwrap_or(0.0);
                let total = num(f[1]).or_else(|| num(f[2])).unwrap_or(0.0);
                let speed = num(f[3]);
                let eta = num(f[4]);
                let fmt = f[5].to_owned();
                if !formats_seen.contains(&fmt) {
                    formats_seen.push(fmt);
                }
                let phase = formats_seen.len().saturating_sub(1) as f64;
                let phases = if two_phase { 2.0 } else { 1.0 };
                let frac = if total > 0.0 { (done / total).clamp(0.0, 1.0) } else { 0.0 };
                // Video is by far the bigger part of a merged download.
                let progress = if two_phase {
                    if phase < 1.0 { frac * 0.9 } else { 0.9 + frac * 0.1 }
                } else {
                    ((phase + frac) / phases).min(1.0)
                };
                if let Some(this) = this.upgrade() {
                    this.update(&id, |e| {
                        e.progress = progress;
                        e.speed = speed.map(|s| format!("{}/s", glib::format_size(s as u64)));
                        e.eta = eta.map(|s| crate::ui::format_time(s));
                    });
                }
            }
            let ok = proc.wait_check_future().await.is_ok();
            let Some(this) = this.upgrade() else { return };
            let cancelled = this.imp().procs.borrow_mut().remove(&id).is_none();
            this.update(&id, |e| {
                if cancelled {
                    e.status = Status::Cancelled;
                } else if ok {
                    e.status = Status::Done;
                    e.progress = 1.0;
                    e.path = final_path;
                } else {
                    let msg = last_error.borrow().clone();
                    e.status = Status::Failed(if msg.is_empty() { "Download failed".into() } else { msg });
                }
            });
            this.save();
            if ok && !cancelled {
                if let Some(e) = this.entry(&id) {
                    crate::ui::main_window().map(|w| w.toast(&format!("Downloaded “{}”", e.title)));
                }
            }
        });
    }

    pub fn cancel(&self, id: &str) {
        if let Some(p) = self.imp().procs.borrow_mut().remove(id) {
            p.force_exit();
        }
    }

    pub fn remove(&self, id: &str, delete_file: bool) {
        self.cancel(id);
        let removed = {
            let mut entries = self.imp().entries.borrow_mut();
            let pos = entries.iter().position(|e| e.id == id);
            pos.map(|p| entries.remove(p))
        };
        if let Some(e) = removed {
            if delete_file {
                if let Some(p) = &e.path {
                    let _ = std::fs::remove_file(p);
                }
                if let Some(t) = &e.thumbnail {
                    let _ = std::fs::remove_file(t);
                }
            }
        }
        self.save();
        self.emit_by_name::<()>("list-changed", &[]);
    }
}
