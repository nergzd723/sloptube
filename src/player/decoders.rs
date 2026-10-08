//! Discovery and ranking of GStreamer decoders (VA-API, V4L2 stateful/stateless, software).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use gst::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Codec {
    Av1,
    Vp9,
    H264,
    Other,
}

impl Codec {
    pub fn from_vcodec(s: &str) -> Codec {
        let s = s.to_ascii_lowercase();
        if s.starts_with("av01") || s == "av1" {
            Codec::Av1
        } else if s.starts_with("vp09") || s.starts_with("vp9") {
            Codec::Vp9
        } else if s.starts_with("avc") || s.starts_with("h264") {
            Codec::H264
        } else {
            Codec::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Codec::Av1 => "AV1",
            Codec::Vp9 => "VP9",
            Codec::H264 => "H.264",
            Codec::Other => "other",
        }
    }

    fn from_caps_name(name: &str) -> Option<Codec> {
        match name {
            "video/x-av1" => Some(Codec::Av1),
            "video/x-vp9" => Some(Codec::Vp9),
            "video/x-h264" => Some(Codec::H264),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DecoderSupport {
    /// codec -> hardware decoder factory names (best first)
    pub hw: HashMap<Codec, Vec<String>>,
    /// codec -> software decoder factory names
    pub sw: HashMap<Codec, Vec<String>>,
    pub opus: bool,
    pub aac: bool,
}

impl DecoderSupport {
    pub fn has_hw(&self, c: Codec) -> bool {
        self.hw.get(&c).is_some_and(|v| !v.is_empty())
    }

    pub fn can_decode(&self, c: Codec) -> bool {
        self.has_hw(c) || self.sw.get(&c).is_some_and(|v| !v.is_empty())
    }

    pub fn summary(&self) -> String {
        let fmt = |m: &HashMap<Codec, Vec<String>>| {
            let mut parts: Vec<String> = [Codec::Av1, Codec::Vp9, Codec::H264]
                .iter()
                .filter_map(|c| m.get(c).and_then(|v| v.first()).map(|f| format!("{} ({f})", c.label())))
                .collect();
            if parts.is_empty() {
                parts.push("none".into());
            }
            parts.join(", ")
        };
        format!("hardware: {}; software: {}", fmt(&self.hw), fmt(&self.sw))
    }
}

fn is_hardware(factory: &gst::ElementFactory) -> bool {
    factory
        .metadata(gst::ELEMENT_METADATA_KLASS)
        .is_some_and(|k| k.contains("Hardware"))
}

fn video_decoders() -> Vec<gst::ElementFactory> {
    gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::DECODER | gst::ElementFactoryType::MEDIA_VIDEO,
        gst::Rank::NONE,
    )
    .into_iter()
    .collect()
}

/// Inspect the registry for usable decoders. Factories with rank NONE are ignored
/// because decodebin3 would never pick them.
pub fn probe() -> DecoderSupport {
    let mut support = DecoderSupport::default();
    let mut factories: Vec<gst::ElementFactory> = gst::ElementFactory::factories_with_type(
        gst::ElementFactoryType::DECODER,
        gst::Rank::MARGINAL,
    )
    .into_iter()
    .collect();
    factories.sort_by_key(|f| std::cmp::Reverse(f.rank()));
    for f in factories {
        // Alpha-channel wrappers only handle `codec-alpha=true` streams.
        if f.name().contains("alpha") {
            continue;
        }
        let hw = is_hardware(&f);
        for tmpl in f.static_pad_templates() {
            if tmpl.direction() != gst::PadDirection::Sink {
                continue;
            }
            let caps = tmpl.caps();
            for s in caps.iter() {
                if let Some(codec) = Codec::from_caps_name(s.name()) {
                    let list = if hw { &mut support.hw } else { &mut support.sw };
                    let entry = list.entry(codec).or_default();
                    if !entry.contains(&f.name().to_string()) {
                        entry.push(f.name().to_string());
                    }
                } else if s.name() == "audio/x-opus" {
                    support.opus = true;
                } else if s.name() == "audio/mpeg"
                    && s.get::<i32>("mpegversion").map_or(true, |v| v == 4 || v == 2)
                    && !f.name().contains("mp3")
                    && !f.name().contains("mpg123")
                {
                    support.aac = true;
                }
            }
        }
    }
    support
}

static ORIGINAL_RANKS: OnceLock<Mutex<HashMap<String, gst::Rank>>> = OnceLock::new();

/// Prefer (or disable) hardware video decoders for autoplugging.
pub fn set_hardware_decoding(enabled: bool) {
    let ranks = ORIGINAL_RANKS.get_or_init(Default::default);
    let mut ranks = ranks.lock().unwrap();
    for f in video_decoders() {
        if !is_hardware(&f) {
            continue;
        }
        let name = f.name().to_string();
        let original = *ranks.entry(name.clone()).or_insert_with(|| f.rank());
        let rank = if enabled {
            // Make sure hardware beats software decoders such as avdec_* (PRIMARY).
            if original > gst::Rank::NONE {
                std::cmp::max(original, gst::Rank::PRIMARY + 1)
            } else {
                gst::Rank::NONE
            }
        } else {
            gst::Rank::NONE
        };
        if f.rank() != rank {
            log::debug!("decoder {name}: rank {:?} -> {:?}", f.rank(), rank);
            f.set_rank(rank);
        }
    }
}
