//! The GtkApplication: startup, actions, CSS, window management.
//!
//! SlopTube Music runs in the same application: `sloptube --music`, the
//! "Open SlopTube Music" menu item and music.youtube.com links open its
//! window. App-wide actions (preferences, about, shortcuts) act for the app
//! whose window is active.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use crate::account::Account;
use crate::api;
use crate::config;
use crate::player::decoders;
use crate::runtime;
use crate::settings;
use crate::ui::window::Window;
use crate::ui::{self, Nav};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Application {
        pub diagnose_started: std::cell::Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Application {
        const NAME: &'static str = "SlopApplication";
        type Type = super::Application;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for Application {}

    impl ApplicationImpl for Application {
        fn startup(&self) {
            self.parent_startup();
            let app = self.obj();
            glib::set_application_name(config::APP_NAME);
            gtk::Window::set_default_icon_name(config::APP_ID);

            let display = gdk::Display::default().expect("no display");
            let icons = gtk::IconTheme::for_display(&display);
            icons.add_resource_path("/io/github/nergzd/SlopTube/icons");
            icons.add_resource_path(&format!("{}/icons", sloptube_music::RESOURCE_BASE));
            add_window_stylesheet(&display, "/io/github/nergzd/SlopTube/style.css", "sloptube");
            add_window_stylesheet(
                &display,
                &format!("{}/style.css", sloptube_music::RESOURCE_BASE),
                sloptube_music::WINDOW_CLASS,
            );

            let s = settings::get();
            ui::preferences::apply_color_scheme(s.color_scheme);
            api::client().set_region(s.region.clone());
            decoders::set_hardware_decoding(s.hw_decode);
            app.setup_actions();
            Account::get().load_stored();
            runtime::spawn(async {
                if let Err(e) = api::client().refresh_client_version().await {
                    log::debug!("could not refresh client version: {e:#}");
                }
            });
        }

        fn activate(&self) {
            self.parent_activate();
            let win = self.obj().main_window();
            win.present();
            if crate::diagnose::requested() && !self.diagnose_started.replace(true) {
                crate::diagnose::start(&win);
            }
        }

        fn shutdown(&self) {
            // Bring pipelines down cleanly (while still servicing the main loop).
            for w in self.obj().windows() {
                if let Ok(win) = w.downcast::<Window>() {
                    win.playback().player().shutdown(std::time::Duration::from_millis(1500));
                }
            }
            sloptube_music::shutdown();
            self.parent_shutdown();
        }

        fn command_line(&self, cmd: &gio::ApplicationCommandLine) -> glib::ExitCode {
            let app = self.obj();
            let music = cmd.options_dict().contains("music");
            let files: Vec<gio::File> = cmd
                .arguments()
                .iter()
                .skip(1)
                .filter(|a| !a.to_string_lossy().starts_with("--"))
                .map(|a| cmd.create_file_for_arg(a))
                .collect();
            match (files.is_empty(), music) {
                (true, true) => sloptube_music::present(app.upcast_ref()),
                (true, false) => app.activate(),
                (false, _) => self.open(&files, if music { "music" } else { "" }),
            }
            glib::ExitCode::SUCCESS
        }

        fn open(&self, files: &[gio::File], hint: &str) {
            let app = self.obj();
            // `--music` opens every link in Music, otherwise only music.youtube.com links.
            let (music, videos): (Vec<gio::File>, Vec<gio::File>) =
                files.iter().cloned().partition(|f| hint == "music" || is_music_link(&f.uri()));
            if !music.is_empty() {
                sloptube_music::open(app.upcast_ref(), &music);
            }
            if videos.is_empty() {
                return;
            }
            let win = app.main_window();
            win.present();
            for f in &videos {
                let uri = f.uri().to_string();
                match ui::nav_for_youtube_url(&uri) {
                    Some(nav) => win.navigate(nav),
                    None => {
                        // Bare video ids are handy on the command line.
                        let arg = f
                            .basename()
                            .map(|b| b.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if arg.len() == 11 && arg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                            win.navigate(Nav::video_id(&arg));
                        } else {
                            win.toast(&format!("Not a YouTube link: {uri}"));
                        }
                    }
                }
            }
        }
    }

    impl GtkApplicationImpl for Application {}
    impl AdwApplicationImpl for Application {}
}

glib::wrapper! {
    pub struct Application(ObjectSubclass<imp::Application>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Application {
    pub fn new() -> Self {
        let app: Self = glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property(
                "flags",
                gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::HANDLES_COMMAND_LINE,
            )
            .property("resource-base-path", "/io/github/nergzd/SlopTube")
            .build();
        app.add_main_option(
            "music",
            glib::Char::from(b'm'),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            "Open SlopTube Music (links too)",
            None,
        );
        app
    }

    pub fn main_window(&self) -> Window {
        if let Some(w) = self.windows().into_iter().find_map(|w| w.downcast::<Window>().ok()) {
            return w;
        }
        let win = Window::new(self.upcast_ref());
        // Each SlopTube window registers its own MPRIS player, which goes away
        // with the window (Music can keep the application running after that).
        let pb = win.playback();
        let raise = {
            let app = self.downgrade();
            move || {
                if let Some(app) = app.upgrade() {
                    app.main_window().present();
                }
            }
        };
        // Music may still be open, so "Quit" only closes SlopTube's windows.
        let quit = {
            let app = self.downgrade();
            move || {
                if let Some(app) = app.upgrade() {
                    app.close_videos();
                }
            }
        };
        glib::spawn_future_local(async move {
            if let Err(e) = crate::mpris::setup(pb, raise, quit).await {
                log::warn!("MPRIS unavailable: {e:#}");
            }
        });
        win
    }

    /// The active window, if it's Music's.
    fn music_window(&self) -> Option<gtk::Window> {
        self.active_window().filter(sloptube_music::owns)
    }

    /// Stop the videos and close SlopTube's windows; Music keeps running.
    fn close_videos(&self) {
        for w in self.windows() {
            if let Ok(win) = w.downcast::<Window>() {
                win.playback().stop();
                // Not close(): with a dialog open, that only closes the dialog.
                win.destroy();
            }
        }
    }

    fn setup_actions(&self) {
        let action = |name: &str, f: fn(&Application)| {
            let a = gio::SimpleAction::new(name, None);
            let app = self.downgrade();
            a.connect_activate(move |_, _| {
                if let Some(app) = app.upgrade() {
                    f(&app);
                }
            });
            self.add_action(&a);
        };
        action("quit", |app| {
            for w in app.windows() {
                if let Ok(win) = w.downcast::<Window>() {
                    win.playback().stop();
                }
            }
            sloptube_music::pause();
            app.quit();
        });
        action("preferences", |app| match app.music_window() {
            Some(w) => sloptube_music::show_preferences(&w),
            None => ui::preferences::present(&app.main_window()),
        });
        action("about", |app| match app.music_window() {
            Some(w) => sloptube_music::show_about(&w),
            None => app.show_about(),
        });
        action("shortcuts", |app| match app.music_window() {
            Some(w) => sloptube_music::show_shortcuts(&w),
            None => show_shortcuts(&app.main_window()),
        });
        action("show-music", |app| sloptube_music::present(app.upcast_ref()));
        action("show-videos", |app| app.main_window().present());

        self.set_accels_for_action("app.quit", &["<Control>q"]);
        self.set_accels_for_action("app.preferences", &["<Control>comma"]);
        self.set_accels_for_action("app.shortcuts", &["<Control>question"]);
        self.set_accels_for_action("win.search", &["<Control>f", "<Control>k"]);
        self.set_accels_for_action("win.refresh", &["<Control>r", "F5"]);
        self.set_accels_for_action("win.open-link", &["<Control>l"]);
        self.set_accels_for_action("win.toggle-fullscreen", &["F11"]);
        // Music's window
        self.set_accels_for_action("win.toggle-player", &["<Control>j"]);
        self.set_accels_for_action("window.close", &["<Control>w"]);
    }

    fn show_about(&self) {
        let support = decoders::probe();
        let about = adw::AboutDialog::builder()
            .application_name(config::APP_NAME)
            .application_icon(config::APP_ID)
            .version(config::VERSION)
            .developer_name("SlopTube contributors")
            .license_type(gtk::License::Gpl30)
            .comments("A lightweight native YouTube client with hardware-accelerated playback.\n\nNot affiliated with YouTube or Google.")
            .debug_info(format!(
                "GTK {}.{}.{}\nlibadwaita {}.{}.{}\nGStreamer {}\nyt-dlp: {}\nDecoders: {}\n",
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
                support.summary(),
            ))
            .build();
        about.add_credit_section(
            Some("Built on"),
            &["GTK & libadwaita", "GStreamer", "yt-dlp", "Material Symbols (icons, Apache-2.0)"],
        );
        about.present(Some(&self.main_window()));
    }
}

impl Default for Application {
    fn default() -> Self {
        Self::new()
    }
}

/// Load a stylesheet from the resources for the windows with the CSS class `scope` only.
/// A music.youtube.com link (opened in Music even without `--music`).
fn is_music_link(uri: &str) -> bool {
    url::Url::parse(uri).is_ok_and(|u| u.host_str() == Some("music.youtube.com"))
}

fn add_window_stylesheet(display: &gdk::Display, resource: &str, scope: &str) {
    let css = match gio::resources_lookup_data(resource, gio::ResourceLookupFlags::NONE) {
        Ok(data) => crate::css::scoped(&String::from_utf8_lossy(&data), scope),
        Err(e) => {
            log::warn!("no stylesheet at {resource}: {e}");
            return;
        }
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    gtk::style_context_add_provider_for_display(display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

fn show_shortcuts(parent: &Window) {
    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(460)
        .content_height(600)
        .build();
    let page = adw::PreferencesPage::new();
    let groups: [(&str, &[(&str, &str)]); 3] = [
        (
            "General",
            &[
                ("Ctrl+F / Ctrl+K", "Search"),
                ("Ctrl+L", "Open a YouTube link"),
                ("Ctrl+R / F5", "Refresh feed"),
                ("Ctrl+,", "Preferences"),
                ("Ctrl+Q", "Quit"),
            ],
        ),
        (
            "Player",
            &[
                ("Space / K", "Play / pause"),
                ("J / L", "Back / forward 10 seconds"),
                ("← / →", "Back / forward 5 seconds"),
                ("↑ / ↓", "Volume"),
                ("M", "Mute"),
                ("F / F11", "Fullscreen"),
                ("0 … 9", "Jump to 0–90%"),
                ("Shift+N / Shift+P", "Next / previous video"),
                ("I", "Stats for nerds"),
                ("Esc", "Exit fullscreen / minimise player"),
            ],
        ),
        (
            "Touch",
            &[
                ("Double-tap left/right", "Seek ∓10 seconds"),
                ("Swipe down on video", "Minimise player"),
                ("Swipe up on video", "Fullscreen"),
            ],
        ),
    ];
    for (title, rows) in groups {
        let g = adw::PreferencesGroup::builder().title(title).build();
        for (keys, what) in rows {
            let row = adw::ActionRow::builder().title(*what).build();
            let l = gtk::Label::new(Some(keys));
            l.add_css_class("dim-label");
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
