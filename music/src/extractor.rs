//! Music's side of the shared yt-dlp extractor (`common::extractor`):
//! audio-only requests, as the signed-in account when allowed, and picking the
//! stream to play.

use std::path::PathBuf;
use std::sync::Mutex;

pub use common::extractor::{Format, Request, StreamInfo, extractor, ytdlp_path};

use crate::settings::AudioQuality;

/// The account's cookie file for yt-dlp, while it may be used (`account.rs`).
static COOKIES: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_cookie_file(path: Option<PathBuf>) {
    *COOKIES.lock().unwrap() = path;
}

/// How Music asks for a song's streams.
pub fn request() -> Request {
    Request::music(COOKIES.lock().unwrap().clone())
}

/// The audio stream to play at `quality`: the original track (not a dub),
/// the best bitrate within the quality's limit, Opus before AAC.
pub fn select(info: &StreamInfo, quality: AudioQuality) -> Option<&Format> {
    let usable: Vec<&Format> = info
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
        assert_eq!(select(&info, AudioQuality::High).unwrap().format_id, "251");
        assert_eq!(select(&info, AudioQuality::Normal).unwrap().format_id, "250");
        assert_eq!(select(&info, AudioQuality::Low).unwrap().format_id, "249");
        let aac_only = StreamInfo {
            formats: vec![fmt("140", "mp4a.40.2", 129.5)],
            ..Default::default()
        };
        assert_eq!(select(&aac_only, AudioQuality::Low).unwrap().format_id, "140");
    }
}
