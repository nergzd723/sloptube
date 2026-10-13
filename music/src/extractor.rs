//! Audio stream URLs through yt-dlp.
//!
//! A resident Python worker (see `ytdlp_helper.py`) keeps yt-dlp imported, so
//! extractions after the first take about a second instead of three. That
//! process costs 60–80 MB, so it is started on demand and stopped after a few
//! idle minutes. If it can't be started we fall back to the `yt-dlp` CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

use crate::settings::AudioQuality;

const HELPER_SRC: &str = include_str!("ytdlp_helper.py");
/// Stream URLs stay valid for about six hours; don't keep them that long.
const CACHE_TTL: Duration = Duration::from_secs(2 * 3600);
const CACHE_MAX: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

fn idle_timeout() -> Option<Duration> {
    let secs = std::env::var("SLOPTUBE_MUSIC_HELPER_IDLE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300);
    (secs > 0).then(|| Duration::from_secs(secs))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Format {
    pub format_id: String,
    pub url: String,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub acodec: Option<String>,
    #[serde(default)]
    pub abr: Option<f64>,
    #[serde(default)]
    pub language_preference: Option<i64>,
    #[serde(default)]
    pub format_note: Option<String>,
    #[serde(default)]
    pub http_headers: HashMap<String, String>,
    #[serde(default)]
    pub has_drm: Option<bool>,
}

impl Format {
    pub fn is_progressive(&self) -> bool {
        matches!(self.protocol.as_deref(), Some("https") | Some("http") | None)
    }

    pub fn is_opus(&self) -> bool {
        self.acodec.as_deref().is_some_and(|c| c.starts_with("opus"))
    }

    /// Keep only the headers a media server cares about.
    pub fn headers(&self) -> Vec<(String, String)> {
        self.http_headers
            .iter()
            .filter(|(k, _)| {
                let k = k.to_ascii_lowercase();
                k == "user-agent" || k == "accept-language" || k == "referer" || k == "origin"
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {} {:.0} kbps",
            self.format_id,
            self.acodec.as_deref().unwrap_or("?"),
            self.abr.unwrap_or(0.0)
        )
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Tracking {
    /// Pinged to add a play to the account's history.
    #[serde(default)]
    pub playback: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)] // Part of the helper's protocol; handy in the debugger.
pub struct StreamInfo {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub formats: Vec<Format>,
    #[serde(default)]
    pub tracking: Option<Tracking>,
}

impl StreamInfo {
    /// The audio stream to play at `quality`: the original track (not a dub),
    /// the best bitrate within the quality's limit, Opus before AAC.
    pub fn select(&self, quality: AudioQuality) -> Option<&Format> {
        let usable: Vec<&Format> = self
            .formats
            .iter()
            .filter(|f| !f.url.is_empty() && f.has_drm != Some(true))
            .collect();
        let progressive: Vec<&Format> = usable.iter().copied().filter(|f| f.is_progressive()).collect();
        if progressive.is_empty() {
            // Live streams and odd cases: an HLS audio rendition.
            return usable.into_iter().max_by(|a, b| a.abr.unwrap_or(0.0).total_cmp(&b.abr.unwrap_or(0.0)));
        }
        let original = |f: &Format| {
            let note = f.format_note.as_deref().unwrap_or("").to_ascii_lowercase();
            (note.contains("original") || note.contains("default")) as i64 * 100 + f.language_preference.unwrap_or(0)
        };
        let best_lang = progressive.iter().map(|f| original(f)).max().unwrap_or(0);
        let mut pool: Vec<&Format> = progressive.into_iter().filter(|f| original(f) == best_lang).collect();
        // DRC variants ("drc" in the id) are louder-normalised copies; skip them when possible.
        if pool.iter().any(|f| !f.format_id.contains("drc")) {
            pool.retain(|f| !f.format_id.contains("drc"));
        }
        let max = quality.max_kbps();
        let within: Vec<&Format> = pool.iter().copied().filter(|f| f.abr.unwrap_or(0.0) <= max).collect();
        let key = |f: &&Format| (f.abr.unwrap_or(0.0) / 16.0).round() as i64 * 2 + f.is_opus() as i64;
        if within.is_empty() {
            pool.into_iter().min_by_key(|f| (f.abr.unwrap_or(0.0) * 10.0) as i64)
        } else {
            within.into_iter().max_by_key(key)
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

struct Helper {
    stdin: ChildStdin,
    _child: Child,
}

pub struct Extractor {
    helper: AsyncMutex<Option<Helper>>,
    pending: Pending,
    next_id: AtomicU64,
    helper_broken: Mutex<bool>,
    last_use: Mutex<Instant>,
    cache: Mutex<HashMap<String, (Instant, Arc<StreamInfo>)>>,
    /// Netscape cookie file of the signed-in account, passed to yt-dlp.
    cookie_file: Mutex<Option<PathBuf>>,
    /// Extractions in flight, so a prefetch and a play share one request.
    inflight: Mutex<HashMap<String, Vec<oneshot::Sender<std::result::Result<Arc<StreamInfo>, String>>>>>,
}

static EXTRACTOR: OnceLock<Extractor> = OnceLock::new();

pub fn extractor() -> &'static Extractor {
    EXTRACTOR.get_or_init(|| Extractor {
        helper: AsyncMutex::new(None),
        pending: Arc::new(Mutex::new(HashMap::new())),
        next_id: AtomicU64::new(1),
        helper_broken: Mutex::new(false),
        last_use: Mutex::new(Instant::now()),
        cache: Mutex::new(HashMap::new()),
        cookie_file: Mutex::new(None),
        inflight: Mutex::new(HashMap::new()),
    })
}

/// Locate the `yt-dlp` executable.
pub fn ytdlp_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SLOPTUBE_MUSIC_YTDLP") {
        return Some(PathBuf::from(p));
    }
    gtk::glib::find_program_in_path("yt-dlp")
}

/// Figure out which Python can import yt_dlp, and whether a PYTHONPATH is needed.
fn python_for(ytdlp: &Path) -> (PathBuf, Option<PathBuf>) {
    let default = (PathBuf::from("python3"), None);
    let Ok(data) = std::fs::read(ytdlp) else {
        return default;
    };
    let first_line = data.split(|b| *b == b'\n').next().unwrap_or_default();
    let shebang = String::from_utf8_lossy(first_line);
    let Some(cmd) = shebang.strip_prefix("#!") else {
        return default;
    };
    let mut parts = cmd.split_whitespace();
    let interp = parts.next().unwrap_or("python3");
    let interp = if interp.ends_with("/env") {
        parts.next().unwrap_or("python3").to_owned()
    } else {
        interp.to_owned()
    };
    // The official release binary is a zipapp: importable once it's on PYTHONPATH.
    let is_zip = data.windows(4).any(|w| w == b"PK\x03\x04");
    (PathBuf::from(interp), is_zip.then(|| ytdlp.to_owned()))
}

impl Extractor {
    pub fn cached(&self, id: &str) -> Option<Arc<StreamInfo>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(id)
            .filter(|(t, _)| t.elapsed() < CACHE_TTL)
            .map(|(_, i)| i.clone())
    }

    /// Extract as the signed-in account (or anonymously with `None`). Cached
    /// results belong to the previous identity and are dropped.
    pub fn set_cookie_file(&self, path: Option<PathBuf>) {
        let mut current = self.cookie_file.lock().unwrap();
        if *current != path {
            *current = path;
            self.cache.lock().unwrap().clear();
        }
    }

    pub fn invalidate(&self, id: &str) {
        self.cache.lock().unwrap().remove(id);
    }

    /// Start the helper in the background when playing looks likely soon.
    pub fn warm_up_soon(&'static self) {
        static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if self.helper.try_lock().is_ok_and(|g| g.is_some()) {
            return;
        }
        if ASKED.swap(true, Ordering::AcqRel) {
            return;
        }
        crate::runtime::spawn(async move {
            self.warm_up().await;
            ASKED.store(false, Ordering::Release);
        });
    }

    /// Start the helper ahead of the first extraction (tokio runtime).
    pub async fn warm_up(&self) {
        if *self.helper_broken.lock().unwrap() {
            return;
        }
        let mut guard = self.helper.lock().await;
        if guard.is_none() {
            match self.start_helper().await {
                Ok(h) => {
                    *guard = Some(h);
                    *self.last_use.lock().unwrap() = Instant::now();
                }
                Err(e) => {
                    log::warn!("yt-dlp helper unavailable ({e:#}); using the CLI");
                    *self.helper_broken.lock().unwrap() = true;
                }
            }
        }
    }

    /// Stream information for a video (must run on the tokio runtime).
    /// Concurrent requests for the same video share one extraction.
    pub async fn extract(&self, id: &str) -> Result<Arc<StreamInfo>> {
        if let Some(info) = self.cached(id) {
            return Ok(info);
        }
        let waiter = {
            let mut inflight = self.inflight.lock().unwrap();
            match inflight.get_mut(id) {
                Some(waiters) => {
                    let (tx, rx) = oneshot::channel();
                    waiters.push(tx);
                    Some(rx)
                }
                None => {
                    inflight.insert(id.to_owned(), Vec::new());
                    None
                }
            }
        };
        if let Some(rx) = waiter {
            return match rx.await {
                Ok(Ok(info)) => Ok(info),
                Ok(Err(e)) => Err(anyhow!(e)),
                Err(_) => Err(anyhow!("extraction was abandoned")),
            };
        }
        let res = self.extract_uncached(id).await;
        let waiters = self.inflight.lock().unwrap().remove(id).unwrap_or_default();
        for w in waiters {
            let _ = w.send(res.as_ref().map(Arc::clone).map_err(|e| format!("{e:#}")));
        }
        res
    }

    async fn extract_uncached(&self, id: &str) -> Result<Arc<StreamInfo>> {
        let url = format!("https://music.youtube.com/watch?v={id}");
        let started = Instant::now();
        let broken = *self.helper_broken.lock().unwrap();
        let cookies = self.cookie_file.lock().unwrap().clone();
        let value = if broken {
            self.cli_extract(&url, cookies.as_deref()).await?
        } else {
            match self.helper_request(&url, cookies.as_deref()).await {
                Ok(v) => v,
                Err(HelperError::Unavailable(e)) => {
                    log::warn!("yt-dlp helper unavailable ({e:#}); falling back to the CLI");
                    *self.helper_broken.lock().unwrap() = true;
                    self.cli_extract(&url, cookies.as_deref()).await?
                }
                Err(HelperError::Failed(msg)) => bail!(msg),
            }
        };
        let info: StreamInfo = serde_json::from_value(value).context("unexpected yt-dlp output")?;
        log::info!(
            "extracted {id} in {:.2}s ({} audio formats)",
            started.elapsed().as_secs_f64(),
            info.formats.len()
        );
        let info = Arc::new(info);
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|_, (t, _)| t.elapsed() < CACHE_TTL);
        if cache.len() >= CACHE_MAX
            && let Some(oldest) = cache.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                cache.remove(&oldest);
            }
        cache.insert(id.to_owned(), (Instant::now(), info.clone()));
        Ok(info)
    }

    async fn start_helper(&self) -> Result<Helper> {
        let ytdlp = ytdlp_path().ok_or_else(|| anyhow!("yt-dlp is not installed"))?;
        let (python, pythonpath) = python_for(&ytdlp);
        let python = std::env::var("SLOPTUBE_MUSIC_YTDLP_PYTHON")
            .map(PathBuf::from)
            .unwrap_or(python);
        let mut cmd = Command::new(&python);
        cmd.arg("-u")
            .arg("-c")
            .arg(HELPER_SRC)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(pp) = pythonpath {
            cmd.env("PYTHONPATH", pp);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot run {}", python.display()))?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let stderr = child.stderr.take().unwrap();

        let ready = tokio::time::timeout(Duration::from_secs(20), stdout.next_line())
            .await
            .context("yt-dlp helper did not start in time")??
            .ok_or_else(|| anyhow!("yt-dlp helper exited immediately"))?;
        let ready: Value = serde_json::from_str(&ready).context("bad helper handshake")?;
        if let Some(f) = ready.get("fatal").and_then(Value::as_str) {
            bail!("{f}");
        }
        log::info!(
            "yt-dlp helper ready (yt-dlp {}, {})",
            ready.get("ready").and_then(Value::as_str).unwrap_or("?"),
            python.display()
        );

        let pending = self.pending.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = stdout.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
                let tx = pending.lock().unwrap().remove(&id);
                if let Some(tx) = tx {
                    let res = match v.get("ok") {
                        Some(ok) => Ok(ok.clone()),
                        None => Err(v
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown yt-dlp error")
                            .to_owned()),
                    };
                    let _ = tx.send(res);
                }
            }
            // Helper gone: fail everything that's still waiting.
            let mut p = pending.lock().unwrap();
            for (_, tx) in p.drain() {
                let _ = tx.send(Err("yt-dlp helper exited".into()));
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!("yt-dlp: {line}");
            }
        });
        if let Some(idle) = idle_timeout() {
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    let ex = extractor();
                    let mut guard = ex.helper.lock().await;
                    if guard.is_none() {
                        return;
                    }
                    let idle_for = ex.last_use.lock().unwrap().elapsed();
                    if idle_for >= idle && ex.pending.lock().unwrap().is_empty() {
                        log::info!("stopping the idle yt-dlp helper");
                        *guard = None;
                        drop(guard);
                        crate::memory::trim_soon();
                        return;
                    }
                }
            });
        }
        Ok(Helper { stdin, _child: child })
    }

    async fn helper_request(&self, url: &str, cookiefile: Option<&Path>) -> std::result::Result<Value, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut req = json!({ "id": id, "op": "extract", "url": url });
        if let Some(c) = cookiefile {
            req["cookiefile"] = json!(c);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        *self.last_use.lock().unwrap() = Instant::now();

        {
            let mut guard = self.helper.lock().await;
            for attempt in 0..2 {
                if guard.is_none() {
                    match self.start_helper().await {
                        Ok(h) => *guard = Some(h),
                        Err(e) => {
                            self.pending.lock().unwrap().remove(&id);
                            return Err(HelperError::Unavailable(e));
                        }
                    }
                }
                let line = format!("{req}\n");
                let h = guard.as_mut().unwrap();
                match h.stdin.write_all(line.as_bytes()).await {
                    Ok(()) => break,
                    Err(e) if attempt == 0 => {
                        log::warn!("yt-dlp helper pipe broken ({e}), restarting");
                        *guard = None;
                    }
                    Err(e) => {
                        self.pending.lock().unwrap().remove(&id);
                        return Err(HelperError::Unavailable(e.into()));
                    }
                }
            }
        }

        let res = match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(msg))) => {
                if msg == "yt-dlp helper exited" {
                    *self.helper.lock().await = None;
                }
                Err(HelperError::Failed(msg))
            }
            Ok(Err(_)) => Err(HelperError::Failed("yt-dlp helper went away".into())),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(HelperError::Failed("yt-dlp took too long to respond".into()))
            }
        };
        *self.last_use.lock().unwrap() = Instant::now();
        res
    }

    async fn cli_extract(&self, url: &str, cookiefile: Option<&Path>) -> Result<Value> {
        let ytdlp = ytdlp_path().ok_or_else(|| anyhow!("yt-dlp is not installed"))?;
        let mut cmd = Command::new(ytdlp);
        cmd.args(["-J", "--no-warnings", "--no-playlist", url])
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if let Some(c) = cookiefile {
            cmd.arg("--cookies").arg(c);
        }
        let out = tokio::time::timeout(REQUEST_TIMEOUT, cmd.output())
            .await
            .context("yt-dlp took too long")??;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let msg = err
                .lines()
                .rev()
                .find(|l| l.starts_with("ERROR:"))
                .unwrap_or("yt-dlp failed")
                .trim_start_matches("ERROR: ");
            bail!("{msg}");
        }
        let mut v: Value = serde_json::from_slice(&out.stdout)?;
        v["is_live"] = json!(v["is_live"].as_bool().unwrap_or(false));
        if let Some(formats) = v.get_mut("formats").and_then(Value::as_array_mut) {
            formats.retain(|f| {
                f.get("url").and_then(Value::as_str).is_some()
                    && f.get("acodec").and_then(Value::as_str).is_some_and(|c| c != "none")
                    && f.get("vcodec").and_then(Value::as_str).is_none_or(|c| c == "none")
            });
        }
        Ok(v)
    }
}

enum HelperError {
    Unavailable(anyhow::Error),
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(id: &str, codec: &str, abr: f64) -> Format {
        Format {
            format_id: id.into(),
            url: format!("https://example.com/{id}"),
            protocol: Some("https".into()),
            acodec: Some(codec.into()),
            abr: Some(abr),
            ..Default::default()
        }
    }

    #[test]
    fn audio_selection() {
        let info = StreamInfo {
            formats: vec![
                fmt("139", "mp4a.40.5", 48.8),
                fmt("249", "opus", 51.2),
                fmt("250", "opus", 67.5),
                fmt("140", "mp4a.40.2", 129.5),
                fmt("251", "opus", 134.5),
            ],
            ..Default::default()
        };
        assert_eq!(info.select(AudioQuality::High).unwrap().format_id, "251");
        assert_eq!(info.select(AudioQuality::Normal).unwrap().format_id, "250");
        assert_eq!(info.select(AudioQuality::Low).unwrap().format_id, "249");
        let aac_only = StreamInfo {
            formats: vec![fmt("140", "mp4a.40.2", 129.5)],
            ..Default::default()
        };
        assert_eq!(aac_only.select(AudioQuality::Low).unwrap().format_id, "140");
    }
}
