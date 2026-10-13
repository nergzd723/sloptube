//! Application identity and well-known paths.

use std::path::PathBuf;

use common::paths;
pub use common::paths::write_atomic;

pub const APP_ID: &str = "io.github.nergzd.SlopTubeMusic";
pub const APP_NAME: &str = "SlopTube Music";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const RESOURCE_BASE: &str = "/io/github/nergzd/SlopTubeMusic";

pub fn config_dir() -> PathBuf {
    paths::config_dir("sloptube-music")
}

pub fn data_dir() -> PathBuf {
    paths::data_dir("sloptube-music")
}
