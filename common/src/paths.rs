//! Per-app directories (`~/.config/<app>`, ...) and atomic writes.

use std::path::{Path, PathBuf};

use gtk::glib;

fn ensure(dir: PathBuf) -> PathBuf {
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_dir(app: &str) -> PathBuf {
    ensure(glib::user_config_dir().join(app))
}

pub fn data_dir(app: &str) -> PathBuf {
    ensure(glib::user_data_dir().join(app))
}

/// Per-session private directory (tmpfs, 0700) for short-lived secrets like the yt-dlp cookie jar.
pub fn runtime_dir(app: &str) -> PathBuf {
    let dir = ensure(glib::user_runtime_dir().join(app));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// Write a file atomically (temporary file + rename).
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Write a file only the user can read, atomically.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(data)?;
    drop(f);
    std::fs::rename(&tmp, path)
}
