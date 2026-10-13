//! The Google session both apps use: cookie jar, SAPISIDHASH authorization,
//! secure storage, and importing it from a browser.

pub mod chromium;
pub mod firefox;

use std::collections::HashSet;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::innertube::model::AccountInfo;
use crate::paths;

/// The session's own cookies (`SAPISID`, `SID`, ...) live on `.youtube.com`, so
/// any YouTube host finds them; this one is used to look them up.
const YT_HOST: &str = "www.youtube.com";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cookie {
    pub domain: String,
    pub name: String,
    pub value: String,
    pub path: String,
    /// Unix timestamp; `None` for session cookies.
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
}

impl Cookie {
    fn matches_host(&self, host: &str) -> bool {
        let d = self.domain.trim_start_matches('.');
        host == d || host.ends_with(&format!(".{d}"))
    }

    fn expired(&self, now: i64) -> bool {
        self.expires.is_some_and(|e| e > 0 && e < now)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub cookies: Vec<Cookie>,
    #[serde(default)]
    pub account: Option<AccountInfo>,
    /// How the session was obtained ("webkit", "chromium", ...).
    #[serde(default)]
    pub source: String,
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sid_hash(ts: i64, sid: &str, origin: &str) -> String {
    let mut h = Sha1::new();
    h.update(format!("{ts} {sid} {origin}").as_bytes());
    format!("{ts}_{}", hex::encode(h.finalize()))
}

impl Session {
    pub fn new(cookies: Vec<Cookie>, source: &str) -> Self {
        let cookies = cookies
            .into_iter()
            .filter(|c| {
                let d = c.domain.trim_start_matches('.');
                d == "youtube.com" || d.ends_with(".youtube.com") || d == "google.com" || d.ends_with(".google.com")
            })
            .collect();
        Self {
            cookies,
            account: None,
            source: source.to_owned(),
        }
    }

    fn valid_for<'a>(&'a self, host: &'a str) -> impl Iterator<Item = &'a Cookie> + 'a {
        let now = now();
        self.cookies
            .iter()
            .filter(move |c| c.matches_host(host) && !c.expired(now))
    }

    pub fn get<'a>(&'a self, host: &str, name: &str) -> Option<&'a str> {
        let now = now();
        self.cookies
            .iter()
            .filter(|c| c.name == name && c.matches_host(host) && !c.expired(now))
            .max_by_key(|c| c.domain.trim_start_matches('.').len())
            .map(|c| c.value.as_str())
    }

    pub fn cookie_header(&self, host: &str) -> String {
        let mut cs: Vec<&Cookie> = self.valid_for(host).collect();
        // Most specific domain wins for duplicate names.
        cs.sort_by_key(|c| std::cmp::Reverse(c.domain.trim_start_matches('.').len()));
        let mut seen = HashSet::new();
        cs.into_iter()
            .filter(|c| seen.insert(c.name.as_str()))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn sapisid(&self) -> Option<&str> {
        self.get(YT_HOST, "SAPISID")
            .or_else(|| self.get(YT_HOST, "__Secure-3PAPISID"))
    }

    pub fn is_signed_in(&self) -> bool {
        self.sapisid().is_some()
            && (self.get(YT_HOST, "SID").is_some()
                || self.get(YT_HOST, "__Secure-3PSID").is_some()
                || self.get(YT_HOST, "__Secure-1PSID").is_some())
    }

    /// Value for the `Authorization` header of InnerTube requests sent from
    /// `origin` (`https://www.youtube.com`, `https://music.youtube.com`).
    pub fn authorization(&self, origin: &str) -> Option<String> {
        let ts = now();
        let sapisid = self.sapisid()?;
        let mut parts = vec![format!("SAPISIDHASH {}", sid_hash(ts, sapisid, origin))];
        if let Some(p1) = self.get(YT_HOST, "__Secure-1PAPISID") {
            parts.push(format!("SAPISID1PHASH {}", sid_hash(ts, p1, origin)));
        }
        if let Some(p3) = self.get(YT_HOST, "__Secure-3PAPISID") {
            parts.push(format!("SAPISID3PHASH {}", sid_hash(ts, p3, origin)));
        }
        Some(parts.join(" "))
    }

    /// Apply a `Set-Cookie` header received from `host`. Returns true if the jar changed.
    pub fn apply_set_cookie(&mut self, header: &str, host: &str) -> bool {
        let Some(cookie) = parse_set_cookie(header, host) else {
            return false;
        };
        let pos = self.cookies.iter().position(|c| {
            c.name == cookie.name
                && c.path == cookie.path
                && c.domain.trim_start_matches('.') == cookie.domain.trim_start_matches('.')
        });
        let delete = cookie.expired(now());
        match (pos, delete) {
            (Some(i), true) => {
                self.cookies.remove(i);
                true
            }
            (Some(i), false) => {
                if self.cookies[i] == cookie {
                    false
                } else {
                    self.cookies[i] = cookie;
                    true
                }
            }
            (None, true) => false,
            (None, false) => {
                self.cookies.push(cookie);
                true
            }
        }
    }

    /// Netscape cookie file for yt-dlp.
    pub fn to_netscape(&self) -> String {
        let mut out = String::from("# Netscape HTTP Cookie File\n");
        for c in &self.cookies {
            let domain = if c.domain.starts_with('.') {
                c.domain.clone()
            } else {
                format!(".{}", c.domain)
            };
            out.push_str(&format!(
                "{}\tTRUE\t{}\t{}\t{}\t{}\t{}\n",
                domain,
                if c.path.is_empty() { "/" } else { &c.path },
                if c.secure { "TRUE" } else { "FALSE" },
                c.expires.unwrap_or(0).max(0),
                c.name,
                c.value
            ));
        }
        out
    }
}

fn parse_set_cookie(header: &str, host: &str) -> Option<Cookie> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.trim().split_once('=')?;
    let mut c = Cookie {
        domain: host.to_owned(),
        name: name.trim().to_owned(),
        value: value.trim().to_owned(),
        path: "/".into(),
        expires: None,
        secure: false,
        http_only: false,
    };
    let mut max_age = None;
    for attr in parts {
        let attr = attr.trim();
        let (k, v) = attr.split_once('=').unwrap_or((attr, ""));
        match k.to_ascii_lowercase().as_str() {
            "domain" => c.domain = v.trim().to_owned(),
            "path" => c.path = v.trim().to_owned(),
            "expires" => c.expires = parse_cookie_date(v),
            "max-age" => max_age = v.trim().parse::<i64>().ok(),
            "secure" => c.secure = true,
            "httponly" => c.http_only = true,
            _ => {}
        }
    }
    if let Some(ma) = max_age {
        c.expires = Some(if ma <= 0 { 1 } else { now() + ma });
    }
    Some(c)
}

/// Lenient parser for cookie dates like "Fri, 08-Oct-2027 19:31:48 GMT".
fn parse_cookie_date(s: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let mut day = None;
    let mut month = None;
    let mut year = None;
    let mut hms = None;
    for tok in s.split([' ', '-', ',']) {
        if tok.is_empty() {
            continue;
        }
        if tok.contains(':') {
            let v: Vec<i64> = tok.split(':').filter_map(|x| x.parse().ok()).collect();
            if v.len() == 3 {
                hms = Some((v[0], v[1], v[2]));
            }
        } else if let Ok(n) = tok.parse::<i64>() {
            if day.is_none() && n <= 31 && tok.len() <= 2 {
                day = Some(n);
            } else {
                year = Some(if n < 100 { 2000 + n } else { n });
            }
        } else if let Some(m) = MONTHS
            .iter()
            .position(|m| tok.to_ascii_lowercase().starts_with(m))
        {
            month = Some(m as i64 + 1);
        }
    }
    let (d, m, y) = (day?, month?, year?);
    let (hh, mm, ss) = hms.unwrap_or((0, 0, 0));
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

// ---------------------------------------------------------------------------
// Storage: Secret Service (gnome-keyring, KWallet, ...) with a private file fallback
// ---------------------------------------------------------------------------

/// The one stored session. Music kept its own until both apps shared one; its
/// item is read once to carry a sign-in over (see `load_with_legacy`).
const ATTRS: [(&str, &str); 2] = [("application", "io.github.nergzd.SlopTube"), ("type", "youtube-session")];
const LEGACY_ATTRS: [(&str, &str); 2] = [("application", "io.github.nergzd.SlopTubeMusic"), ("type", "youtube-session")];

fn fallback_path() -> std::path::PathBuf {
    paths::data_dir("sloptube").join("session.json")
}

// Not `paths::data_dir`, which would create Music's old directory.
fn legacy_fallback_path() -> std::path::PathBuf {
    gtk::glib::user_data_dir().join("sloptube-music").join("session.json")
}

/// Remembers whether a session is stored, so that startup without one doesn't
/// wake the keyring (it may ask to be unlocked). Unknown until the first load.
fn marker_path() -> std::path::PathBuf {
    paths::data_dir("sloptube").join("has-session")
}

/// Whether a session may be stored: unknown (never checked) counts as yes.
pub fn may_have_session() -> bool {
    match std::fs::read_to_string(marker_path()) {
        Ok(s) => s.trim() != "no" || fallback_path().exists() || legacy_fallback_path().exists(),
        Err(_) => true,
    }
}

pub fn set_has_session(has: bool) {
    if let Err(e) = paths::write_atomic(&marker_path(), if has { b"yes" } else { b"no" }) {
        log::warn!("cannot write the session marker: {e}");
    }
}

async fn keyring() -> Result<oo7::Keyring> {
    let k = oo7::Keyring::new().await.context("no Secret Service")?;
    k.unlock().await.context("keyring is locked")?;
    Ok(k)
}

async fn read_item(k: &oo7::Keyring, attrs: &[(&str, &str); 2]) -> Option<Session> {
    match k.search_items(attrs).await {
        Ok(items) => match items.first()?.secret().await {
            Ok(secret) => match serde_json::from_slice(secret.as_bytes()) {
                Ok(s) => Some(s),
                Err(e) => {
                    log::warn!("stored session is corrupt: {e}");
                    None
                }
            },
            Err(e) => {
                log::warn!("cannot read session secret: {e}");
                None
            }
        },
        Err(e) => {
            log::warn!("keyring search failed: {e}");
            None
        }
    }
}

fn read_file(path: std::path::PathBuf) -> Option<Session> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Load the stored session, and the one SlopTube Music stored before the apps
/// shared one (must run on the tokio runtime). One keyring connection serves
/// both: without a Secret Service, connecting takes a while to time out.
pub async fn load_with_legacy() -> (Option<Session>, Option<Session>) {
    let (current, legacy) = match keyring().await {
        Ok(k) => (read_item(&k, &ATTRS).await, read_item(&k, &LEGACY_ATTRS).await),
        Err(e) => {
            log::info!("keyring unavailable ({e:#}), using file fallback");
            (None, None)
        }
    };
    (
        current.or_else(|| read_file(fallback_path())),
        legacy.or_else(|| read_file(legacy_fallback_path())),
    )
}

/// Load the stored session (must run on the tokio runtime).
pub async fn load() -> Option<Session> {
    match keyring().await {
        Ok(k) => read_item(&k, &ATTRS).await,
        Err(e) => {
            log::info!("keyring unavailable ({e:#}), using file fallback");
            None
        }
    }
    .or_else(|| read_file(fallback_path()))
}

/// Forget Music's old session (once the shared one is settled).
pub async fn clear_legacy() {
    // Files first: the keyring may take a while to answer, or never.
    let _ = std::fs::remove_file(legacy_fallback_path());
    if let Ok(k) = keyring().await
        && let Err(e) = k.delete(&LEGACY_ATTRS).await
    {
        log::warn!("failed to delete Music's old session from the keyring: {e}");
    }
}

pub async fn store(session: &Session) -> Result<()> {
    let data = serde_json::to_vec(session)?;
    match keyring().await {
        Ok(k) => {
            k.create_item("SlopTube YouTube session", &ATTRS, data, true).await?;
            let _ = std::fs::remove_file(fallback_path());
            Ok(())
        }
        Err(e) => {
            log::warn!("storing session in a private file because the keyring is unavailable: {e:#}");
            paths::write_private(&fallback_path(), &data)?;
            Ok(())
        }
    }
}

/// Forget the session, including the cookie files handed to yt-dlp.
pub async fn clear() {
    // Files first: the keyring may take a while to answer, or never.
    let _ = std::fs::remove_file(fallback_path());
    for name in COOKIE_FILES {
        let _ = std::fs::remove_file(cookie_file_path(name));
    }
    // Where each app kept its cookie file before they shared the session.
    let _ = std::fs::remove_file(paths::runtime_dir("sloptube").join("cookies.txt"));
    let _ = std::fs::remove_file(gtk::glib::user_runtime_dir().join("sloptube-music").join("cookies.txt"));
    if let Ok(k) = keyring().await
        && let Err(e) = k.delete(&ATTRS).await
    {
        log::warn!("failed to delete session from keyring: {e}");
    }
}

/// The cookie files handed to yt-dlp, one per app, so that one never reads the
/// other's while it's being rewritten.
const COOKIE_FILES: [&str; 2] = ["video", "music"];

/// Cookie jar handed to yt-dlp (`name`: "video" or "music"), in the per-session
/// runtime directory.
pub fn cookie_file_path(name: &str) -> std::path::PathBuf {
    paths::runtime_dir("sloptube").join(format!("cookies-{name}.txt"))
}

pub fn write_cookie_file(session: &Session, name: &str) -> Result<std::path::PathBuf> {
    let path = cookie_file_path(name);
    paths::write_private(&path, session.to_netscape().as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_dates() {
        assert_eq!(parse_cookie_date("Fri, 08-Oct-2027 19:31:48 GMT"), Some(1823023908));
        assert_eq!(parse_cookie_date("Thu, 01 Jan 1970 00:00:01 GMT"), Some(1));
    }

    #[test]
    fn set_cookie_roundtrip() {
        let mut s = Session::default();
        assert!(s.apply_set_cookie(
            "SAPISID=abc; expires=Fri, 08-Oct-2099 19:31:48 GMT; path=/; domain=.youtube.com; Secure",
            "www.youtube.com"
        ));
        assert!(s.apply_set_cookie("SID=x; Max-Age=3600; domain=.youtube.com", "www.youtube.com"));
        assert!(s.is_signed_in());
        assert!(s.cookie_header("www.youtube.com").contains("SAPISID=abc"));
        assert!(s.authorization("https://www.youtube.com").unwrap().starts_with("SAPISIDHASH "));
        // Deletion via an expired date
        assert!(s.apply_set_cookie(
            "SID=; expires=Thu, 01 Jan 1970 00:00:01 GMT; domain=.youtube.com",
            "www.youtube.com"
        ));
        assert!(!s.is_signed_in());
    }
}
