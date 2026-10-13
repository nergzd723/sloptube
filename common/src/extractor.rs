//! Stream URL extraction through yt-dlp, for both apps.
//!
//! A resident Python worker (see `ytdlp_helper.py`) keeps yt-dlp imported, so
//! extractions after the first take about a second instead of three. One
//! worker serves SlopTube and Music (each costs 55-80 MB), started on demand
//! and stopped after a few idle minutes. If it can't be started we fall back
//! to running the `yt-dlp` CLI per video.

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
use tokio::process::{ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

const HELPER_SRC: &str = include_str!("ytdlp_helper.py");
/// Stream URLs stay valid for about six hours; don't keep them that long.
const CACHE_TTL: Duration = Duration::from_secs(2 * 3600);
const CACHE_MAX: usize = 96;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the worker may sit idle before it exits (`SLOPTUBE_HELPER_IDLE`
/// seconds; 0 keeps it running).
fn idle_timeout() -> Option<Duration> {
    let secs = std::env::var("SLOPTUBE_HELPER_IDLE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(300);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// What to extract and how.
#[derive(Debug, Clone)]
pub struct Request {
    /// Only audio formats, no captions or DASH ranges (Music).
    pub audio_only: bool,
    /// The site the watch URL is on (`www.youtube.com`, `music.youtube.com`).
    pub host: &'static str,
    /// Netscape cookie file of the signed-in account, if yt-dlp should use it.
    pub cookies: Option<PathBuf>,
}

impl Request {
    pub fn video(cookies: Option<PathBuf>) -> Self {
        Self { audio_only: false, host: "www.youtube.com", cookies }
    }

    pub fn music(cookies: Option<PathBuf>) -> Self {
        Self { audio_only: true, host: "music.youtube.com", cookies }
    }

    /// Results depend on the kind of request and on who asked: a stream
    /// extracted signed out isn't reused once signed in, or for someone else.
    fn cache_key(&self, id: &str) -> String {
        format!(
            "{id}|{}|{}",
            if self.audio_only { "audio" } else { "all" },
            self.cookies.as_ref().map_or(0, |_| crate::session::generation() + 1)
        )
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Format {
    pub format_id: String,
    pub url: String,
    #[serde(default)]
    pub manifest_url: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub ext: Option<String>,
    #[serde(default)]
    pub vcodec: Option<String>,
    #[serde(default)]
    pub acodec: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub fps: Option<f64>,
    #[serde(default)]
    pub tbr: Option<f64>,
    #[serde(default)]
    pub abr: Option<f64>,
    #[serde(default)]
    pub asr: Option<u32>,
    #[serde(default)]
    pub audio_channels: Option<u32>,
    #[serde(default)]
    pub filesize: Option<u64>,
    #[serde(default)]
    pub filesize_approx: Option<u64>,
    #[serde(default)]
    pub dynamic_range: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub language_preference: Option<i64>,
    #[serde(default)]
    pub format_note: Option<String>,
    #[serde(default)]
    pub http_headers: HashMap<String, String>,
    #[serde(default)]
    pub has_drm: Option<bool>,
}

fn has_codec(c: &Option<String>) -> bool {
    c.as_deref().is_some_and(|c| c != "none" && !c.is_empty())
}

impl Format {
    pub fn has_video(&self) -> bool {
        has_codec(&self.vcodec)
    }

    pub fn has_audio(&self) -> bool {
        has_codec(&self.acodec)
    }

    /// Plain progressive HTTP download (as opposed to HLS/DASH manifests).
    pub fn is_progressive(&self) -> bool {
        matches!(self.protocol.as_deref(), Some("https") | Some("http") | None)
    }

    pub fn size(&self) -> Option<u64> {
        self.filesize.or(self.filesize_approx)
    }

    pub fn is_opus(&self) -> bool {
        self.acodec.as_deref().is_some_and(|c| c.starts_with("opus"))
    }

    /// Only the headers a media server cares about.
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
pub struct Chapter {
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    #[serde(default)]
    pub title: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Caption {
    pub lang: String,
    pub name: String,
    pub url: String,
}

/// Playback-tracking URLs of the player response, for watch history.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Tracking {
    #[serde(default)]
    pub playback: Option<String>,
    #[serde(default)]
    pub watchtime: Option<String>,
}

/// DASH byte ranges of one adaptive stream (from the raw player response).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Ranges {
    pub init: [u64; 2],
    pub index: [u64; 2],
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub bitrate: Option<u64>,
    #[serde(default)]
    pub audio_sample_rate: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
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
    pub chapters: Vec<Chapter>,
    #[serde(default)]
    pub captions: Vec<Caption>,
    #[serde(default)]
    pub tracking: Option<Tracking>,
    /// format_id -> byte ranges, for building a DASH manifest.
    #[serde(default)]
    pub ranges: HashMap<String, Ranges>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, HelperFailure>>>>>;
type Waiters = Vec<oneshot::Sender<std::result::Result<Arc<StreamInfo>, Shared>>>;

/// An extraction's error, handed to the requests that shared it.
#[derive(Clone)]
struct Shared {
    message: String,
    network: bool,
}

impl Shared {
    fn of(e: &anyhow::Error) -> Self {
        Self {
            message: format!("{e:#}"),
            network: e.downcast_ref::<NetworkError>().is_some(),
        }
    }

    fn into_error(self) -> anyhow::Error {
        if self.network {
            NetworkError(self.message).into()
        } else {
            anyhow!(self.message)
        }
    }
}

/// Extraction failed because YouTube couldn't be reached at all.
#[derive(Debug)]
pub struct NetworkError(pub String);

impl std::fmt::Display for NetworkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NetworkError {}

struct Helper {
    stdin: ChildStdin,
}

pub struct Extractor {
    helper: AsyncMutex<Option<Helper>>,
    pending: Pending,
    next_id: AtomicU64,
    helper_broken: Mutex<bool>,
    last_use: Mutex<Instant>,
    /// By `Request::cache_key`.
    cache: Mutex<HashMap<String, (Instant, Arc<StreamInfo>)>>,
    /// Extractions in flight, so a prefetch and a play share one request.
    inflight: Mutex<HashMap<String, Waiters>>,
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
        inflight: Mutex::new(HashMap::new()),
    })
}

/// Locate the `yt-dlp` executable.
pub fn ytdlp_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SLOPTUBE_YTDLP") {
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
    pub fn cached(&self, id: &str, req: &Request) -> Option<Arc<StreamInfo>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(&req.cache_key(id))
            .filter(|(t, _)| t.elapsed() < CACHE_TTL)
            .map(|(_, i)| i.clone())
    }

    /// Forget what was extracted for `id` (e.g. after its URLs stopped working).
    pub fn invalidate(&self, id: &str) {
        let prefix = format!("{id}|");
        self.cache.lock().unwrap().retain(|k, _| !k.starts_with(&prefix));
    }

    /// Start the worker in the background when playing looks likely soon.
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

    /// Start the worker ahead of the first extraction (tokio runtime).
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

    /// Stream information for a video or song (must run on the tokio runtime).
    /// Concurrent requests for the same thing share one extraction. A failure
    /// to reach YouTube at all comes back as a [`NetworkError`].
    pub async fn extract(&self, id: &str, req: &Request) -> Result<Arc<StreamInfo>> {
        if let Some(info) = self.cached(id, req) {
            return Ok(info);
        }
        let key = req.cache_key(id);
        let waiter = {
            let mut inflight = self.inflight.lock().unwrap();
            match inflight.get_mut(&key) {
                Some(waiters) => {
                    let (tx, rx) = oneshot::channel();
                    waiters.push(tx);
                    Some(rx)
                }
                None => {
                    inflight.insert(key.clone(), Vec::new());
                    None
                }
            }
        };
        if let Some(rx) = waiter {
            return match rx.await {
                Ok(Ok(info)) => Ok(info),
                Ok(Err(e)) => Err(e.into_error()),
                Err(_) => Err(anyhow!("extraction was abandoned")),
            };
        }
        let res = self.extract_uncached(id, req, &key).await;
        let waiters = self.inflight.lock().unwrap().remove(&key).unwrap_or_default();
        for w in waiters {
            let _ = w.send(res.as_ref().map(Arc::clone).map_err(Shared::of));
        }
        res
    }

    async fn extract_uncached(&self, id: &str, req: &Request, key: &str) -> Result<Arc<StreamInfo>> {
        let url = format!("https://{}/watch?v={id}", req.host);
        let cookies = req.cookies.as_deref();
        let started = Instant::now();
        let broken = *self.helper_broken.lock().unwrap();
        let value = if broken {
            self.cli_extract(&url, req).await?
        } else {
            match self.helper_request(&url, req).await {
                Ok(v) => v,
                Err(HelperError::Unavailable(e)) => {
                    log::warn!("yt-dlp helper unavailable ({e:#}); falling back to the CLI");
                    *self.helper_broken.lock().unwrap() = true;
                    self.cli_extract(&url, req).await?
                }
                Err(HelperError::Failed(f)) if f.network => return Err(NetworkError(f.message).into()),
                Err(HelperError::Failed(f)) => bail!(f.message),
            }
        };
        let info: StreamInfo = serde_json::from_value(value).context("unexpected yt-dlp output")?;
        log::info!(
            "extracted {id} in {:.2}s ({} {}formats{})",
            started.elapsed().as_secs_f64(),
            info.formats.len(),
            if req.audio_only { "audio " } else { "" },
            if cookies.is_some() { ", signed in" } else { "" },
        );
        let info = Arc::new(info);
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|_, (t, _)| t.elapsed() < CACHE_TTL);
        if cache.len() >= CACHE_MAX
            && let Some(oldest) = cache.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(key.to_owned(), (Instant::now(), info.clone()));
        Ok(info)
    }

    async fn start_helper(&self) -> Result<Helper> {
        let ytdlp = ytdlp_path().ok_or_else(|| anyhow!("yt-dlp is not installed"))?;
        let (python, pythonpath) = python_for(&ytdlp);
        let python = std::env::var("SLOPTUBE_YTDLP_PYTHON").map(PathBuf::from).unwrap_or(python);
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

        // Wait for the "ready" line.
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
                        None => Err(HelperFailure {
                            message: v
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown yt-dlp error")
                                .to_owned(),
                            network: v.get("network").and_then(Value::as_bool).unwrap_or(false),
                        }),
                    };
                    let _ = tx.send(res);
                }
            }
            // Helper died: fail everything that's still waiting.
            let mut p = pending.lock().unwrap();
            for (_, tx) in p.drain() {
                let _ = tx.send(Err(HelperFailure::other("yt-dlp helper exited")));
            }
            drop(child);
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
                        // Closing its stdin ends the worker's read loop.
                        *guard = None;
                        drop(guard);
                        crate::memory::trim_soon();
                        return;
                    }
                }
            });
        }
        Ok(Helper { stdin })
    }

    async fn helper_request(&self, url: &str, request: &Request) -> std::result::Result<Value, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut req = json!({ "id": id, "op": "extract", "url": url, "audio_only": request.audio_only });
        if let Some(c) = &request.cookies {
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
            Ok(Ok(Err(f))) => {
                if f.message == "yt-dlp helper exited" {
                    *self.helper.lock().await = None;
                }
                Err(HelperError::Failed(f))
            }
            Ok(Err(_)) => Err(HelperError::Failed(HelperFailure::other("yt-dlp helper went away"))),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(HelperError::Failed(HelperFailure::other("yt-dlp took too long to respond")))
            }
        };
        *self.last_use.lock().unwrap() = Instant::now();
        res
    }

    async fn cli_extract(&self, url: &str, req: &Request) -> Result<Value> {
        let cookiefile = req.cookies.as_deref();
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
        // Normalise to the helper's shape.
        let chapters = v
            .get("chapters")
            .and_then(Value::as_array)
            .map(|chapters| {
                chapters
                    .iter()
                    .map(|c| json!({"start": c["start_time"], "end": c["end_time"], "title": c["title"]}))
                    .collect()
            })
            .unwrap_or_default();
        v["chapters"] = Value::Array(chapters);
        v["captions"] = json!([]);
        v["is_live"] = json!(v["is_live"].as_bool().unwrap_or(false));
        if let Some(formats) = v.get_mut("formats").and_then(Value::as_array_mut) {
            let audio = |f: &Value| {
                f.get("acodec").and_then(Value::as_str).is_some_and(|c| c != "none")
                    && f.get("vcodec").and_then(Value::as_str).is_none_or(|c| c == "none")
            };
            formats.retain(|f| f.get("url").and_then(Value::as_str).is_some() && (!req.audio_only || audio(f)));
        }
        Ok(v)
    }
}

enum HelperError {
    Unavailable(anyhow::Error),
    Failed(HelperFailure),
}

struct HelperFailure {
    message: String,
    /// yt-dlp couldn't reach the server (see `NetworkError`).
    network: bool,
}

impl HelperFailure {
    fn other(message: &str) -> Self {
        Self {
            message: message.to_owned(),
            network: false,
        }
    }
}
