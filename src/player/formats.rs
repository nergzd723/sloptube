//! Choosing which YouTube streams to play for the local hardware.

use anyhow::{Result, bail};

use super::decoders::{Codec, DecoderSupport};
use crate::extractor::{Format, StreamInfo};
use crate::settings::CodecPref;

#[derive(Debug, Clone)]
pub struct Prefs {
    pub codec: CodecPref,
    pub max_height: u32,
    pub allow_60fps: bool,
    pub hw_decode: bool,
    pub audio_only: bool,
}

#[derive(Debug, Clone)]
pub enum Selection {
    /// Separate DASH video and audio streams, synced in one pipeline.
    Separate { video: Format, audio: Format },
    /// A single stream containing both audio and video.
    Muxed(Format),
    /// An HLS manifest (live streams).
    Manifest { url: String, headers: Vec<(String, String)> },
    AudioOnly(Format),
}

impl Selection {
    pub fn video_format(&self) -> Option<&Format> {
        match self {
            Selection::Separate { video, .. } => Some(video),
            Selection::Muxed(f) => Some(f),
            _ => None,
        }
    }

    pub fn audio_format(&self) -> Option<&Format> {
        match self {
            Selection::Separate { audio, .. } => Some(audio),
            Selection::Muxed(f) | Selection::AudioOnly(f) => Some(f),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualityOption {
    pub height: u32,
    pub fps: u32,
    pub codec: Codec,
    pub hardware: bool,
}

impl QualityOption {
    pub fn label(&self) -> String {
        let fps = if self.fps > 30 { self.fps.to_string() } else { String::new() };
        format!("{}p{fps}", self.height)
    }
}

pub fn codec_order(pref: CodecPref, support: &DecoderSupport, hw_decode: bool) -> Vec<Codec> {
    let mut order: Vec<Codec> = Vec::new();
    let preferred = match pref {
        CodecPref::Av1 => Some(Codec::Av1),
        CodecPref::Vp9 => Some(Codec::Vp9),
        CodecPref::H264 => Some(Codec::H264),
        CodecPref::Auto => None,
    };
    if let Some(p) = preferred {
        order.push(p);
    }
    if hw_decode {
        // Most efficient codecs the hardware can do...
        for c in [Codec::Av1, Codec::Vp9, Codec::H264] {
            if support.has_hw(c) && !order.contains(&c) {
                order.push(c);
            }
        }
    }
    // ...then the cheapest to decode in software.
    for c in [Codec::H264, Codec::Vp9, Codec::Av1] {
        if support.can_decode(c) && !order.contains(&c) {
            order.push(c);
        }
    }
    order.retain(|c| support.can_decode(*c));
    order
}

fn is_sdr(f: &Format) -> bool {
    f.dynamic_range.as_deref().map_or(true, |d| d == "SDR")
}

fn video_candidates<'a>(info: &'a StreamInfo, support: &DecoderSupport) -> Vec<&'a Format> {
    info.formats
        .iter()
        .filter(|f| {
            f.has_video()
                && !f.has_audio()
                && f.is_progressive()
                && !f.has_drm.unwrap_or(false)
                && is_sdr(f)
                && f.height.is_some()
                && support.can_decode(Codec::from_vcodec(f.vcodec.as_deref().unwrap_or("")))
        })
        .collect()
}

fn audio_score(f: &Format, support: &DecoderSupport, want_mp4: bool) -> Option<(i64, i64, i64, i64, i64)> {
    if f.has_video() || !f.has_audio() || !f.is_progressive() || f.has_drm.unwrap_or(false) {
        return None;
    }
    let acodec = f.acodec.as_deref().unwrap_or("");
    let codec_rank = if acodec.starts_with("opus") && support.opus {
        2
    } else if acodec.starts_with("mp4a") && support.aac {
        1
    } else {
        return None;
    };
    let drc = f.format_id.contains("drc");
    let note = f.format_note.as_deref().unwrap_or("").to_ascii_lowercase();
    let original = note.contains("original") || note.contains("default");
    let lang_pref = f.language_preference.unwrap_or(0);
    // MP4 video plays through a DASH manifest, which needs MP4 audio to seek.
    let container_match = (acodec.starts_with("mp4a") == want_mp4) as i64;
    Some((
        if original { 1 } else { 0 } * 100 + lang_pref.clamp(-50, 50),
        container_match,
        if drc { 0 } else { 1 },
        codec_rank,
        f.abr.or(f.tbr).unwrap_or(0.0) as i64,
    ))
}

pub fn is_mp4(f: &Format) -> bool {
    matches!(f.ext.as_deref(), Some("mp4") | Some("m4a"))
        || f.vcodec.as_deref().is_some_and(|v| v.starts_with("avc1") || v.starts_with("av01"))
}

pub fn best_audio<'a>(info: &'a StreamInfo, support: &DecoderSupport, want_mp4: bool) -> Option<&'a Format> {
    info.formats
        .iter()
        .filter_map(|f| audio_score(f, support, want_mp4).map(|s| (s, f)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, f)| f)
}

pub fn quality_options(info: &StreamInfo, support: &DecoderSupport, prefs: &Prefs) -> Vec<QualityOption> {
    let order = codec_order(prefs.codec, support, prefs.hw_decode);
    let mut out: Vec<QualityOption> = Vec::new();
    for f in video_candidates(info, support) {
        let codec = Codec::from_vcodec(f.vcodec.as_deref().unwrap_or(""));
        if !order.contains(&codec) {
            continue;
        }
        let opt = QualityOption {
            height: f.height.unwrap_or(0),
            fps: f.fps.unwrap_or(30.0).round() as u32,
            codec,
            hardware: prefs.hw_decode && support.has_hw(codec),
        };
        if !prefs.allow_60fps && opt.fps > 30 && out.iter().any(|o| o.height == opt.height) {
            continue;
        }
        if let Some(existing) = out.iter_mut().find(|o| o.height == opt.height) {
            // Keep one entry per height: hardware first, then higher fps, then codec order.
            let rank = |o: &QualityOption| {
                (
                    o.hardware,
                    if prefs.allow_60fps { o.fps } else { 0 },
                    std::cmp::Reverse(order.iter().position(|c| *c == o.codec)),
                )
            };
            if rank(&opt) > rank(existing) {
                *existing = opt;
            }
        } else {
            out.push(opt);
        }
    }
    out.sort_by_key(|o| std::cmp::Reverse(o.height));
    out
}

/// Pick streams. `height` forces a specific quality from the quality menu.
pub fn select(info: &StreamInfo, support: &DecoderSupport, prefs: &Prefs, height: Option<u32>) -> Result<Selection> {
    if info.is_live {
        let manifest = info
            .formats
            .iter()
            .filter(|f| f.protocol.as_deref().is_some_and(|p| p.starts_with("m3u8")))
            .find_map(|f| f.manifest_url.clone().map(|u| (u, f)));
        if let Some((url, f)) = manifest {
            return Ok(Selection::Manifest {
                url,
                headers: f.http_headers.clone().into_iter().collect(),
            });
        }
    }

    if prefs.audio_only {
        if let Some(a) = best_audio(info, support, false) {
            return Ok(Selection::AudioOnly(a.clone()));
        }
    }

    let order = codec_order(prefs.codec, support, prefs.hw_decode);
    let candidates = video_candidates(info, support);
    let target = height.unwrap_or(prefs.max_height.max(144));
    let score = |f: &Format| {
        let codec = Codec::from_vcodec(f.vcodec.as_deref().unwrap_or(""));
        let fps = f.fps.unwrap_or(30.0).round() as u32;
        let fps_ok = prefs.allow_60fps || fps <= 30;
        let exact = height.is_some_and(|h| f.height == Some(h));
        (
            exact,
            prefs.hw_decode && support.has_hw(codec),
            f.height.unwrap_or(0),
            fps_ok,
            std::cmp::Reverse(order.iter().position(|c| *c == codec).unwrap_or(99)),
            if fps_ok { fps } else { 0 },
            f.tbr.unwrap_or(0.0) as u64,
        )
    };
    let best_video = candidates
        .iter()
        .copied()
        .filter(|f| f.height.unwrap_or(0) <= target)
        .filter(|f| order.contains(&Codec::from_vcodec(f.vcodec.as_deref().unwrap_or(""))))
        .max_by_key(|f| score(f))
        .or_else(|| candidates.iter().copied().min_by_key(|f| f.height.unwrap_or(0)));

    let audio = best_video
        .and_then(|v| best_audio(info, support, is_mp4(v) && info.ranges.contains_key(&v.format_id)))
        .or_else(|| best_audio(info, support, false))
        .cloned();
    match (best_video, audio) {
        (Some(v), Some(a)) => Ok(Selection::Separate {
            video: v.clone(),
            audio: a,
        }),
        _ => {
            // Old-style muxed stream (itag 18 & co.)
            let muxed = info
                .formats
                .iter()
                .filter(|f| f.has_video() && f.has_audio() && f.is_progressive())
                .filter(|f| support.can_decode(Codec::from_vcodec(f.vcodec.as_deref().unwrap_or(""))))
                .max_by_key(|f| f.height.unwrap_or(0));
            if let Some(m) = muxed {
                return Ok(Selection::Muxed(m.clone()));
            }
            // Anything with an HLS manifest as a last resort.
            if let Some(f) = info.formats.iter().find(|f| f.manifest_url.is_some()) {
                return Ok(Selection::Manifest {
                    url: f.manifest_url.clone().unwrap(),
                    headers: f.http_headers.clone().into_iter().collect(),
                });
            }
            bail!("No playable stream found for this device's decoders")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fmt(id: &str, vcodec: &str, acodec: &str, h: Option<u32>, fps: f64, tbr: f64) -> Format {
        Format {
            format_id: id.into(),
            url: format!("https://x/{id}"),
            protocol: Some("https".into()),
            vcodec: Some(vcodec.into()),
            acodec: Some(acodec.into()),
            height: h,
            fps: Some(fps),
            tbr: Some(tbr),
            abr: if vcodec == "none" { Some(tbr) } else { None },
            ..Default::default()
        }
    }

    fn info() -> StreamInfo {
        StreamInfo {
            id: "x".into(),
            formats: vec![
                fmt("140", "none", "mp4a.40.2", None, 0.0, 128.0),
                fmt("251", "none", "opus", None, 0.0, 130.0),
                fmt("251-drc", "none", "opus", None, 0.0, 131.0),
                fmt("299", "avc1.64002A", "none", Some(1080), 60.0, 3000.0),
                fmt("303", "vp9", "none", Some(1080), 60.0, 2000.0),
                fmt("399", "av01.0.09M.08", "none", Some(1080), 60.0, 1500.0),
                fmt("401", "av01.0.13M.08", "none", Some(2160), 60.0, 9000.0),
                fmt("137", "avc1.640028", "none", Some(1080), 30.0, 2500.0),
            ],
            ..Default::default()
        }
    }

    fn support(hw: &[Codec], sw: &[Codec]) -> DecoderSupport {
        let mut s = DecoderSupport {
            opus: true,
            aac: true,
            ..Default::default()
        };
        s.hw = hw.iter().map(|c| (*c, vec!["hw".to_string()])).collect::<HashMap<_, _>>();
        s.sw = sw.iter().map(|c| (*c, vec!["sw".to_string()])).collect::<HashMap<_, _>>();
        s
    }

    fn prefs(max: u32) -> Prefs {
        Prefs {
            codec: CodecPref::Auto,
            max_height: max,
            allow_60fps: true,
            hw_decode: true,
            audio_only: false,
        }
    }

    #[test]
    fn prefers_hardware_av1() {
        let s = support(&[Codec::Av1, Codec::Vp9, Codec::H264], &[Codec::H264, Codec::Vp9]);
        let sel = select(&info(), &s, &prefs(1080), None).unwrap();
        assert_eq!(sel.video_format().unwrap().format_id, "399");
        assert_eq!(sel.audio_format().unwrap().format_id, "251");
    }

    #[test]
    fn h264_only_hardware() {
        let s = support(&[Codec::H264], &[Codec::H264, Codec::Vp9, Codec::Av1]);
        let sel = select(&info(), &s, &prefs(2160), None).unwrap();
        assert_eq!(sel.video_format().unwrap().format_id, "299");
    }

    #[test]
    fn forced_height() {
        let s = support(&[Codec::Av1], &[Codec::H264]);
        let sel = select(&info(), &s, &prefs(1080), Some(2160)).unwrap();
        assert_eq!(sel.video_format().unwrap().format_id, "401");
        let q = quality_options(&info(), &s, &prefs(1080));
        assert_eq!(q.iter().map(|o| o.height).collect::<Vec<_>>(), vec![2160, 1080]);
    }

    #[test]
    fn software_only_prefers_h264() {
        let s = support(&[], &[Codec::H264, Codec::Vp9, Codec::Av1]);
        let sel = select(&info(), &s, &prefs(1080), None).unwrap();
        assert_eq!(sel.video_format().unwrap().format_id, "299");
    }
}
