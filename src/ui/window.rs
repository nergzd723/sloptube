//! Main window: adaptive navigation (sidebar ↔ bottom bar), tabs, the watch sheet and mini player.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use common::sheet::{self, Hero, PlayerSheet};
use common::shell::{self, Shell};
use gtk::{gio, glib};

use super::card::CardStyle;
use super::channel::ChannelPage;
use super::feed::FeedView;
use super::mini_player::MiniPlayer;
use super::playlist::PlaylistPage;
use super::search::SearchPage;
use super::watch::WatchView;
use super::{Nav, pages};
use crate::account::Account;
use crate::api::VideoItem;
use crate::config;
use crate::downloads::Downloads;
use crate::image;
use crate::playback::Playback;
use crate::player::PlayState;
use crate::runtime;
use crate::settings;

const TABS: [(&str, &str, &str); 4] = [
    ("home", "Home", "sloptube-home-symbolic"),
    ("explore", "Explore", "sloptube-explore-symbolic"),
    ("subscriptions", "Subscriptions", "sloptube-subscriptions-symbolic"),
    ("library", "Library", "sloptube-library-symbolic"),
];

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Window {
        pub playback: Playback,
        pub shell: OnceCell<Shell>,
        pub sidebar_subs: gtk::ListBox,
        pub sidebar_subs_title: gtk::Label,
        pub mini_revealer: gtk::Revealer,
        pub sheet: RefCell<Option<PlayerSheet>>,
        pub watch: RefCell<Option<WatchView>>,
        pub expanded: Cell<bool>,
        pub root_feeds: RefCell<Vec<(String, FeedView)>>,
        pub inhibit: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "SlopWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;
    }

    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.build();
            obj.setup_actions();
            obj.bind_playback();
        }
    }

    impl WidgetImpl for Window {}

    impl WindowImpl for Window {
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            let pb = &self.playback;
            let playing = pb.active() && pb.player().is_playing_intent();
            if playing && settings::get().background_playback {
                // Keep the audio going; MPRIS / relaunching brings the window back.
                obj.collapse_player();
                obj.set_visible(false);
                if let Some(app) = obj.application() {
                    let n = gio::Notification::new("SlopTube is still playing");
                    n.set_body(Some("Use your media keys or open SlopTube again to get back."));
                    app.send_notification(Some("background"), &n);
                }
                return glib::Propagation::Stop;
            }
            pb.stop();
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
        // SLOPTUBE_WINDOW_SIZE=390x844 is handy for trying phone/tablet layouts.
        let (w, h) = shell::window_size("SLOPTUBE_WINDOW_SIZE", (1280, 820));
        glib::Object::builder()
            .property("application", app)
            .property("title", config::APP_NAME)
            .property("default-width", w)
            .property("default-height", h)
            .property("width-request", 360)
            .property("height-request", 294)
            .build()
    }

    pub fn playback(&self) -> Playback {
        self.imp().playback.clone()
    }

    fn shell(&self) -> &Shell {
        self.imp().shell.get().expect("window built")
    }

    /// Allocated heights along the main column (diagnostics).
    pub fn debug_layout(&self) -> String {
        let imp = self.imp();
        let mut out = format!(
            "window {}x{} | stack {} | mini_revealer {} (reveal {}) | switcher {}",
            self.width(),
            self.height(),
            self.shell().stack().height(),
            imp.mini_revealer.height(),
            imp.mini_revealer.reveals_child(),
            self.shell().switcher_bar().height()
        );
        if let Some(mini) = imp.mini_revealer.child() {
            let (min, nat, _, _) = mini.measure(gtk::Orientation::Vertical, mini.width());
            out.push_str(&format!(" | mini {} (measure min {min} nat {nat})", mini.height()));
            let mut c = mini.first_child();
            while let Some(w) = c {
                let (min, nat, _, _) = w.measure(gtk::Orientation::Vertical, w.width());
                out.push_str(&format!(" | {} {} (min {min} nat {nat})", w.type_().name(), w.height()));
                let mut g = w.first_child();
                while let Some(x) = g {
                    let (min, nat, _, _) = x.measure(gtk::Orientation::Vertical, x.width());
                    let (wmin, wnat, _, _) = x.measure(gtk::Orientation::Horizontal, -1);
                    out.push_str(&format!(" [{} {}x{} hmin {min} hnat {nat} wmin {wmin} wnat {wnat} hexp {} vexp {}]", x.type_().name(), x.width(), x.height(), x.compute_expand(gtk::Orientation::Horizontal), x.compute_expand(gtk::Orientation::Vertical)));
                    g = x.next_sibling();
                }
                c = w.next_sibling();
            }
        }
        out
    }

    pub fn sheet(&self) -> Option<PlayerSheet> {
        self.imp().sheet.borrow().clone()
    }

    pub fn watch_view(&self) -> Option<WatchView> {
        self.imp().watch.borrow().clone()
    }

    pub fn toast(&self, message: &str) {
        self.shell().toast(message);
    }

    pub fn register_narrow_only(&self, w: &impl IsA<gtk::Widget>) {
        self.shell().register_narrow_only(w);
    }

    pub fn register_wide_only(&self, w: &impl IsA<gtk::Widget>) {
        self.shell().register_wide_only(w);
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("sloptube");
        let logo = shell::logo(config::APP_ID, config::APP_NAME, 24);
        let shell = Shell::new(self.upcast_ref(), &logo, (200.0, 240.0), 860);
        imp.shell.set(shell.clone()).ok();

        // Tabs
        for (name, title, icon) in TABS {
            let root = match name {
                "home" => {
                    let (page, feed) = pages::home_page(self);
                    imp.root_feeds.borrow_mut().push((name.into(), feed));
                    page
                }
                "explore" => super::explore::explore_page(self),
                "subscriptions" => {
                    let (page, feed) = pages::subscriptions_page(self);
                    imp.root_feeds.borrow_mut().push((name.into(), feed));
                    page
                }
                _ => pages::library_page(self),
            };
            shell.add_tab(name, title, icon, &root);
        }
        shell.connect_tab_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move || win.collapse_player()
        ));

        // Mini player (stays laid out under the sheet while a video is open)
        let mini = MiniPlayer::new(&imp.playback);
        imp.mini_revealer.set_child(Some(&mini));
        imp.mini_revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        imp.mini_revealer.set_reveal_child(false);
        imp.mini_revealer.set_vexpand(false);

        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(shell.stack());
        main_box.append(&imp.mini_revealer);
        main_box.append(shell.switcher_bar());

        // Sidebar: the subscriptions below the tabs.
        imp.sidebar_subs.add_css_class("navigation-sidebar");
        imp.sidebar_subs.set_selection_mode(gtk::SelectionMode::None);
        imp.sidebar_subs.connect_row_activated(|list, row| {
            super::navigate(list, Nav::Channel(row.widget_name().to_string()));
        });
        imp.sidebar_subs_title.set_text("Subscriptions");
        imp.sidebar_subs_title.set_xalign(0.0);
        imp.sidebar_subs_title.add_css_class("heading");
        imp.sidebar_subs_title.add_css_class("sidebar-section");
        shell.sidebar().append(&imp.sidebar_subs_title);
        shell.sidebar().append(&imp.sidebar_subs);
        shell.split().set_content(Some(&main_box));

        // Watch sheet: drag the video down into the mini player and back up.
        let watch = WatchView::new(&imp.playback);
        let sheet = PlayerSheet::new(
            &watch,
            Hero {
                rect: Box::new(glib::clone!(
                    #[weak]
                    watch,
                    #[upgrade_or_default]
                    move || watch.player_frame()?.compute_bounds(&watch)
                )),
                widget: Box::new(glib::clone!(
                    #[weak]
                    watch,
                    #[upgrade_or_default]
                    move || watch.player_frame().map(|f| f.upcast())
                )),
                paintable: Box::new(glib::clone!(
                    #[weak]
                    watch,
                    #[upgrade_or_default]
                    move || watch.player_view().visual_paintable()
                )),
                fit: gtk::ContentFit::Contain,
                css_class: "floating-video",
                aspect: 16.0 / 9.0,
            },
        );
        sheet.set_halign(gtk::Align::Fill);
        sheet.set_valign(gtk::Align::Fill);
        let mini_weak = mini.downgrade();
        sheet.set_mini_rect_provider(move |sheet| {
            let mini = mini_weak.upgrade()?;
            sheet::bounds_in(&mini.thumb()?, sheet)
        });
        sheet.connect_progress_notify(glib::clone!(
            #[weak]
            mini,
            move |s| mini.set_opacity((1.0 - s.progress() / 0.35).clamp(0.0, 1.0))
        ));
        sheet.connect_settled(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, expanded| win.on_sheet_settled(expanded)
        ));
        let pv = watch.player_view();
        sheet.attach_collapse_drag(
            &pv,
            Some(Box::new(glib::clone!(
                #[weak]
                pv,
                move || pv.cancel_pending_click()
            ))),
            Some(Box::new(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move || win.toggle_fullscreen()
            ))),
        );
        if let Some(row) = mini.row() {
            let pb = imp.playback.clone();
            // A sideways swipe either way stops playback.
            sheet.attach_expand_drag(&row, move |_| pb.stop());
        }
        imp.watch.replace(Some(watch.clone()));
        imp.sheet.replace(Some(sheet.clone()));

        let root_overlay = gtk::Overlay::new();
        root_overlay.set_child(Some(shell.split()));
        root_overlay.add_overlay(&sheet);
        shell.toast_overlay().set_child(Some(&root_overlay));
        self.set_content(Some(shell.toast_overlay()));

        self.setup_watch_shortcuts(&watch);
        self.connect_fullscreened_notify(|win| {
            let fs = win.is_fullscreen();
            if let Some(w) = win.imp().watch.borrow().as_ref() {
                w.set_fullscreen(fs);
            }
            if let Some(s) = win.imp().sheet.borrow().as_ref() {
                s.set_locked(fs);
            }
        });
        // Keep keyboard focus in the player while it covers the window.
        shell.keep_focus_in(
            &sheet,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[upgrade_or]
                false,
                move || win.imp().expanded.get()
            ),
            glib::clone!(
                #[weak]
                watch,
                move || watch.player_view().grab_player_focus()
            ),
        );

        // Sidebar subscriptions follow the account.
        let account = Account::get();
        account.connect_signed_in_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.reload_sidebar_subscriptions()
        ));
        self.reload_sidebar_subscriptions();
    }

    fn reload_sidebar_subscriptions(&self) {
        let imp = self.imp();
        imp.sidebar_subs.remove_all();
        let signed_in = Account::get().signed_in();
        imp.sidebar_subs_title.set_visible(signed_in);
        imp.sidebar_subs.set_visible(signed_in);
        if !signed_in {
            return;
        }
        let win = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { crate::api::client().guide_subscriptions().await }).await;
            let (Some(win), Ok(channels)) = (win.upgrade(), res) else { return };
            let scale = win.scale_factor().max(1) as u32;
            for ch in channels {
                let row = gtk::ListBoxRow::new();
                row.set_widget_name(&ch.id);
                let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                let avatar = adw::Avatar::new(24, Some(&ch.name), true);
                image::set_avatar(
                    &avatar,
                    ch.avatar.as_deref().map(|u| image::sized_avatar_url(u, 24 * scale)).as_deref(),
                );
                let l = gtk::Label::new(Some(&ch.name));
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.set_xalign(0.0);
                l.set_hexpand(true);
                b.append(&avatar);
                b.append(&l);
                if ch.has_new_content {
                    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    dot.add_css_class("new-dot");
                    dot.set_valign(gtk::Align::Center);
                    b.append(&dot);
                }
                row.set_child(Some(&b));
                win.imp().sidebar_subs.append(&row);
            }
        });
    }

    fn bind_playback(&self) {
        let pb = self.playback();
        pb.connect_active_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |pb| {
                if !pb.active() {
                    win.collapse_player();
                    win.imp().mini_revealer.set_reveal_child(false);
                } else {
                    win.imp().mini_revealer.set_reveal_child(true);
                }
            }
        ));
        pb.connect_title_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |pb| {
                let t = pb.title();
                win.set_title(Some(&if t.is_empty() || !pb.active() {
                    config::APP_NAME.to_string()
                } else {
                    format!("{t} — {}", config::APP_NAME)
                }));
            }
        ));
        // Keep the screen on while a video plays.
        pb.player().connect_state_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |p| {
                let imp = win.imp();
                let want = p.state() == PlayState::Playing && !win.playback().audio_only();
                let Some(app) = win.application() else { return };
                if want && imp.inhibit.get() == 0 {
                    let cookie = app.inhibit(Some(&win), gtk::ApplicationInhibitFlags::IDLE, Some("Playing a video"));
                    imp.inhibit.set(cookie);
                } else if !want && imp.inhibit.get() != 0 {
                    app.uninhibit(imp.inhibit.get());
                    imp.inhibit.set(0);
                }
            }
        ));
    }

    fn setup_watch_shortcuts(&self, watch: &WatchView) {
        let controller = gtk::ShortcutController::new();
        controller.set_scope(gtk::ShortcutScope::Local);
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
        add("space|k|AudioPlay|AudioPause", |w| w.playback().toggle());
        add("j", |w| w.player_seek(-10.0));
        add("l", |w| w.player_seek(10.0));
        add("Left", |w| w.player_seek(-5.0));
        add("Right", |w| w.player_seek(5.0));
        add("Up", |w| {
            let p = w.playback().player();
            p.set_volume((p.volume() + 0.05).min(1.0));
        });
        add("Down", |w| {
            let p = w.playback().player();
            p.set_volume((p.volume() - 0.05).max(0.0));
        });
        add("m", |w| {
            let p = w.playback().player();
            p.set_muted(!p.muted());
        });
        add("f", |w| w.toggle_fullscreen());
        add("<Shift>n|AudioNext", |w| {
            w.playback().next();
        });
        add("<Shift>p|AudioPrev", |w| {
            w.playback().previous();
        });
        add("i", |w| {
            if let Some(v) = w.imp().watch.borrow().as_ref() {
                v.player_view().toggle_stats();
            }
        });
        add("Home", |w| w.playback().player().seek(0.0, true));
        add("Escape", |w| {
            if w.is_fullscreen() {
                w.unfullscreen();
            } else if !w.imp().watch.borrow().as_ref().is_some_and(|v| v.go_back()) {
                w.collapse_player();
            }
        });
        for n in 0..10 {
            // 0-9 jump to n*10 %
            let win = self.downgrade();
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(win) = win.upgrade() {
                    let p = win.playback().player();
                    if p.duration() > 0.0 {
                        p.seek(p.duration() * n as f64 / 10.0, true);
                    }
                }
                glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(&n.to_string()),
                Some(action),
            ));
        }
        watch.add_controller(controller);
    }

    fn player_seek(&self, delta: f64) {
        if let Some(v) = self.imp().watch.borrow().as_ref() {
            v.player_view().seek_by(delta);
        }
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
        action("sign-in", |w| super::login::present(w));
        action("sign-out", |w| super::login::confirm_sign_out(w));
        action("show-downloads", |w| {
            w.shell().stack().set_visible_child_name("library");
            w.collapse_player();
            let nav = w.current_nav();
            if nav.find_page("downloads").is_none() {
                nav.push(&super::downloads::downloads_page());
            }
        });
        action("show-explore", |w| {
            w.collapse_player();
            w.shell().stack().set_visible_child_name("explore");
        });
        action("show-channels", |w| {
            let (page, _) = pages::feed_page("Channels", pages::browse_loader("FEchannels"), CardStyle::GRID);
            w.push_page(&page);
        });
        action("open-link", |w| w.open_link_dialog());
        action("toggle-fullscreen", |w| w.toggle_fullscreen());
        action("collapse-player", |w| w.collapse_player());
        action("expand-player", |w| w.expand_player());
        action("refresh", |w| w.refresh());
        action("play-pause", |w| w.playback().toggle());
    }

    fn current_nav(&self) -> adw::NavigationView {
        self.shell().current_nav()
    }

    fn push_page(&self, page: &impl IsA<adw::NavigationPage>) {
        self.collapse_player();
        self.current_nav().push(page);
    }

    pub fn navigate(&self, nav: Nav) {
        // Someone browsing is about to watch something: start yt-dlp's worker.
        crate::extractor::extractor().warm_up_soon();
        match nav {
            Nav::Video { item, playlist, start } => {
                let same = self.playback().active() && self.playback().video_id() == item.id;
                if same {
                    if let Some(t) = start {
                        self.playback().seek(t);
                    }
                } else {
                    self.playback().open(item, playlist, start);
                }
                self.expand_player();
            }
            Nav::Channel(id) => {
                let nav = self.current_nav();
                if let Some(top) = nav.visible_page().and_downcast::<ChannelPage>() {
                    if top.channel_id() == id {
                        self.collapse_player();
                        return;
                    }
                }
                self.push_page(&ChannelPage::new(&id));
            }
            Nav::Playlist(id) => self.push_page(&PlaylistPage::new(&id)),
            Nav::Search(q) => {
                self.collapse_player();
                self.open_search(Some(&q));
            }
            Nav::Url(url) => {
                if let Some(nav) = super::nav_for_youtube_url(&url) {
                    self.navigate(nav);
                } else {
                    super::navigate(self, Nav::Url(url));
                }
            }
        }
    }

    pub fn open_search(&self, query: Option<&str>) {
        self.collapse_player();
        let nav = self.current_nav();
        if let Some(page) = nav.visible_page().and_downcast::<SearchPage>() {
            match query {
                Some(q) => page.search(q),
                None => page.focus_entry(),
            }
            return;
        }
        nav.push(&SearchPage::new(query));
    }

    pub fn expand_player(&self) {
        let imp = self.imp();
        if !imp.playback.active() {
            return;
        }
        imp.expanded.set(true);
        imp.mini_revealer.set_reveal_child(true);
        if let Some(sheet) = imp.sheet.borrow().as_ref() {
            sheet.animate_to(1.0, 0.0);
        }
        if let Some(w) = imp.watch.borrow().as_ref() {
            w.player_view().grab_player_focus();
        }
    }

    pub fn collapse_player(&self) {
        let imp = self.imp();
        if self.is_fullscreen() {
            self.unfullscreen();
        }
        let Some(sheet) = imp.sheet.borrow().clone() else { return };
        if sheet.target() <= 0.0 && sheet.progress() <= 0.0 {
            return;
        }
        imp.expanded.set(false);
        sheet.animate_to(0.0, 0.0);
    }

    fn on_sheet_settled(&self, expanded: bool) {
        let imp = self.imp();
        imp.expanded.set(expanded);
        if expanded {
            if let Some(w) = imp.watch.borrow().as_ref() {
                w.player_view().grab_player_focus();
            }
        } else if !imp.playback.active() {
            imp.mini_revealer.set_reveal_child(false);
        }
    }

    pub fn toggle_fullscreen(&self) {
        if self.is_fullscreen() {
            self.unfullscreen();
        } else if self.imp().playback.active() {
            self.expand_player();
            self.fullscreen();
        }
    }

    fn refresh(&self) {
        let imp = self.imp();
        let name = self.shell().visible_tab();
        if self.current_nav().navigation_stack().n_items() > 1 {
            return;
        }
        if let Some((_, feed)) = imp.root_feeds.borrow().iter().find(|(n, _)| *n == name) {
            feed.reload();
        }
    }

    pub fn start_download(&self, item: &VideoItem) {
        Downloads::get().start(item);
        self.toast(&format!("Downloading “{}”", if item.title.is_empty() { &item.id } else { &item.title }));
    }

    fn open_link_dialog(&self) {
        let win = self.downgrade();
        self.shell().open_link_dialog(
            "Open YouTube Link",
            None,
            "https://www.youtube.com/watch?v=…",
            |text| super::nav_for_youtube_url(text).is_some(),
            move |text| {
                let Some(win) = win.upgrade() else { return };
                match super::nav_for_youtube_url(&text) {
                    Some(nav) => win.navigate(nav),
                    None if !text.trim().is_empty() => win.navigate(Nav::Search(text)),
                    None => {}
                }
            },
        );
    }
}
