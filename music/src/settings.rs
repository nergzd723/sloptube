//! User preferences, persisted as JSON in the config directory.

use std::cell::RefCell;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ColorScheme {
    System,
    Light,
    #[default]
    Dark,
}

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
    pub color_scheme: ColorScheme,
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
            color_scheme: ColorScheme::Dark,
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

fn path() -> PathBuf {
    config::config_dir().join("settings.json")
}

fn load() -> Settings {
    std::fs::read(path())
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(load());
}

/// A snapshot of the current settings (main thread only).
pub fn get() -> Settings {
    SETTINGS.with(|s| s.borrow().clone())
}

/// Modify and persist settings (main thread only).
pub fn update(f: impl FnOnce(&mut Settings)) {
    SETTINGS.with(|s| {
        let mut s = s.borrow_mut();
        f(&mut s);
        match serde_json::to_vec_pretty(&*s) {
            Ok(data) => {
                if let Err(e) = config::write_atomic(&path(), &data) {
                    log::warn!("failed to save settings: {e}");
                }
            }
            Err(e) => log::warn!("failed to serialize settings: {e}"),
        }
    });
}

pub fn add_search_history(query: &str) {
    let q = query.trim();
    if q.is_empty() || !get().save_search_history {
        return;
    }
    update(|s| {
        s.search_history.retain(|h| !h.eq_ignore_ascii_case(q));
        s.search_history.insert(0, q.to_owned());
        s.search_history.truncate(30);
    });
}
