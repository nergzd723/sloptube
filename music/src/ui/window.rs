//! Main window: adaptive navigation (sidebar ↔ bottom tabs), the player bar
//! and the Now playing sheet.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use common::sheet::{self, Hero, PlayerSheet};
use common::search::Search;
use common::shell::{self, Shell};
use gtk::{gio, glib};

use super::art::ArtView;
use super::artist::artist_page;
use super::browse::BrowseView;
use super::collection::collection_page;
use super::now_playing::NowPlayingView;
use super::player_bar::PlayerBar;
use super::search::search_page;
use super::{LocalList, Nav, page};
use crate::api::{BrowseTarget, CardKind, PageKind};
use crate::config;
use crate::library::Library;
use crate::playback::Playback;
use crate::player::PlayState;
use crate::settings;

/// The CSS class of Music's windows; Music's stylesheet only applies inside them.
pub const WINDOW_CLASS: &str = "stmusic";

const TABS: [(&str, &str, &str); 3] = [
    ("home", "Home", "sloptube-home-symbolic"),
    ("explore", "Explore", "sloptube-explore-symbolic"),
    ("library", "Library", "sloptube-library-symbolic"),
];

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Window)]
    pub struct Window {
        pub shell: OnceCell<Shell>,
        pub root_overlay: gtk::Overlay,
        pub content_overlay: gtk::Overlay,
        pub sidebar_saved: gtk::ListBox,
        pub bar_revealer: gtk::Revealer,
        pub bar: RefCell<Option<PlayerBar>>,
        pub sheet: RefCell<Option<PlayerSheet>>,
        pub now_playing: RefCell<Option<NowPlayingView>>,
        /// Phone layout: bottom tabs, mini player.
        #[property(get)]
        pub narrow: Cell<bool>,
        pub expanded: Cell<bool>,
        pub home: RefCell<Option<BrowseView>>,
        pub explore: RefCell<Option<BrowseView>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "StmWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.build();
            obj.setup_actions();
            obj.setup_shortcuts();
            obj.bind_playback();
        }
    }

    impl WidgetImpl for Window {}

    impl WindowImpl for Window {
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            let pb = Playback::get();
            pb.save_session();
            Library::get().save_now();
            if pb.active() && pb.is_playing() && settings::get().background_playback {
                // Keep the music going; MPRIS or launching the app again brings the window back.
                obj.set_visible(false);
                if let Some(app) = obj.application() {
                    let n = gio::Notification::new("SlopTube Music is still playing");
                    n.set_body(Some("Use your media keys or open SlopTube Music again to get back."));
                    app.send_notification(Some("music-background"), &n);
                }
                return glib::Propagation::Stop;
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub fn new(app: &adw::Application) -> Self {
        // SLOPTUBE_MUSIC_WINDOW_SIZE=390x844 tries the phone layout.
        let (w, h) = shell::window_size("SLOPTUBE_MUSIC_WINDOW_SIZE", (1180, 780));
        let win: Self = glib::Object::builder()
            .property("application", app)
            .property("title", config::APP_NAME)
            .property("icon-name", config::APP_ID)
            .property("default-width", w)
            .property("default-height", h)
            .property("width-request", 360)
            .property("height-request", 480)
            .build();
        // GTK gives every window the application's ID (SlopTube's); with its
        // own, the compositor shows Music's icon and window rules can tell
        // the two apart.
        #[cfg(target_os = "linux")]
        win.connect_realize(|w| {
            if let Some(toplevel) = w.surface().and_downcast::<gdk_wayland::WaylandToplevel>() {
                toplevel.set_application_id(config::APP_ID);
            }
        });
        win
    }

    fn shell(&self) -> &Shell {
        self.imp().shell.get().expect("window built")
    }

    pub fn toast(&self, message: &str) {
        self.shell().toast(message);
    }

    fn register_narrow_only(&self, w: &impl IsA<gtk::Widget>) {
        self.shell().register_narrow_only(w);
    }

    fn root_page(&self, name: &str, title: &str, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
        let chrome = page::page(title, Some(name), content, true);
        if let Some(logo) = &chrome.logo {
            self.register_narrow_only(logo);
        }
        chrome.page
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class(WINDOW_CLASS);
        let shell = Shell::new(self.upcast_ref(), &page::logo(), (220.0, 260.0), 760);
        imp.shell.set(shell.clone()).ok();

        // Tabs, each with its own navigation stack.
        for (name, title, icon) in TABS {
            let root = match name {
                "home" => {
                    let view = BrowseView::new(BrowseTarget::new("FEmusic_home"), true);
                    view.rows().set_empty_state(
                        "sloptube-home-symbolic",
                        "Nothing to show",
                        "YouTube Music didn't recommend anything. Try Explore or Search.",
                    );
                    imp.home.replace(Some(view.clone()));
                    self.root_page(name, title, &view)
                }
                "explore" => {
                    let view = BrowseView::new(BrowseTarget::new("FEmusic_explore"), false);
                    imp.explore.replace(Some(view.clone()));
                    self.root_page(name, title, &view)
                }
                _ => self.root_page(name, title, &super::library::library_view()),
            };
            shell.add_tab(name, title, icon, &root);
        }
        shell.stack().connect_visible_child_name_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.on_tab_changed()
        ));
        shell.connect_tab_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move || win.collapse_player()
        ));

        // Sidebar: the library below the tabs.
        imp.sidebar_saved.add_css_class("navigation-sidebar");
        imp.sidebar_saved.set_selection_mode(gtk::SelectionMode::None);
        imp.sidebar_saved.connect_row_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, row| win.open_sidebar_item(&row.widget_name())
        ));
        let saved_title = gtk::Label::new(Some("Your library"));
        saved_title.set_xalign(0.0);
        saved_title.add_css_class("heading");
        saved_title.add_css_class("sidebar-section");
        shell.sidebar().append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        shell.sidebar().append(&saved_title);
        shell.sidebar().append(&imp.sidebar_saved);
        shell.split().set_content(Some(shell.stack()));
        Library::get().connect_changed(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, what| {
                if what == "saved" {
                    win.reload_sidebar_saved();
                }
            }
        ));
        self.reload_sidebar_saved();

        // Player bar and Now playing.
        let bar = PlayerBar::new();
        imp.bar_revealer.set_child(Some(&bar));
        imp.bar_revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        imp.bar_revealer.set_reveal_child(false);
        let now_playing = NowPlayingView::new();
        let sheet = PlayerSheet::new(
            &now_playing,
            Hero {
                rect: Box::new(glib::clone!(
                    #[weak]
                    now_playing,
                    #[upgrade_or_default]
                    move || now_playing.art_rect_in(&now_playing)
                )),
                widget: Box::new(glib::clone!(
                    #[weak]
                    now_playing,
                    #[upgrade_or_default]
                    move || Some(now_playing.art().upcast())
                )),
                paintable: Box::new(glib::clone!(
                    #[weak]
                    now_playing,
                    #[upgrade_or_default]
                    move || now_playing.art().texture().map(|t| t.upcast())
                )),
                fit: gtk::ContentFit::Cover,
                css_class: "floating-art",
                aspect: 1.0,
            },
        );
        sheet.set_halign(gtk::Align::Fill);
        sheet.set_valign(gtk::Align::Fill);
        let bar_weak = bar.downgrade();
        sheet.set_mini_rect_provider(move |sheet| {
            let bar = bar_weak.upgrade()?;
            let art: ArtView = bar.art();
            sheet::bounds_in(&art, sheet)
        });
        sheet.connect_progress_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |s| {
                if win.imp().narrow.get()
                    && let Some(bar) = win.imp().bar.borrow().as_ref() {
                        bar.set_opacity((1.0 - s.progress() / 0.35).clamp(0.0, 1.0));
                    }
            }
        ));
        sheet.connect_settled(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, open| win.on_sheet_settled(open)
        ));
        // Upward drags are left to the Up next / Lyrics sheet; downward ones close Now playing.
        sheet.attach_collapse_drag(&now_playing.drag_area(), None, None);
        sheet.attach_expand_drag(&bar.mini_row(), |dir| {
            let pb = Playback::get();
            if dir < 0 {
                pb.next();
            } else {
                pb.previous();
            }
        });
        imp.bar.replace(Some(bar));
        imp.now_playing.replace(Some(now_playing));
        imp.sheet.replace(Some(sheet));

        imp.content_overlay.set_child(Some(shell.split()));
        imp.content_overlay.set_vexpand(true);
        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(&imp.content_overlay);
        main_box.append(&imp.bar_revealer);
        main_box.append(shell.switcher_bar());
        imp.root_overlay.set_child(Some(&main_box));
        shell.toast_overlay().set_child(Some(&imp.root_overlay));
        self.set_content(Some(shell.toast_overlay()));

        shell.connect_layout(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |narrow| win.apply_layout(narrow)
        ));
        self.apply_layout(shell.narrow());
        // Keep keyboard focus in Now playing while it covers the window.
        if let (Some(sheet), Some(np)) = (imp.sheet.borrow().clone(), imp.now_playing.borrow().clone()) {
            shell.keep_focus_in(
                &sheet,
                glib::clone!(
                    #[weak(rename_to = win)]
                    self,
                    #[upgrade_or]
                    false,
                    move || win.imp().expanded.get()
                ),
                move || np.grab_play_focus(),
            );
        }

        if let Some(home) = imp.home.borrow().as_ref() {
            home.ensure_loaded();
        }
        // Recommendations depend on who is signed in.
        crate::account::Account::get().connect_signed_in_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                let imp = win.imp();
                if let Some(home) = imp.home.borrow().as_ref() {
                    home.reload();
                }
                if let Some(explore) = imp.explore.borrow().as_ref()
                    && explore.rows().is_loaded() {
                        explore.reload();
                    }
            }
        ));
    }

    fn reload_sidebar_saved(&self) {
        let imp = self.imp();
        super::clear(&imp.sidebar_saved);
        let add = |name: &str, title: &str, subtitle: &str, icon: Option<&str>, thumbs: &[crate::api::Thumbnail], round: bool| {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(name);
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            match icon {
                Some(i) => {
                    let img = gtk::Image::from_icon_name(i);
                    img.set_size_request(36, 36);
                    img.add_css_class("sidebar-icon");
                    b.append(&img);
                }
                None => {
                    let art = ArtView::new(36, 36, 4.0);
                    art.set_round(round);
                    art.set_thumbnails(thumbs);
                    b.append(&art);
                }
            }
            let texts = gtk::Box::new(gtk::Orientation::Vertical, 0);
            texts.set_valign(gtk::Align::Center);
            let t = gtk::Label::new(Some(title));
            t.set_xalign(0.0);
            t.set_ellipsize(gtk::pango::EllipsizeMode::End);
            texts.append(&t);
            if !subtitle.is_empty() {
                let s = gtk::Label::new(Some(subtitle));
                s.set_xalign(0.0);
                s.set_ellipsize(gtk::pango::EllipsizeMode::End);
                s.add_css_class("dimmed");
                s.add_css_class("caption");
                texts.append(&s);
            }
            b.append(&texts);
            row.set_child(Some(&b));
            imp.sidebar_saved.append(&row);
        };
        add("local:liked", "Liked songs", "", Some("sloptube-thumbs-up-symbolic"), &[], false);
        add("local:history", "History", "", Some("document-open-recent-symbolic"), &[], false);
        for c in Library::get().saved().iter().take(40) {
            add(
                &format!("browse:{}", c.target.id),
                &c.title,
                c.kind.label(),
                None,
                &c.thumbnails,
                matches!(c.kind, CardKind::Artist | CardKind::Profile),
            );
        }
    }

    fn open_sidebar_item(&self, name: &str) {
        if name == "local:liked" {
            self.navigate(Nav::Local(LocalList::Liked));
        } else if name == "local:history" {
            self.navigate(Nav::Local(LocalList::History));
        } else if let Some(id) = name.strip_prefix("browse:")
            && let Some(c) = Library::get().saved().into_iter().find(|c| c.target.id == id) {
                self.navigate(Nav::Page(c.target.clone(), c.title.clone()));
            }
    }

    fn on_tab_changed(&self) {
        let imp = self.imp();
        let name = self.shell().visible_tab();
        if name == "explore"
            && let Some(e) = imp.explore.borrow().as_ref() {
                e.ensure_loaded();
            }
    }

    /// The shell switched layouts: adapt the player bar, Now playing and where the sheet goes.
    fn apply_layout(&self, narrow: bool) {
        let imp = self.imp();
        let changed = imp.narrow.replace(narrow) != narrow;
        if let Some(bar) = imp.bar.borrow().as_ref() {
            bar.set_narrow(narrow);
            bar.set_opacity(1.0);
        }
        if let Some(np) = imp.now_playing.borrow().as_ref() {
            np.set_narrow(narrow);
        }
        // The sheet covers the whole window on a phone, only the pages on a desktop.
        if let Some(sheet) = imp.sheet.borrow().as_ref() {
            if let Some(parent) = sheet.parent()
                && let Some(o) = parent.downcast_ref::<gtk::Overlay>() {
                    o.remove_overlay(sheet);
                }
            if narrow {
                imp.root_overlay.add_overlay(sheet);
            } else {
                imp.content_overlay.add_overlay(sheet);
            }
        }
        if changed {
            self.notify_narrow();
        }
    }

    fn bind_playback(&self) {
        let pb = Playback::get();
        pb.connect_active_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |pb| {
                win.imp().bar_revealer.set_reveal_child(pb.active());
                if !pb.active() {
                    win.collapse_player();
                }
            }
        ));
        self.imp().bar_revealer.set_reveal_child(pb.active());
        pb.connect_title_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |pb| {
                let t = pb.title();
                win.set_title(Some(&if t.is_empty() || !pb.active() {
                    config::APP_NAME.to_string()
                } else {
                    format!("{t} – {}", pb.artist())
                }));
            }
        ));
        pb.connect_message(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, msg| win.toast(msg)
        ));
        // Keep the session from suspending while music plays.
        pb.player().connect_state_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |p| {
                let Some(app) = win.application() else { return };
                let want = p.state() == PlayState::Playing;
                // SAFETY: the cookie is only ever stored as a u32.
                let cookie = unsafe { win.data::<u32>("inhibit").map(|c| *c.as_ref()).unwrap_or(0) };
                if want && cookie == 0 {
                    let c = app.inhibit(Some(&win), gtk::ApplicationInhibitFlags::SUSPEND, Some("Playing music"));
                    unsafe { win.set_data("inhibit", c) };
                } else if !want && cookie != 0 {
                    app.uninhibit(cookie);
                    unsafe { win.set_data("inhibit", 0u32) };
                }
            }
        ));
    }

    fn setup_actions(&self) {
        let action = |name: &str, f: fn(&Window)| {
            let a = gio::SimpleAction::new(name, None);
            let win = self.downgrade();
            a.connect_activate(move |_, _| {
                if let Some(w) = win.upgrade() {
                    f(&w);
                }
            });
            self.add_action(&a);
        };
        action("search", |w| w.open_search(None));
        action("open-link", |w| w.open_link_dialog());
        action("collapse-player", |w| w.collapse_player());
        action("expand-player", |w| w.expand_player());
        action("toggle-player", |w| {
            if w.imp().expanded.get() {
                w.collapse_player();
            } else {
                w.expand_player();
            }
        });
        action("refresh", |w| w.refresh());
        action("play-pause", |_| Playback::get().toggle());
        action("next", |_| Playback::get().next());
        action("previous", |_| Playback::get().previous());
        action("show-liked", |w| w.navigate(Nav::Local(LocalList::Liked)));
        action("sign-in", super::login::present);
        action("sign-out", super::login::confirm_sign_out);
        action("show-account", |w| {
            w.collapse_player();
            w.shell().stack().set_visible_child_name("library");
        });
    }

    fn setup_shortcuts(&self) {
        let controller = gtk::ShortcutController::new();
        let add = |trigger: &str, f: fn(&Window)| {
            let win = self.downgrade();
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(win) = win.upgrade() {
                    f(&win);
                }
                glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(trigger), Some(action)));
        };
        add("slash", |w| w.open_search(None));
        add("space|k|AudioPlay|AudioPause", |_| Playback::get().toggle());
        add("<Shift>n|AudioNext|<Control>Right", |_| Playback::get().next());
        add("<Shift>p|AudioPrev|<Control>Left", |_| Playback::get().previous());
        add("l", |_| seek_by(10.0));
        add("j", |_| seek_by(-10.0));
        add("plus|equal|KP_Add", |_| {
            let p = Playback::get().player();
            p.set_volume((p.volume() + 0.05).min(1.0));
        });
        add("minus|KP_Subtract", |_| {
            let p = Playback::get().player();
            p.set_volume((p.volume() - 0.05).max(0.0));
        });
        add("m", |_| {
            let p = Playback::get().player();
            p.set_muted(!p.muted());
        });
        add("s", |_| {
            let pb = Playback::get();
            pb.set_shuffle(!pb.shuffle());
        });
        add("r", |_| super::now_playing::cycle_repeat());
        add("Escape", |w| {
            let imp = w.imp();
            if imp.expanded.get() {
                let closed_tabs = imp.now_playing.borrow().as_ref().is_some_and(|np| np.close_sheet());
                if !closed_tabs {
                    w.collapse_player();
                }
            } else {
                w.current_nav().pop();
            }
        });
        self.add_controller(controller);
    }

    fn current_nav(&self) -> adw::NavigationView {
        self.shell().current_nav()
    }

    fn push_page(&self, page: &impl IsA<adw::NavigationPage>) {
        self.collapse_player();
        self.current_nav().push(page);
    }

    pub fn navigate(&self, nav: Nav) {
        // Someone browsing albums, artists or search results is about to play something.
        crate::extractor::extractor().warm_up_soon();
        match nav {
            Nav::Page(target, title) => {
                let page = match target.kind {
                    PageKind::Album | PageKind::Playlist | PageKind::Podcast => {
                        collection_page(&Nav::Page(target, title))
                    }
                    PageKind::Artist | PageKind::Profile => artist_page(&target, &title),
                    _ => {
                        let view = BrowseView::new(target, false);
                        let chrome = page::page(&title, None, &view, false);
                        let p = chrome.page.clone();
                        view.connect_title(move |t| p.set_title(t));
                        view.ensure_loaded();
                        chrome.page
                    }
                };
                self.push_page(&page);
            }
            Nav::Local(_) => self.push_page(&collection_page(&nav)),
            Nav::Search(q) => self.open_search(Some(&q)),
            Nav::Play(target) => {
                Playback::get().play_watch(target, "");
                self.expand_player();
            }
            Nav::Url(url) => super::navigate(self, Nav::Url(url)),
        }
    }

    pub fn open_search(&self, query: Option<&str>) {
        self.collapse_player();
        let nav = self.current_nav();
        if let Some(search) = nav.visible_page().as_ref().and_then(Search::of_page) {
            match query {
                Some(q) => search.search(q),
                None => search.focus_entry(),
            }
            return;
        }
        nav.push(&search_page(query));
    }

    pub fn expand_player(&self) {
        let imp = self.imp();
        if !Playback::get().active() {
            return;
        }
        imp.expanded.set(true);
        imp.bar_revealer.set_reveal_child(true);
        if let Some(bar) = imp.bar.borrow().as_ref() {
            bar.set_expanded(true);
        }
        if let Some(sheet) = imp.sheet.borrow().as_ref() {
            sheet.animate_to(1.0, 0.0);
        }
    }

    pub fn collapse_player(&self) {
        let imp = self.imp();
        let Some(sheet) = imp.sheet.borrow().clone() else { return };
        if sheet.target() <= 0.0 && sheet.progress() <= 0.0 {
            return;
        }
        imp.expanded.set(false);
        if let Some(bar) = imp.bar.borrow().as_ref() {
            bar.set_expanded(false);
        }
        sheet.animate_to(0.0, 0.0);
    }

    fn on_sheet_settled(&self, open: bool) {
        let imp = self.imp();
        imp.expanded.set(open);
        if let Some(bar) = imp.bar.borrow().as_ref() {
            bar.set_expanded(open);
        }
        if !open
            && let Some(np) = imp.now_playing.borrow().as_ref() {
                np.close_sheet();
            }
    }

    fn refresh(&self) {
        let imp = self.imp();
        if self.current_nav().navigation_stack().n_items() > 1 {
            return;
        }
        match self.shell().visible_tab().as_str() {
            "home" => imp.home.borrow().as_ref().map(BrowseView::reload),
            "explore" => imp.explore.borrow().as_ref().map(BrowseView::reload),
            _ => None,
        };
    }

    fn open_link_dialog(&self) {
        let win = self.downgrade();
        self.shell().open_link_dialog(
            "Open Link",
            Some("A YouTube Music or YouTube link"),
            "https://music.youtube.com/…",
            |text| super::nav_for_url(text).is_some(),
            move |text| {
                let Some(win) = win.upgrade() else { return };
                match super::nav_for_url(&text) {
                    Some(nav) => win.navigate(nav),
                    None if !text.trim().is_empty() => win.navigate(Nav::Search(text)),
                    None => {}
                }
            },
        );
    }
}

fn seek_by(delta: f64) {
    let pb = Playback::get();
    let p = pb.player();
    pb.seek((p.position() + delta).max(0.0));
}
