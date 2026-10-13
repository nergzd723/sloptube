//! The AdwApplication: startup, actions, styling, window management.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use crate::api;
use crate::config;
use crate::library::Library;
use crate::playback::Playback;
use crate::runtime;
use crate::settings;
use crate::ui::window::Window;
use crate::ui::{self, Nav};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Application {
        pub mpris_started: std::cell::Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Application {
        const NAME: &'static str = "StmApplication";
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
            gtk::IconTheme::for_display(&display).add_resource_path(&format!("{}/icons", config::RESOURCE_BASE));
            let css = gtk::CssProvider::new();
            css.load_from_resource(&format!("{}/style.css", config::RESOURCE_BASE));
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

            let s = settings::get();
            ui::preferences::apply_color_scheme(s.color_scheme);
            api::client().set_region(s.region.clone());
            app.setup_actions();
            runtime::spawn(async {
                if let Err(e) = api::client().refresh_client_version().await {
                    log::debug!("could not refresh the client version: {e:#}");
                }
            });
            crate::account::Account::get().load_stored();
            crate::image::warm_up_connections();
            // Restored paused: nothing is fetched and yt-dlp isn't started until play is pressed.
            Playback::get().restore_session();
        }

        fn activate(&self) {
            self.parent_activate();
            self.obj().main_window().present();
        }

        fn shutdown(&self) {
            let pb = Playback::get();
            pb.save_session();
            Library::get().save_now();
            pb.player().shutdown(std::time::Duration::from_millis(800));
            self.parent_shutdown();
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            let win = self.obj().main_window();
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
    }

    impl GtkApplicationImpl for Application {}
    impl AdwApplicationImpl for Application {}
}

glib::wrapper! {
    pub struct Application(ObjectSubclass<imp::Application>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for Application {
    fn default() -> Self {
        Self::new()
    }
}

impl Application {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .property("resource-base-path", config::RESOURCE_BASE)
            .build()
    }

    pub fn main_window(&self) -> Window {
        if let Some(w) = self.windows().into_iter().find_map(|w| w.downcast::<Window>().ok()) {
            return w;
        }
        let win = Window::new(self.upcast_ref());
        if !self.imp().mpris_started.replace(true) {
            let raise = {
                let app = self.downgrade();
                move || {
                    if let Some(app) = app.upgrade() {
                        app.main_window().present();
                    }
                }
            };
            let quit = {
                let app = self.downgrade();
                move || {
                    if let Some(app) = app.upgrade() {
                        app.quit();
                    }
                }
            };
            glib::spawn_future_local(async move {
                if let Err(e) = crate::mpris::setup(Playback::get(), raise, quit).await {
                    log::warn!("MPRIS unavailable: {e:#}");
                }
            });
        }
        win
    }

    fn setup_actions(&self) {
        let add = |name: &str, f: fn(&Application)| {
            let a = gio::SimpleAction::new(name, None);
            let app = self.downgrade();
            a.connect_activate(move |_, _| {
                if let Some(app) = app.upgrade() {
                    f(&app);
                }
            });
            self.add_action(&a);
        };
        add("quit", |app| {
            Playback::get().player().pause();
            app.quit();
        });
        add("preferences", |app| ui::preferences::present(&app.main_window()));
        add("about", |app| app.show_about());
        add("shortcuts", |app| show_shortcuts(&app.main_window()));
        add("sign-in", |app| {
            let win = app.main_window();
            win.present();
            ui::login::present(&win);
        });
        // app.search('query') searches; an empty query opens the search page.
        let search = gio::SimpleAction::new("search", Some(glib::VariantTy::STRING));
        search.connect_activate(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, param| {
                let q = param.and_then(|p| p.str().map(str::to_owned)).unwrap_or_default();
                let win = app.main_window();
                win.present();
                win.open_search((!q.is_empty()).then_some(q.as_str()));
            }
        ));
        self.add_action(&search);

        self.set_accels_for_action("app.quit", &["<Control>q"]);
        self.set_accels_for_action("app.preferences", &["<Control>comma"]);
        self.set_accels_for_action("app.shortcuts", &["<Control>question"]);
        self.set_accels_for_action("win.search", &["<Control>f", "<Control>k", "slash"]);
        self.set_accels_for_action("win.refresh", &["<Control>r", "F5"]);
        self.set_accels_for_action("win.open-link", &["<Control>l"]);
        self.set_accels_for_action("win.toggle-player", &["<Control>j"]);
        self.set_accels_for_action("window.close", &["<Control>w"]);
    }

    fn show_about(&self) {
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
        about.present(Some(&self.main_window()));
    }
}

fn show_shortcuts(parent: &Window) {
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
