#![allow(clippy::type_complexity)] // Boxed GTK callbacks.

mod account;
mod api;
mod application;
mod auth;
mod config;
mod extractor;
mod image;
mod library;
mod memory;
mod mpris;
mod playback;
mod player;
mod runtime;
mod selftest;
mod settings;
mod ui;

use gtk::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    memory::init();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("sloptube_music=info")).init();

    // libadwaita skips its stylesheet entirely when GTK_THEME is set; keep only the dark wish.
    if let Ok(theme) = std::env::var("GTK_THEME")
        && (theme == "Adwaita" || theme.starts_with("Adwaita:")) {
            config::GTK_THEME_DARK.store(theme.ends_with(":dark"), std::sync::atomic::Ordering::Relaxed);
            // SAFETY: no other threads have been started yet.
            unsafe { std::env::remove_var("GTK_THEME") };
        }

    if std::env::args().any(|a| a == "--selftest") {
        return match runtime::runtime().block_on(selftest::run()) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("selftest failed: {e:#}");
                glib::ExitCode::FAILURE
            }
        };
    }

    if let Err(e) = gst::init() {
        eprintln!("GStreamer failed to initialise: {e}");
        return glib::ExitCode::FAILURE;
    }
    gio::resources_register_include!("sloptube-music.gresource").expect("failed to register resources");
    let app = application::Application::new();
    // Quit cleanly on SIGTERM/SIGINT (logout, Ctrl+C) so the queue and library are saved.
    runtime::spawn(async {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        glib::MainContext::default().invoke(|| {
            if let Some(app) = gio::Application::default() {
                app.quit();
            }
        });
    });
    app.run()
}
