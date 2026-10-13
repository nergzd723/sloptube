//! Music inside SlopTube's application: setting up on first use, the window,
//! links, the app-wide dialogs, and the hooks SlopTube uses to keep the two
//! players from playing at once.
//!
//! Nothing here runs until Music is first opened: launching SlopTube alone
//! doesn't read Music's session, restore its queue or register its MPRIS
//! player.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::account::Account;
use crate::api;
use crate::config;
use crate::library::Library;
use crate::playback::Playback;
use crate::player::PlayState;
use crate::runtime;
use crate::settings;
use crate::ui::window::Window;
use crate::ui::{self, Nav};

pub use crate::config::{APP_ID, APP_NAME, RESOURCE_BASE};
pub use crate::ui::window::WINDOW_CLASS;

thread_local! {
    static STARTED: Cell<bool> = const { Cell::new(false) };
    static ON_PLAYING: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Register Music's resources (stylesheet, icons) under [`RESOURCE_BASE`].
/// For home, explore, library and likes Music uses SlopTube's icons
/// (`sloptube-*-symbolic`), which the host registers too.
pub fn register_resources() {
    gio::resources_register_include!("sloptube-music.gresource").expect("failed to register Music's resources");
}

/// Set Music up the first time it's opened.
fn start(app: &adw::Application) {
    if STARTED.replace(true) {
        return;
    }
    let s = settings::get();
    api::client().set_region(s.region.clone());
    runtime::spawn(async {
        if let Err(e) = api::client().refresh_client_version().await {
            log::debug!("could not refresh the client version: {e:#}");
        }
    });
    Account::get().load_stored();
    crate::image::warm_up_connections();
    let pb = Playback::get();
    // Restored paused: nothing is fetched and yt-dlp isn't started until play is pressed.
    pb.restore_session();
    pb.player().connect_state_notify(|p| {
        if p.state() == PlayState::Playing {
            ON_PLAYING.with(|f| {
                if let Some(f) = &*f.borrow() {
                    f();
                }
            });
        }
    });

    let raise = {
        let app = app.downgrade();
        move || {
            if let Some(app) = app.upgrade() {
                present(&app);
            }
        }
    };
    let quit = {
        let app = app.downgrade();
        move || {
            if let Some(app) = app.upgrade() {
                close(&app);
            }
        }
    };
    glib::spawn_future_local(async move {
        if let Err(e) = crate::mpris::setup(Playback::get(), raise, quit).await {
            log::warn!("MPRIS unavailable for Music: {e:#}");
        }
    });
}

/// The Music window (created, and Music set up, the first time).
pub fn window(app: &adw::Application) -> Window {
    start(app);
    app.windows()
        .into_iter()
        .find_map(|w| w.downcast::<Window>().ok())
        .unwrap_or_else(|| Window::new(app))
}

/// Show the Music window.
pub fn present(app: &adw::Application) {
    window(app).present();
}

/// Open links in Music: music.youtube.com, youtube.com and youtu.be links,
/// or bare video ids.
pub fn open(app: &adw::Application, files: &[gio::File]) {
    let win = window(app);
    win.present();
    for f in files {
        let uri = f.uri().to_string();
        match ui::nav_for_url(&uri) {
            Some(nav) => win.navigate(nav),
            None => {
                let arg = f
                    .basename()
                    .map(|b| b.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if arg.len() == 11 && arg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                    win.navigate(Nav::Play(api::WatchTarget {
                        video_id: Some(arg),
                        ..Default::default()
                    }));
                } else {
                    win.toast(&format!("Not a YouTube Music link: {uri}"));
                }
            }
        }
    }
}

/// Whether `window` is Music's.
pub fn owns(window: &gtk::Window) -> bool {
    window.is::<Window>()
}

/// Pause the music and close its window; the rest of SlopTube keeps running
/// (MPRIS "Quit").
pub fn close(app: &adw::Application) {
    if STARTED.get() {
        let pb = Playback::get();
        pb.player().pause();
        pb.save_session();
        Library::get().save_now();
    }
    for w in app.windows() {
        if w.is::<Window>() {
            // Not close(): with a dialog open, that only closes the dialog.
            w.destroy();
        }
    }
}

/// Pause Music, if it's playing.
pub fn pause() {
    if STARTED.get() {
        let player = Playback::get().player();
        if player.is_playing_intent() {
            player.pause();
        }
    }
}

/// Call `f` whenever Music starts playing.
pub fn connect_started_playing(f: impl Fn() + 'static) {
    ON_PLAYING.with(|cb| cb.replace(Some(Box::new(f))));
}

/// Before the application exits: save the queue and the library, and bring
/// the player down.
pub fn shutdown() {
    if !STARTED.get() {
        return;
    }
    let pb = Playback::get();
    pb.save_session();
    Library::get().save_now();
    pb.player().shutdown(std::time::Duration::from_millis(800));
}

/// Music's preferences, over `parent`.
pub fn show_preferences(parent: &impl IsA<gtk::Widget>) {
    ui::preferences::present(parent);
}

/// Music's About dialog, over `parent`.
pub fn show_about(parent: &impl IsA<gtk::Widget>) {
    let about = adw::AboutDialog::builder()
        .application_name(config::APP_NAME)
        .application_icon(config::APP_ID)
        .version(config::VERSION)
        .developer_name("SlopTube contributors")
        .license_type(gtk::License::Gpl30)
        .comments("A lightweight native YouTube Music client.\n\nNot affiliated with YouTube or Google.")
        .debug_info(format!(
            "GTK {}.{}.{}\nlibadwaita {}.{}.{}\nGStreamer {}\nyt-dlp: {}\n",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
            adw::major_version(),
            adw::minor_version(),
            adw::micro_version(),
            gst::version_string(),
            crate::extractor::ytdlp_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not found".into()),
        ))
        .build();
    about.add_credit_section(
        Some("Built on"),
        &["GTK & libadwaita", "GStreamer", "yt-dlp", "Material Symbols (icons, Apache-2.0)"],
    );
    about.present(Some(parent));
}

/// Music's keyboard shortcuts, over `parent`.
pub fn show_shortcuts(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(440)
        .content_height(620)
        .build();
    let page = adw::PreferencesPage::new();
    let groups: [(&str, &[(&str, &str)]); 2] = [
        (
            "General",
            &[
                ("Ctrl+F, /", "Search"),
                ("Ctrl+L", "Open a link"),
                ("Ctrl+J", "Show or hide Now playing"),
                ("Ctrl+R, F5", "Refresh"),
                ("Ctrl+,", "Preferences"),
                ("Ctrl+W", "Close the window"),
                ("Ctrl+Q", "Quit"),
            ],
        ),
        (
            "Playback",
            &[
                ("Space, K", "Play or pause"),
                ("Shift+N, Ctrl+→", "Next song"),
                ("Shift+P, Ctrl+←", "Previous song"),
                ("J / L", "Back / forward 10 seconds"),
                ("+ / −", "Volume"),
                ("M", "Mute"),
                ("S", "Shuffle"),
                ("R", "Repeat"),
                ("Esc", "Close Now playing"),
            ],
        ),
    ];
    for (title, rows) in groups {
        let g = adw::PreferencesGroup::builder().title(title).build();
        for (keys, what) in rows {
            let row = adw::ActionRow::builder().title(*what).build();
            let l = gtk::Label::new(Some(keys));
            l.add_css_class("dimmed");
            row.add_suffix(&l);
            g.add(&row);
        }
        page.add(&g);
    }
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&page));
    dialog.set_child(Some(&tv));
    dialog.present(Some(parent));
}
