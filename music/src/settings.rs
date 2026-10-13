//! User preferences, persisted as JSON in the config directory.

use common::prefs::{self, Store};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AudioQuality {
    /// ~50 kbps Opus.
    Low,
    /// ~70 kbps Opus.
    Normal,
    /// ~130–160 kbps Opus or AAC.
    #[default]
    High,
}

impl AudioQuality {
    pub const ALL: [AudioQuality; 3] = [AudioQuality::Low, AudioQuality::Normal, AudioQuality::High];

    pub fn label(self) -> &'static str {
        match self {
            AudioQuality::Low => "Low",
            AudioQuality::Normal => "Normal",
            AudioQuality::High => "High",
        }
    }

    /// Highest audio bitrate (kbps) to pick at this quality.
    pub fn max_kbps(self) -> f64 {
        match self {
            AudioQuality::Low => 56.0,
            AudioQuality::Normal => 80.0,
            AudioQuality::High => 400.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub quality: AudioQuality,
    pub volume: f64,
    pub muted: bool,
    pub repeat: Repeat,
    /// Keep playing a radio when the queue runs out.
    pub autoplay: bool,
    pub background_playback: bool,
    /// Restore the last queue (paused) at startup.
    pub restore_queue: bool,
    /// Two-letter region code for charts and recommendations (None = from the IP).
    pub region: Option<String>,
    pub search_history: Vec<String>,
    pub save_search_history: bool,
    /// Let yt-dlp use the account (Premium audio quality, age-restricted songs).
    pub use_account_for_streams: bool,
    /// Add songs played here to the account's YouTube history.
    pub send_history: bool,
    /// A session was stored; without one, startup doesn't touch the keyring.
    pub has_session: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            quality: AudioQuality::High,
            volume: 1.0,
            muted: false,
            repeat: Repeat::Off,
            autoplay: true,
            background_playback: true,
            restore_queue: true,
            region: None,
            search_history: Vec::new(),
            save_search_history: true,
            use_account_for_streams: true,
            send_history: true,
            has_session: false,
        }
    }
}

thread_local! {
    static SETTINGS: Store<Settings> = Store::load(config::config_dir().join("settings.json"));
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
