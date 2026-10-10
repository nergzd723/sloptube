//! Main window: adaptive navigation (sidebar ↔ bottom tabs), the player bar
//! and the Now playing sheet.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use super::art::ArtView;
use super::artist::artist_page;
use super::browse::BrowseView;
use super::collection::collection_page;
use super::now_playing::NowPlayingView;
use super::player_bar::PlayerBar;
use super::search::{SearchView, search_page};
use super::sheet::{self, PlayerSheet};
use super::{LocalList, Nav, page};
use crate::api::{BrowseTarget, CardKind, PageKind};
use crate::config;
use crate::library::Library;
use crate::playback::Playback;
use crate::player::PlayState;
use crate::settings;

const TABS: [(&str, &str, &str); 3] = [
    ("home", "Home", "stm-home-symbolic"),
    ("explore", "Explore", "stm-explore-symbolic"),
    ("library", "Library", "stm-library-symbolic"),
];

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::Window)]
    pub struct Window {
        pub toast_overlay: adw::ToastOverlay,
        pub root_overlay: gtk::Overlay,
        pub content_overlay: gtk::Overlay,
        pub split: adw::OverlaySplitView,
        pub stack: adw::ViewStack,
        pub switcher_bar: adw::ViewSwitcherBar,
        pub sidebar_nav: gtk::ListBox,
        pub sidebar_saved: gtk::ListBox,
        pub navs: RefCell<Vec<(String, adw::NavigationView)>>,
        pub bar_revealer: gtk::Revealer,
        pub bar: RefCell<Option<PlayerBar>>,
        pub sheet: RefCell<Option<PlayerSheet>>,
        pub now_playing: RefCell<Option<NowPlayingView>>,
        pub narrow_only: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
        /// Phone layout: bottom tabs, mini player.
        #[property(get)]
        pub narrow: Cell<bool>,
        pub expanded: Cell<bool>,
        pub home: RefCell<Option<BrowseView>>,
        pub explore: RefCell<Option<BrowseView>>,
        pub syncing: Cell<bool>,
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
            obj.setup_breakpoints();
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
                    app.send_notification(Some("background"), &n);
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
        let (w, h) = std::env::var("SLOPTUBE_MUSIC_WINDOW_SIZE")
            .ok()
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((1180, 780));
        glib::Object::builder()
            .property("application", app)
            .property("title", config::APP_NAME)
            .property("default-width", w)
            .property("default-height", h)
            .property("width-request", 360)
            .property("height-request", 480)
            .build()
    }

    pub fn toast(&self, message: &str) {
        let t = adw::Toast::new(message);
        t.set_timeout(3);
        self.imp().toast_overlay.add_toast(t);
    }

    fn register_narrow_only(&self, w: &impl IsA<gtk::Widget>) {
        w.set_visible(self.imp().narrow.get());
        self.imp()
            .narrow_only
            .borrow_mut()
            .push(w.upcast_ref::<gtk::Widget>().downgrade());
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
        self.add_css_class("stmusic");

        // Tabs, each with its own navigation stack.
        for (name, title, icon) in TABS {
            let nav = adw::NavigationView::new();
            let root = match name {
                "home" => {
                    let view = BrowseView::new(BrowseTarget::new("FEmusic_home"), true);
                    view.rows().set_empty_state(
                        "stm-home-symbolic",
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
            nav.add(&root);
            let p = imp.stack.add_titled_with_icon(&nav, Some(name), title, icon);
            p.set_use_underline(false);
            imp.navs.borrow_mut().push((name.into(), nav));
        }
        imp.stack.set_vexpand(true);
        imp.stack.connect_visible_child_name_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.on_tab_changed()
        ));
        imp.switcher_bar.set_stack(Some(&imp.stack));

        // Sidebar.
        imp.sidebar_nav.add_css_class("navigation-sidebar");
        for (name, title, icon) in TABS {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(name);
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 14);
            b.append(&gtk::Image::from_icon_name(icon));
            b.append(&gtk::Label::new(Some(title)));
            row.set_child(Some(&b));
            imp.sidebar_nav.append(&row);
        }
        imp.sidebar_nav.connect_row_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, row| {
                let name = row.widget_name();
                let imp = win.imp();
                if imp.stack.visible_child_name().as_deref() == Some(name.as_str()) {
                    win.current_nav().pop_to_tag(&name);
                }
                imp.stack.set_visible_child_name(&name);
                win.collapse_player();
            }
        ));
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
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sidebar_box.append(&imp.sidebar_nav);
        sidebar_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        sidebar_box.append(&saved_title);
        sidebar_box.append(&imp.sidebar_saved);
        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .child(&sidebar_box)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let sidebar_header = page::header_bar();
        sidebar_header.set_show_title(false);
        sidebar_header.set_show_end_title_buttons(false);
        sidebar_header.pack_start(&page::logo());
        let sidebar_tv = adw::ToolbarView::new();
        sidebar_tv.add_top_bar(&sidebar_header);
        sidebar_tv.set_content(Some(&sidebar_scroll));
        sidebar_tv.add_css_class("sidebar");
        imp.split.set_sidebar(Some(&sidebar_tv));
        imp.split.set_content(Some(&imp.stack));
        imp.split.set_min_sidebar_width(220.0);
        imp.split.set_max_sidebar_width(260.0);
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
        let sheet = PlayerSheet::new(&now_playing);
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
        sheet.attach_collapse_drag(&now_playing.drag_area());
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

        imp.content_overlay.set_child(Some(&imp.split));
        imp.content_overlay.set_vexpand(true);
        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(&imp.content_overlay);
        main_box.append(&imp.bar_revealer);
        main_box.append(&imp.switcher_bar);
        imp.root_overlay.set_child(Some(&main_box));
        imp.toast_overlay.set_child(Some(&imp.root_overlay));
        self.set_content(Some(&imp.toast_overlay));

        if let Some(home) = imp.home.borrow().as_ref() {
            home.ensure_loaded();
        }
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
        add("local:liked", "Liked songs", "", Some("stm-thumbs-up-symbolic"), &[], false);
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
        let name = imp.stack.visible_child_name().unwrap_or_default();
        if name == "explore"
            && let Some(e) = imp.explore.borrow().as_ref() {
                e.ensure_loaded();
            }
        imp.syncing.set(true);
        let mut child = imp.sidebar_nav.first_child();
        while let Some(c) = child {
            if let Some(row) = c.downcast_ref::<gtk::ListBoxRow>()
                && row.widget_name() == name {
                    imp.sidebar_nav.select_row(Some(row));
                }
            child = c.next_sibling();
        }
        imp.syncing.set(false);
    }

    fn setup_breakpoints(&self) {
        // Only one breakpoint applies at a time (the last matching one), so both
        // switch to the narrow layout; the phone one also tightens the styling.
        for (condition, phone) in [("max-width: 760sp", false), ("max-width: 520sp", true)] {
            let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse(condition).unwrap());
            bp.connect_apply(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| {
                    if phone {
                        win.add_css_class("phone");
                    } else {
                        win.remove_css_class("phone");
                    }
                    win.set_narrow(true);
                }
            ));
            bp.connect_unapply(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| {
                    if phone {
                        win.remove_css_class("phone");
                    }
                    // Switching between the two breakpoints unapplies one before
                    // applying the other: only go wide if none applies after all.
                    glib::idle_add_local_once(glib::clone!(
                        #[weak]
                        win,
                        move || {
                            if win.current_breakpoint().is_none() {
                                win.set_narrow(false);
                            }
                        }
                    ));
                }
            ));
            self.add_breakpoint(bp);
        }
        self.apply_layout(false);
    }

    fn set_narrow(&self, narrow: bool) {
        if self.imp().narrow.get() != narrow {
            self.apply_layout(narrow);
        }
    }

    fn apply_layout(&self, narrow: bool) {
        log::debug!("layout: narrow={narrow} (window {}x{})", self.width(), self.height());
        let imp = self.imp();
        let changed = imp.narrow.replace(narrow) != narrow;
        imp.split.set_collapsed(narrow);
        imp.split.set_show_sidebar(!narrow);
        imp.switcher_bar.set_reveal(narrow);
        if narrow {
            self.add_css_class("narrow");
        } else {
            self.remove_css_class("narrow");
        }
        imp.narrow_only.borrow_mut().retain(|w| w.upgrade().is_some());
        for w in imp.narrow_only.borrow().iter().filter_map(|w| w.upgrade()) {
            w.set_visible(narrow);
        }
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
        let imp = self.imp();
        let name = imp.stack.visible_child_name().unwrap_or_default();
        imp.navs
            .borrow()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| imp.navs.borrow()[0].1.clone())
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

    fn visible_search(&self) -> Option<SearchView> {
        let page = self.current_nav().visible_page()?;
        if page.tag().as_deref() != Some("search") {
            return None;
        }
        page.child()
            .and_downcast::<adw::ToolbarView>()?
            .content()
            .and_downcast::<SearchView>()
    }

    pub fn open_search(&self, query: Option<&str>) {
        self.collapse_player();
        if let Some(view) = self.visible_search() {
            match query {
                Some(q) => view.search(q),
                None => {
                    view.entry().grab_focus();
                }
            }
            return;
        }
        let (page, view) = search_page();
        self.current_nav().push(&page);
        if let Some(q) = query {
            view.search(q);
        }
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
        match imp.stack.visible_child_name().as_deref() {
            Some("home") => imp.home.borrow().as_ref().map(BrowseView::reload),
            Some("explore") => imp.explore.borrow().as_ref().map(BrowseView::reload),
            _ => None,
        };
    }

    fn open_link_dialog(&self) {
        let dialog = adw::AlertDialog::new(Some("Open Link"), Some("A YouTube Music or YouTube link"));
        let entry = gtk::Entry::builder()
            .placeholder_text("https://music.youtube.com/…")
            .activates_default(true)
            .build();
        if let Some(display) = gdk::Display::default() {
            let entry = entry.downgrade();
            display.clipboard().read_text_async(gio::Cancellable::NONE, move |res| {
                if let (Ok(Some(text)), Some(entry)) = (res, entry.upgrade())
                    && super::nav_for_url(&text).is_some() && entry.text().is_empty() {
                        entry.set_text(text.trim());
                        entry.select_region(0, -1);
                    }
            });
        }
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("open", "Open");
        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("open"));
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                entry,
                move |_, resp| {
                    if resp != "open" {
                        return;
                    }
                    let text = entry.text().to_string();
                    match super::nav_for_url(&text) {
                        Some(nav) => win.navigate(nav),
                        None if !text.trim().is_empty() => win.navigate(Nav::Search(text)),
                        None => {}
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }
}

fn seek_by(delta: f64) {
    let pb = Playback::get();
    let p = pb.player();
    pb.seek((p.position() + delta).max(0.0));
}
