//! Minimal DASH manifests for YouTube's adaptive streams.
//!
//! YouTube serves each quality as one file with a byte index (`sidx` for MP4,
//! `Cues` for WebM). Wrapping the chosen video and audio files in an on-demand
//! MPD lets GStreamer's dashdemux2 fetch exactly the fragments it needs via
//! HTTP range requests: seeks are instant and buffering stays bounded.

use std::fmt::Write;
use std::path::PathBuf;

use crate::config;
use crate::extractor::{Format, Ranges, StreamInfo};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn mime_and_codecs(f: &Format, r: &Ranges) -> (String, String) {
    let raw = r.mime.as_deref().unwrap_or_default();
    let (mime, rest) = raw.split_once(';').unwrap_or((raw, ""));
    let codecs = rest
        .split('"')
        .nth(1)
        .map(str::to_owned)
        .or_else(|| {
            if f.has_video() {
                f.vcodec.clone()
            } else {
                f.acodec.clone()
            }
        })
        .unwrap_or_default();
    let mime = if mime.trim().is_empty() {
        let kind = if f.has_video() { "video" } else { "audio" };
        let ext = match f.ext.as_deref() {
            Some("webm") => "webm",
            _ => "mp4",
        };
        format!("{kind}/{ext}")
    } else {
        mime.trim().to_owned()
    };
    (mime, codecs)
}

fn representation(out: &mut String, f: &Format, r: &Ranges) {
    let (mime, codecs) = mime_and_codecs(f, r);
    let bandwidth = r
        .bitrate
        .or_else(|| f.tbr.map(|t| (t * 1000.0) as u64))
        .unwrap_or(1_000_000);
    let _ = write!(
        out,
        r#"<Representation id="{}" mimeType="{}" codecs="{}" bandwidth="{}""#,
        esc(&f.format_id),
        esc(&mime),
        esc(&codecs),
        bandwidth
    );
    if let (Some(w), Some(h)) = (f.width, f.height) {
        let _ = write!(out, r#" width="{w}" height="{h}""#);
    }
    if let Some(fps) = f.fps.filter(|f| *f > 0.0) {
        let _ = write!(out, r#" frameRate="{}""#, fps.round() as u32);
    }
    if let Some(sr) = r.audio_sample_rate.as_deref() {
        let _ = write!(out, r#" audioSamplingRate="{}""#, esc(sr));
    }
    let _ = write!(
        out,
        r#"><BaseURL>{}</BaseURL><SegmentBase indexRange="{}-{}"><Initialization range="{}-{}"/></SegmentBase></Representation>"#,
        esc(&f.url),
        r.index[0],
        r.index[1],
        r.init[0],
        r.init[1]
    );
}

/// Build an on-demand MPD for one video and one audio stream (None if byte ranges are missing).
pub fn manifest(info: &StreamInfo, video: &Format, audio: &Format) -> Option<String> {
    let vr = info.ranges.get(&video.format_id)?;
    let ar = info.ranges.get(&audio.format_id)?;
    let duration = info.duration.unwrap_or(0.0).max(1.0);
    let mut out = String::with_capacity(4096);
    let _ = write!(
        out,
        r#"<?xml version="1.0" encoding="UTF-8"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011" minBufferTime="PT1.5S" mediaPresentationDuration="PT{duration:.3}S"><Period>"#
    );
    out.push_str(r#"<AdaptationSet contentType="video" subsegmentAlignment="true">"#);
    representation(&mut out, video, vr);
    out.push_str(r#"</AdaptationSet><AdaptationSet contentType="audio" subsegmentAlignment="true">"#);
    representation(&mut out, audio, ar);
    out.push_str("</AdaptationSet></Period></MPD>");
    Some(out)
}

/// Write the manifest into the private runtime directory and return its path.
pub fn write_manifest(info: &StreamInfo, video: &Format, audio: &Format) -> Option<PathBuf> {
    let xml = manifest(info, video, audio)?;
    let safe: String = info
        .id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = config::runtime_dir().join(format!("{safe}-{}-{}.mpd", video.format_id, audio.format_id));
    if let Err(e) = crate::auth::write_private(&path, xml.as_bytes()) {
        log::warn!("cannot write DASH manifest: {e}");
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_escapes_and_ranges() {
        let mut info = StreamInfo {
            id: "abc".into(),
            duration: Some(10.0),
            ..Default::default()
        };
        info.ranges.insert(
            "399".into(),
            Ranges {
                init: [0, 739],
                index: [740, 2271],
                mime: Some(r#"video/mp4; codecs="av01.0.09M.08""#.into()),
                bitrate: Some(1_500_000),
                audio_sample_rate: None,
            },
        );
        info.ranges.insert(
            "251".into(),
            Ranges {
                init: [0, 258],
                index: [259, 1000],
                mime: Some(r#"audio/webm; codecs="opus""#.into()),
                bitrate: Some(130_000),
                audio_sample_rate: Some("48000".into()),
            },
        );
        let v = Format {
            format_id: "399".into(),
            url: "https://x/videoplayback?a=1&b=2".into(),
            vcodec: Some("av01".into()),
            acodec: Some("none".into()),
            width: Some(1920),
            height: Some(1080),
            fps: Some(60.0),
            ..Default::default()
        };
        let a = Format {
            format_id: "251".into(),
            url: "https://x/a".into(),
            vcodec: Some("none".into()),
            acodec: Some("opus".into()),
            ..Default::default()
        };
        let m = manifest(&info, &v, &a).unwrap();
        assert!(m.contains("a=1&amp;b=2"));
        assert!(m.contains(r#"indexRange="740-2271""#));
        assert!(m.contains(r#"codecs="av01.0.09M.08""#));
        assert!(m.contains(r#"mimeType="audio/webm""#));
        assert!(m.contains(r#"frameRate="60""#));
    }
}
