//! Application identity and well-known paths.

use std::path::PathBuf;

use gtk::glib;

pub const APP_ID: &str = "io.github.nergzd.SlopTubeMusic";
pub const APP_NAME: &str = "SlopTube Music";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RESOURCE_BASE: &str = "/io/github/nergzd/SlopTubeMusic";

/// Set when the environment asked for a dark Adwaita via `GTK_THEME=Adwaita:dark`.
pub static GTK_THEME_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn ensure(dir: PathBuf) -> PathBuf {
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_dir() -> PathBuf {
    ensure(glib::user_config_dir().join("sloptube-music"))
}

pub fn data_dir() -> PathBuf {
    ensure(glib::user_data_dir().join("sloptube-music"))
}

/// Per-session private directory (tmpfs, 0700) for short-lived secrets like the yt-dlp cookie jar.
pub fn runtime_dir() -> PathBuf {
    let dir = ensure(glib::user_runtime_dir().join("sloptube-music"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Write a file atomically (temporary file + rename).
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}
