//! YouTube Music InnerTube API: models, parsers and the HTTP client.

pub mod client;
pub mod model;
pub mod parse;

pub use client::client;
pub use model::*;
