//! Main window: adaptive navigation (sidebar ↔ bottom bar), tabs, the watch sheet and mini player.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use common::sheet::{self, Hero, PlayerSheet};
use gtk::{gdk, gio, glib};

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
        pub toast_overlay: adw::ToastOverlay,
        pub split: adw::OverlaySplitView,
        pub stack: adw::ViewStack,
        pub switcher_bar: adw::ViewSwitcherBar,
        pub sidebar_nav: gtk::ListBox,
        pub sidebar_subs: gtk::ListBox,
        pub sidebar_subs_title: gtk::Label,
        pub navs: RefCell<Vec<(String, adw::NavigationView)>>,
        pub mini_revealer: gtk::Revealer,
        pub sheet: RefCell<Option<PlayerSheet>>,
        pub watch: RefCell<Option<WatchView>>,
        pub narrow_only: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
        pub wide_only: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
        pub narrow: Cell<bool>,
        pub expanded: Cell<bool>,
        pub root_feeds: RefCell<Vec<(String, FeedView)>>,
        pub inhibit: Cell<u32>,
        pub syncing_sidebar: Cell<bool>,
        pub refocus_queued: Cell<bool>,
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
            obj.setup_breakpoints();
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
        let (w, h) = std::env::var("SLOPTUBE_WINDOW_SIZE")
            .ok()
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or((1280, 820));
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

    /// Allocated heights along the main column (diagnostics).
    pub fn debug_layout(&self) -> String {
        let imp = self.imp();
        let mut out = format!(
            "window {}x{} | stack {} | mini_revealer {} (reveal {}) | switcher {}",
            self.width(),
            self.height(),
            imp.stack.height(),
            imp.mini_revealer.height(),
            imp.mini_revealer.reveals_child(),
            imp.switcher_bar.height()
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
        let t = adw::Toast::new(message);
        t.set_timeout(3);
        self.imp().toast_overlay.add_toast(t);
    }

    pub fn register_narrow_only(&self, w: &impl IsA<gtk::Widget>) {
        w.set_visible(self.imp().narrow.get());
        self.imp().narrow_only.borrow_mut().push(w.upcast_ref::<gtk::Widget>().downgrade());
    }

    pub fn register_wide_only(&self, w: &impl IsA<gtk::Widget>) {
        w.set_visible(!self.imp().narrow.get());
        self.imp().wide_only.borrow_mut().push(w.upcast_ref::<gtk::Widget>().downgrade());
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("sloptube");

        // Tabs
        for (name, title, icon) in TABS {
            let nav = adw::NavigationView::new();
            nav.connect_visible_page_notify(|nav| {
                if let Some(page) = nav.visible_page() {
                    hide_content_while_unmapped(&page);
                }
            });
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
            hide_content_while_unmapped(&root);
            nav.add(&root);
            let p = imp.stack.add_titled_with_icon(&nav, Some(name), title, icon);
            p.set_use_underline(false);
            imp.navs.borrow_mut().push((name.into(), nav));
        }
        imp.stack.set_vexpand(true);
        imp.switcher_bar.set_stack(Some(&imp.stack));

        // Mini player (stays laid out under the sheet while a video is open)
        let mini = MiniPlayer::new(&imp.playback);
        imp.mini_revealer.set_child(Some(&mini));
        imp.mini_revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        imp.mini_revealer.set_reveal_child(false);
        imp.mini_revealer.set_vexpand(false);

        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main_box.append(&imp.stack);
        main_box.append(&imp.mini_revealer);
        main_box.append(&imp.switcher_bar);

        // Sidebar
        imp.sidebar_nav.add_css_class("navigation-sidebar");
        for (name, title, icon) in TABS {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(name);
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            b.append(&gtk::Image::from_icon_name(icon));
            b.append(&gtk::Label::new(Some(title)));
            row.set_child(Some(&b));
            imp.sidebar_nav.append(&row);
        }
        // The selection only mirrors the visible tab. GTK also selects rows when focus
        // moves into the sidebar, as it does when the player collapses and hides the
        // focused button, so tabs switch on activation (a tap or Enter) instead.
        imp.sidebar_nav.connect_row_selected(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| {
                if !win.imp().syncing_sidebar.get() {
                    win.sync_sidebar();
                }
            }
        ));
        imp.sidebar_nav.connect_row_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, row| {
                let name = row.widget_name();
                if win.imp().stack.visible_child_name().as_deref() == Some(name.as_str()) {
                    // Tapping the tab that is showing returns to its root.
                    win.current_nav().pop_to_tag(&name);
                }
                win.imp().stack.set_visible_child_name(&name);
                win.collapse_player();
            }
        ));
        imp.stack.connect_visible_child_name_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.sync_sidebar()
        ));
        self.sync_sidebar();
        imp.sidebar_subs.add_css_class("navigation-sidebar");
        imp.sidebar_subs.set_selection_mode(gtk::SelectionMode::None);
        imp.sidebar_subs.connect_row_activated(|list, row| {
            super::navigate(list, Nav::Channel(row.widget_name().to_string()));
        });
        imp.sidebar_subs_title.set_text("Subscriptions");
        imp.sidebar_subs_title.set_xalign(0.0);
        imp.sidebar_subs_title.add_css_class("heading");
        imp.sidebar_subs_title.add_css_class("sidebar-section");
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        sidebar_box.append(&imp.sidebar_nav);
        sidebar_box.append(&imp.sidebar_subs_title);
        sidebar_box.append(&imp.sidebar_subs);
        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .child(&sidebar_box)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_show_title(false);
        let logo = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        logo.add_css_class("app-logo");
        let icon = gtk::Image::from_icon_name(config::APP_ID);
        icon.set_pixel_size(24);
        let label = gtk::Label::new(Some(config::APP_NAME));
        label.add_css_class("logo-text");
        logo.append(&icon);
        logo.append(&label);
        sidebar_header.pack_start(&logo);
        let sidebar_tv = adw::ToolbarView::new();
        sidebar_tv.add_top_bar(&sidebar_header);
        sidebar_tv.set_content(Some(&sidebar_scroll));

        imp.split.set_sidebar(Some(&sidebar_tv));
        imp.split.set_content(Some(&main_box));
        imp.split.set_min_sidebar_width(200.0);
        imp.split.set_max_sidebar_width(240.0);

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
        root_overlay.set_child(Some(&imp.split));
        root_overlay.add_overlay(&sheet);
        imp.toast_overlay.set_child(Some(&root_overlay));
        self.set_content(Some(&imp.toast_overlay));

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
        // Keep keyboard focus in the player while it covers the window. Crossing a
        // breakpoint (Super+F fullscreen, or a resize) hides the content for a frame;
        // GTK moves focus away and libadwaita replays that as Tab presses on the next
        // frame, which end up on a sidebar row or header button behind the sheet.
        // Check once that replay has run.
        self.connect_focus_widget_notify(|win| {
            if !win.imp().expanded.get() || win.imp().refocus_queued.replace(true) {
                return;
            }
            win.add_tick_callback(|win, _| {
                let win = win.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(win) = win.upgrade() {
                        win.imp().refocus_queued.set(false);
                        win.keep_focus_in_player();
                    }
                });
                glib::ControlFlow::Break
            });
        });

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

    fn setup_breakpoints(&self) {
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 860sp").unwrap());
        bp.connect_apply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.set_narrow(true)
        ));
        bp.connect_unapply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.set_narrow(false)
        ));
        self.add_breakpoint(bp);
        // Phones: one column, edge-to-edge thumbnails like the YouTube app.
        let phone = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 520sp").unwrap());
        phone.connect_apply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.add_css_class("phone")
        ));
        phone.connect_unapply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.remove_css_class("phone")
        ));
        self.add_breakpoint(phone);
        self.set_narrow(false);
    }

    fn set_narrow(&self, narrow: bool) {
        let imp = self.imp();
        imp.narrow.set(narrow);
        imp.split.set_collapsed(narrow);
        imp.split.set_show_sidebar(!narrow);
        imp.switcher_bar.set_reveal(narrow);
        if narrow {
            self.add_css_class("narrow");
        } else {
            self.remove_css_class("narrow");
        }
        imp.narrow_only.borrow_mut().retain(|w| w.upgrade().is_some());
        imp.wide_only.borrow_mut().retain(|w| w.upgrade().is_some());
        for w in imp.narrow_only.borrow().iter().filter_map(|w| w.upgrade()) {
            w.set_visible(narrow);
        }
        for w in imp.wide_only.borrow().iter().filter_map(|w| w.upgrade()) {
            w.set_visible(!narrow);
        }
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
            w.imp().stack.set_visible_child_name("library");
            w.collapse_player();
            let nav = w.current_nav();
            if nav.find_page("downloads").is_none() {
                nav.push(&super::downloads::downloads_page());
            }
        });
        action("show-explore", |w| {
            w.collapse_player();
            w.imp().stack.set_visible_child_name("explore");
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

    /// Select the visible tab's row in the sidebar.
    fn sync_sidebar(&self) {
        let imp = self.imp();
        let name = imp.stack.visible_child_name().unwrap_or_default();
        imp.syncing_sidebar.set(true);
        let mut child = imp.sidebar_nav.first_child();
        while let Some(c) = child {
            if let Some(row) = c.downcast_ref::<gtk::ListBoxRow>() {
                if row.widget_name() == name {
                    imp.sidebar_nav.select_row(Some(row));
                }
            }
            child = c.next_sibling();
        }
        imp.syncing_sidebar.set(false);
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

    fn keep_focus_in_player(&self) {
        let imp = self.imp();
        let (Some(sheet), Some(watch)) = (imp.sheet.borrow().clone(), imp.watch.borrow().clone()) else { return };
        if !imp.expanded.get() {
            return;
        }
        // Dialogs and menus may take focus from the player.
        let lost = GtkWindowExt::focus(self).is_none_or(|f| {
            !f.is_ancestor(&sheet)
                && f.ancestor(adw::Dialog::static_type()).is_none()
                && f.ancestor(gtk::Popover::static_type()).is_none()
        });
        if lost {
            watch.player_view().grab_player_focus();
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
        let name = imp.stack.visible_child_name().unwrap_or_default();
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
        let dialog = adw::AlertDialog::new(Some("Open YouTube Link"), None);
        let entry = gtk::Entry::builder()
            .placeholder_text("https://www.youtube.com/watch?v=…")
            .activates_default(true)
            .build();
        if let Some(display) = gdk::Display::default() {
            // Prefill from the clipboard when it holds a YouTube link.
            let entry = entry.downgrade();
            display.clipboard().read_text_async(gio::Cancellable::NONE, move |res| {
                if let (Ok(Some(text)), Some(entry)) = (res, entry.upgrade()) {
                    if super::nav_for_youtube_url(&text).is_some() && entry.text().is_empty() {
                        entry.set_text(text.trim());
                        entry.select_region(0, -1);
                    }
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
                    match super::nav_for_youtube_url(&text) {
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

/// On every press and release GTK sets `:active` on each widget from the one
/// pressed up to the window, and that restyles every widget in the window whose
/// style depends on an ancestor's state. CSS only skips hidden widgets, not
/// pages that stacks merely stop drawing, so each touch restyled every feed in
/// the other tabs and the back stack (15-25 ms per press and release). Hide a
/// page's content while the page is off screen to keep it out of that.
fn hide_content_while_unmapped(page: &adw::NavigationPage) {
    // SAFETY: the key is only ever used with `()`.
    if unsafe { page.data::<()>("sloptube-hides-content") }.is_some() {
        return;
    }
    unsafe { page.set_data("sloptube-hides-content", ()) };
    let sync = |page: &adw::NavigationPage| {
        if let Some(child) = page.child() {
            child.set_visible(page.is_mapped());
        }
    };
    page.connect_map(sync);
    page.connect_unmap(sync);
    sync(page);
}
