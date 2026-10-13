//! Import a YouTube session from a Firefox-family browser profile.
//!
//! Firefox keeps cookies unencrypted in `cookies.sqlite`; recent writes may
//! still be in the write-ahead log next to it, so both are copied.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gtk::glib;

use super::Cookie;

#[derive(Debug, Clone)]
pub struct FirefoxProfile {
    pub browser: &'static str,
    pub profile: String,
    pub cookies_db: PathBuf,
}

impl FirefoxProfile {
    pub fn label(&self) -> String {
        format!("{} — {}", self.browser, self.profile)
    }
}

/// (display name, profile root relative to $HOME)
const ROOTS: &[(&str, &str)] = &[
    ("Firefox", ".mozilla/firefox"),
    ("Firefox", ".config/mozilla/firefox"),
    ("Firefox", ".var/app/org.mozilla.firefox/.mozilla/firefox"),
    ("Firefox", ".var/app/org.mozilla.firefox/config/mozilla/firefox"),
    ("LibreWolf", ".librewolf"),
    ("LibreWolf", ".var/app/io.gitlab.librewolf-community/.librewolf"),
    ("Zen", ".zen"),
    ("Floorp", ".floorp"),
    ("Waterfox", ".waterfox"),
];

pub fn find_profiles() -> Vec<FirefoxProfile> {
    let home = glib::home_dir();
    let mut out: Vec<FirefoxProfile> = Vec::new();
    for (browser, root) in ROOTS {
        let Ok(entries) = std::fs::read_dir(home.join(root)) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        dirs.sort();
        for dir in dirs {
            let db = dir.join("cookies.sqlite");
            if !db.is_file() || out.iter().any(|p| p.cookies_db == db) {
                continue;
            }
            // Profile directories are "<random>.<name>"; show the name.
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let profile = name.split_once('.').map(|(_, n)| n.to_owned()).unwrap_or(name);
            out.push(FirefoxProfile {
                browser,
                profile,
                cookies_db: db,
            });
        }
    }
    out
}

/// Read YouTube/Google cookies (must run on the tokio runtime).
pub async fn import(profile: &FirefoxProfile) -> Result<Vec<Cookie>> {
    let db = profile.cookies_db.clone();
    tokio::task::spawn_blocking(move || read_cookies(&db)).await?
}

fn read_cookies(db: &Path) -> Result<Vec<Cookie>> {
    // Work on a copy: Firefox keeps the live database locked.
    let tmp = crate::paths::runtime_dir("sloptube").join(format!("import-ff-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let copy = tmp.join("cookies.sqlite");
    let result = (|| -> Result<Vec<Cookie>> {
        std::fs::copy(db, &copy).context("cannot copy the browser cookie database")?;
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        if wal.exists() {
            let _ = std::fs::copy(&wal, tmp.join("cookies.sqlite-wal"));
        }
        let conn = rusqlite::Connection::open(&copy)?;
        let mut stmt = conn.prepare(
            "SELECT host, name, value, path, expiry, isSecure, isHttpOnly, originAttributes \
             FROM moz_cookies WHERE host LIKE '%youtube.com' OR host LIKE '%google.com'",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, String>(7).unwrap_or_default(),
            ))
        })?;
        let mut cookies = Vec::new();
        for row in rows {
            let (host, name, value, path, expiry, secure, http_only, origin) = row?;
            // Only the default container: other containers may hold other accounts.
            if !origin.is_empty() && !origin.starts_with("^firstPartyDomain=") {
                continue;
            }
            // Newer Firefox versions store milliseconds.
            let expires = if expiry > 100_000_000_000 { expiry / 1000 } else { expiry };
            cookies.push(Cookie {
                domain: host,
                name,
                value,
                path,
                expires: (expires > 0).then_some(expires),
                secure,
                http_only,
            });
        }
        if cookies.is_empty() {
            bail!("this profile has no YouTube cookies; sign in to YouTube in that browser first");
        }
        Ok(cookies)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}
