//! User preferences, persisted as JSON in the config directory.

use std::path::PathBuf;

use common::prefs::{self, Store};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CodecPref {
    #[default]
    Auto,
    Av1,
    Vp9,
    H264,
}

impl CodecPref {
    pub const ALL: [CodecPref; 4] = [CodecPref::Auto, CodecPref::Av1, CodecPref::Vp9, CodecPref::H264];

    pub fn label(self) -> &'static str {
        match self {
            CodecPref::Auto => "Automatic (hardware first)",
            CodecPref::Av1 => "AV1",
            CodecPref::Vp9 => "VP9",
            CodecPref::H264 => "H.264",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DownloadQuality {
    #[default]
    Best,
    Max1080,
    Max720,
    Max480,
    AudioOnly,
}

impl DownloadQuality {
    pub const ALL: [DownloadQuality; 5] = [
        DownloadQuality::Best,
        DownloadQuality::Max1080,
        DownloadQuality::Max720,
        DownloadQuality::Max480,
        DownloadQuality::AudioOnly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DownloadQuality::Best => "Best available",
            DownloadQuality::Max1080 => "Up to 1080p",
            DownloadQuality::Max720 => "Up to 720p",
            DownloadQuality::Max480 => "Up to 480p",
            DownloadQuality::AudioOnly => "Audio only",
        }
    }

    pub fn max_height(self) -> Option<u32> {
        match self {
            DownloadQuality::Best => None,
            DownloadQuality::Max1080 => Some(1080),
            DownloadQuality::Max720 => Some(720),
            DownloadQuality::Max480 => Some(480),
            DownloadQuality::AudioOnly => Some(0),
        }
    }
}

pub const HEIGHTS: [u32; 8] = [2160, 1440, 1080, 720, 480, 360, 240, 144];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub codec: CodecPref,
    pub max_height: u32,
    pub allow_60fps: bool,
    pub hw_decode: bool,
    pub autoplay: bool,
    pub speed: f64,
    pub volume: f64,
    pub muted: bool,
    pub background_playback: bool,
    pub color_scheme: ColorScheme,
    /// Two-letter region code for content (None = let YouTube guess from the IP).
    pub region: Option<String>,
    pub use_account_for_streams: bool,
    pub send_watch_history: bool,
    pub download_dir: Option<PathBuf>,
    pub download_quality: DownloadQuality,
    pub search_history: Vec<String>,
    pub save_search_history: bool,
    pub resume_playback: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            codec: CodecPref::Auto,
            max_height: 1080,
            allow_60fps: true,
            hw_decode: true,
            autoplay: true,
            speed: 1.0,
            volume: 1.0,
            muted: false,
            background_playback: true,
            color_scheme: ColorScheme::System,
            region: None,
            use_account_for_streams: true,
            send_watch_history: true,
            download_dir: None,
            download_quality: DownloadQuality::Max1080,
            search_history: Vec::new(),
            save_search_history: true,
            resume_playback: true,
        }
    }
}

impl Settings {
    pub fn download_dir(&self) -> PathBuf {
        self.download_dir
            .clone()
            .unwrap_or_else(config::default_download_dir)
    }
}

thread_local! {
    static SETTINGS: Store<Settings> = Store::load(config::config_dir().join("settings.json"));
}

/// Keep changes in memory only (used by `--diagnose`).
pub fn set_read_only(ro: bool) {
    SETTINGS.with(|s| s.set_read_only(ro));
}

/// A snapshot of the current settings (main thread only).
pub fn get() -> Settings {
    SETTINGS.with(Store::get)
}

/// Modify and persist settings (main thread only).
pub fn update(f: impl FnOnce(&mut Settings)) {
    SETTINGS.with(|s| s.update(f));
}

pub fn add_search_history(query: &str) {
    if get().save_search_history {
        update(|s| prefs::remember_search(&mut s.search_history, query));
    }
}
