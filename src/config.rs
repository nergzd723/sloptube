//! Application identity and well-known paths.

use std::path::PathBuf;

use common::paths;
use gtk::glib;

pub const APP_ID: &str = "io.github.nergzd.SlopTube";
pub const APP_NAME: &str = "SlopTube";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set when the environment asked for a dark Adwaita via `GTK_THEME=Adwaita:dark`.
pub static GTK_THEME_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn config_dir() -> PathBuf {
    paths::config_dir("sloptube")
}

pub fn data_dir() -> PathBuf {
    paths::data_dir("sloptube")
}

/// Per-session private directory (tmpfs, 0700) for short-lived secrets like the yt-dlp cookie jar.
pub fn runtime_dir() -> PathBuf {
    paths::runtime_dir("sloptube")
}

pub fn default_download_dir() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Videos)
        .unwrap_or_else(|| glib::home_dir().join("Videos"))
        .join("SlopTube")
}
