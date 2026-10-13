mod api;
mod application;
mod config;
mod css;
mod diagnose;
mod downloads;
mod extractor;
mod image;
mod mpris;
mod playback;
mod player;
mod selftest;
mod settings;
mod ui;

use common::{account, auth, runtime};
use gtk::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    // Music's Google sign-in window runs as its own process.
    #[cfg(all(feature = "webkit", target_os = "linux"))]
    if std::env::args().nth(1).as_deref() == Some("--music-login") {
        return sloptube_music::login_helper::main();
    }
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("sloptube=info,sloptube_music=info,sloptube_common=info"),
    )
    .init();

    // libadwaita skips its stylesheet entirely when GTK_THEME is set. A global
    // GTK_THEME=Adwaita[:dark] (common in tiling-WM setups to darken GTK3 apps)
    // would leave this app half-styled, so drop it and keep only the dark wish.
    if let Ok(theme) = std::env::var("GTK_THEME") {
        if theme == "Adwaita" || theme.starts_with("Adwaita:") {
            config::GTK_THEME_DARK.store(theme.ends_with(":dark"), std::sync::atomic::Ordering::Relaxed);
            // SAFETY: no other threads have been started yet.
            unsafe { std::env::remove_var("GTK_THEME") };
        }
    }
    if let Err(e) = gst::init() {
        eprintln!("GStreamer failed to initialise: {e}");
        return glib::ExitCode::FAILURE;
    }
    #[cfg(feature = "static-sink")]
    if let Err(e) = gstgtk4::plugin_register_static() {
        log::warn!("could not register the built-in gtk4paintablesink: {e}");
    }
    gio::resources_register_include!("sloptube.gresource").expect("failed to register resources");
    sloptube_music::register_resources();

    if std::env::args().any(|a| a == "--selftest") {
        if std::env::args().any(|a| a == "--music") {
            return sloptube_music::selftest();
        }
        let support = player::decoders::probe();
        println!("{}", support.summary());
        return match runtime::runtime().block_on(selftest::run()) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("selftest failed: {e:#}");
                glib::ExitCode::FAILURE
            }
        };
    }

    // Our own flags; everything else goes to GApplication (--music, URLs to open, --help, ...).
    let mut args: Vec<String> = Vec::new();
    for a in std::env::args() {
        if a == "--diagnose" {
            diagnose::request(diagnose::DEFAULT_VIDEO.to_owned());
        } else if let Some(id) = a.strip_prefix("--diagnose=") {
            diagnose::request(id.to_owned());
        } else {
            args.push(a);
        }
    }
    let app = application::Application::new();
    if diagnose::requested() {
        // A separate instance so it never talks to an already-running SlopTube.
        app.set_flags(app.flags() | gio::ApplicationFlags::NON_UNIQUE);
    }
    // Quit cleanly on SIGTERM/SIGINT (logout, Ctrl+C) so Music's queue and library are saved.
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
        // A second one while quitting doesn't wait.
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        std::process::exit(1);
    });
    app.run_with_args(&args)
}
