//! SlopTube Music: a YouTube Music client that runs inside SlopTube.
//!
//! SlopTube's application owns the process and the GApplication; this crate
//! adds the Music window to it. Everything else (the InnerTube client, the
//! audio player, the queue, the library, the account) is Music's own. The
//! public functions here (from `application.rs`) are what SlopTube calls.

#![allow(clippy::type_complexity)] // Boxed GTK callbacks.

mod account;
mod api;
mod application;
mod auth;
mod config;
mod extractor;
mod image;
mod library;
#[cfg(all(feature = "webkit", target_os = "linux"))]
pub mod login_helper;
mod mpris;
mod playback;
mod player;
mod selftest;
mod settings;
mod ui;

use common::{memory, runtime};

pub use application::*;

/// `sloptube --music --selftest`: check every endpoint and yt-dlp without a window.
pub fn selftest() -> gtk::glib::ExitCode {
    match runtime::runtime().block_on(selftest::run()) {
        Ok(()) => gtk::glib::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("selftest failed: {e:#}");
            gtk::glib::ExitCode::FAILURE
        }
    }
}
