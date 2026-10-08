//! Stream URL extraction through yt-dlp.
//!
//! A persistent Python worker (see `ytdlp_helper.py`) keeps yt-dlp imported;
//! if it can't be started we fall back to running the `yt-dlp` CLI per video.

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
const CACHE_TTL: Duration = Duration::from_secs(2 * 3600);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

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

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Tracking {
    pub playback: Option<String>,
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

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>;

struct Helper {
    stdin: ChildStdin,
}

pub struct Extractor {
    helper: AsyncMutex<Option<Helper>>,
    pending: Pending,
    next_id: AtomicU64,
    helper_broken: Mutex<bool>,
    cache: Mutex<HashMap<String, (Instant, Arc<StreamInfo>)>>,
}

static EXTRACTOR: OnceLock<Extractor> = OnceLock::new();

pub fn extractor() -> &'static Extractor {
    EXTRACTOR.get_or_init(|| Extractor {
        helper: AsyncMutex::new(None),
        pending: Arc::new(Mutex::new(HashMap::new())),
        next_id: AtomicU64::new(1),
        helper_broken: Mutex::new(false),
        cache: Mutex::new(HashMap::new()),
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
    pub fn cached(&self, id: &str) -> Option<Arc<StreamInfo>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(id)
            .filter(|(t, _)| t.elapsed() < CACHE_TTL)
            .map(|(_, i)| i.clone())
    }

    pub fn invalidate(&self, id: &str) {
        self.cache.lock().unwrap().remove(id);
    }

    /// Extract stream information for a video (must run on the tokio runtime).
    pub async fn extract(&self, id: &str, cookiefile: Option<PathBuf>) -> Result<Arc<StreamInfo>> {
        if let Some(info) = self.cached(id) {
            return Ok(info);
        }
        let url = format!("https://www.youtube.com/watch?v={id}");
        let started = Instant::now();
        let broken = *self.helper_broken.lock().unwrap();
        let value = if broken {
            self.cli_extract(&url, cookiefile.as_deref()).await?
        } else {
            match self.helper_request(&url, cookiefile.as_deref()).await {
                Ok(v) => v,
                Err(HelperError::Unavailable(e)) => {
                    log::warn!("yt-dlp helper unavailable ({e:#}); falling back to the CLI");
                    *self.helper_broken.lock().unwrap() = true;
                    self.cli_extract(&url, cookiefile.as_deref()).await?
                }
                Err(HelperError::Failed(msg)) => bail!(msg),
            }
        };
        let info: StreamInfo = serde_json::from_value(value).context("unexpected yt-dlp output")?;
        log::info!(
            "extracted {} in {:.2}s ({} formats)",
            id,
            started.elapsed().as_secs_f64(),
            info.formats.len()
        );
        let info = Arc::new(info);
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|_, (t, _)| t.elapsed() < CACHE_TTL);
        cache.insert(id.to_owned(), (Instant::now(), info.clone()));
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
                        None => Err(v
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown yt-dlp error")
                            .to_owned()),
                    };
                    let _ = tx.send(res);
                }
            }
            // Helper died: fail everything that's still waiting.
            let mut p = pending.lock().unwrap();
            for (_, tx) in p.drain() {
                let _ = tx.send(Err("yt-dlp helper exited".into()));
            }
            drop(child);
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!("yt-dlp: {line}");
            }
        });
        Ok(Helper { stdin })
    }

    async fn helper_request(&self, url: &str, cookiefile: Option<&Path>) -> std::result::Result<Value, HelperError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut req = json!({ "id": id, "op": "extract", "url": url });
        if let Some(c) = cookiefile {
            req["cookiefile"] = json!(c);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

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

        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
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
        }
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
            formats.retain(|f| f.get("url").and_then(Value::as_str).is_some());
        }
        Ok(v)
    }
}

enum HelperError {
    Unavailable(anyhow::Error),
    Failed(String),
}
