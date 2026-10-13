//! Video playback: the shared GStreamer player (`common::player`), set up for
//! video, and what only videos need: decoder ranking, stream selection and
//! DASH manifests for MP4 streams.

pub mod dash;
pub mod decoders;
pub mod formats;

use std::time::Duration;

pub use common::player::{LoadRequest, PlayState, Player, PlayerConfig, Source, headers_from_map};

/// Video into a `gtk4paintablesink`; a few seconds buffered without hoarding memory.
pub const CONFIG: PlayerConfig = PlayerConfig {
    name: "sloptube",
    video: true,
    buffer_duration: Duration::from_secs(8),
    buffer_size: 16 * 1024 * 1024,
    audio_sink_var: None,
};
