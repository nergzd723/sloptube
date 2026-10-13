//! Music's player: the shared GStreamer player (`common::player`), audio only.

use std::time::Duration;

pub use common::player::{LoadRequest, NextStream, PlayState, Player, PlayerConfig, Source};

/// Audio only, with little buffering: ~15 s of Opus is a few hundred KB.
pub const CONFIG: PlayerConfig = PlayerConfig {
    name: "stmusic",
    video: false,
    buffer_duration: Duration::from_secs(15),
    buffer_size: 1024 * 1024,
    audio_sink_var: Some("SLOPTUBE_MUSIC_AUDIO_SINK"),
};
