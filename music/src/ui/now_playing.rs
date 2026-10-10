//! The Now playing page.
//!
//! On a phone: artwork, title, actions, seek bar and transport, with Up next,
//! Lyrics and Related in a bottom sheet, like the YouTube Music app. On a
//! desktop: the artwork on the left and the tabs on the right, with the
//! transport in the player bar below.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, graphene};

use super::art::{ArtView, TintBin};
use super::collection::{artist_links, set_artist_links};
use super::lyrics::{LyricsView, related_view};
use super::queue::{queue_view, scroll_to_current};
use super::{Nav, actions, format_time, navigate};
use crate::api::{BrowseTarget, Item, PageKind};
use crate::image;
use crate::library::Library;
use crate::playback::{Playback, RepeatMode};
use crate::player::PlayState;

const TABS: [(&str, &str); 3] = [("queue", "Up next"), ("lyrics", "Lyrics"), ("related", "Related")];

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct NowPlayingView {
        pub tint: RefCell<Option<TintBin>>,
        pub layout: gtk::Stack,
        pub sheet: adw::BottomSheet,
        pub column: gtk::Box,
        pub art_narrow: adw::Bin,
        pub art_wide: adw::Bin,
        pub tabs_wide: adw::Bin,
        pub art: RefCell<Option<ArtView>>,
        pub tabs: gtk::Box,
        pub tab_stack: gtk::Stack,
        pub tab_buttons: RefCell<Vec<gtk::ToggleButton>>,
        pub queue_list: RefCell<Option<gtk::ListView>>,
        pub top_narrow: gtk::Box,
        pub from: gtk::Label,
        pub title: gtk::Label,
        pub artists: RefCell<Option<gtk::Label>>,
        pub like: gtk::Button,
        pub album: gtk::Button,
        pub scale: gtk::Scale,
        pub position: gtk::Label,
        pub duration: gtk::Label,
        pub play: gtk::Button,
        pub spinner: adw::Spinner,
        pub shuffle: gtk::ToggleButton,
        pub repeat: gtk::Button,
        pub seeking: Cell<bool>,
        pub seek_commit: RefCell<Option<glib::SourceId>>,
        pub narrow: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for NowPlayingView {
        const NAME: &'static str = "StmNowPlayingView";
        type Type = super::NowPlayingView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for NowPlayingView {}
    impl WidgetImpl for NowPlayingView {}
    impl BinImpl for NowPlayingView {}
}

glib::wrapper! {
    pub struct NowPlayingView(ObjectSubclass<imp::NowPlayingView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for NowPlayingView {
    fn default() -> Self {
        Self::new()
    }
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b.add_css_class("circular");
    b.set_valign(gtk::Align::Center);
    b
}

pub fn repeat_icon(r: RepeatMode) -> &'static str {
    match r {
        RepeatMode::One => "media-playlist-repeat-song-symbolic",
        _ => "media-playlist-repeat-symbolic",
    }
}

pub fn repeat_tooltip(r: RepeatMode) -> &'static str {
    match r {
        RepeatMode::Off => "Repeat: off",
        RepeatMode::All => "Repeat: all",
        RepeatMode::One => "Repeat: one",
    }
}

pub fn cycle_repeat() {
    let pb = Playback::get();
    pb.set_repeat(match pb.repeat() {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    });
}

impl NowPlayingView {
    pub fn new() -> Self {
        let obj: Self = glib::Object::new();
        obj.build();
        obj
    }

    pub fn art(&self) -> ArtView {
        self.imp().art.borrow().clone().unwrap()
    }

    /// Where the artwork is drawn, in `target`'s coordinates.
    pub fn art_rect_in(&self, target: &impl IsA<gtk::Widget>) -> Option<graphene::Rect> {
        let art = self.art();
        if !art.is_mapped() {
            return None;
        }
        let b = art.compute_bounds(target)?;
        let side = b.width().min(b.height());
        Some(graphene::Rect::new(
            b.x() + (b.width() - side) / 2.0,
            b.y() + (b.height() - side) / 2.0,
            side,
            side,
        ))
    }

    /// The area that drags the page down on a phone.
    pub fn drag_area(&self) -> gtk::Widget {
        self.imp().column.clone().upcast()
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("now-playing");
        let art = ArtView::new_fill(560, 10.0);
        art.add_css_class("np-art");
        art.set_shadow(true);
        art.set_vexpand(true);
        art.set_hexpand(true);
        imp.art.replace(Some(art.clone()));

        // Tabs: Up next / Lyrics / Related.
        let switcher = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        switcher.set_homogeneous(true);
        switcher.add_css_class("np-tabs");
        let mut group: Option<gtk::ToggleButton> = None;
        let mut buttons = Vec::new();
        for (name, label) in TABS {
            let b = gtk::ToggleButton::with_label(&label.to_uppercase());
            b.add_css_class("flat");
            b.add_css_class("np-tab");
            if let Some(g) = &group {
                b.set_group(Some(g));
            } else {
                group = Some(b.clone());
            }
            b.connect_toggled(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |b| {
                    if b.is_active() {
                        view.show_tab(name);
                    }
                }
            ));
            switcher.append(&b);
            buttons.push(b);
        }
        let queue = queue_view();
        imp.tab_stack.add_named(&queue.root, Some("queue"));
        imp.tab_stack.add_named(&LyricsView::new(), Some("lyrics"));
        imp.tab_stack.add_named(&related_view(), Some("related"));
        imp.tab_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        imp.tab_stack.set_vexpand(true);
        imp.queue_list.replace(Some(queue.list));
        imp.tabs.set_orientation(gtk::Orientation::Vertical);
        imp.tabs.add_css_class("np-tabs-box");
        imp.tabs.append(&switcher);
        imp.tabs.append(&imp.tab_stack);
        buttons[0].set_active(true);
        imp.tab_buttons.replace(buttons);

        self.build_column();

        // Phone: player column with a bottom sheet for the tabs.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.set_homogeneous(true);
        bar.add_css_class("np-sheet-bar");
        for (name, label) in TABS {
            let b = gtk::Button::with_label(&label.to_uppercase());
            b.add_css_class("flat");
            b.connect_clicked(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| view.open_tab(name)
            ));
            bar.append(&b);
        }
        imp.sheet.set_content(Some(&imp.column));
        imp.sheet.set_bottom_bar(Some(&bar));
        imp.sheet.set_show_drag_handle(true);
        imp.sheet.set_full_width(true);
        // The bottom bar overlaps the content; keep the transport above it.
        imp.sheet.connect_bottom_bar_height_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |s| view.imp().column.set_margin_bottom(s.bottom_bar_height())
        ));
        imp.sheet.connect_open_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |s| {
                if s.is_open() {
                    view.on_tab_shown();
                }
            }
        ));

        // Desktop: artwork left, tabs right.
        let wide = gtk::Box::new(gtk::Orientation::Horizontal, 32);
        wide.add_css_class("np-wide");
        let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
        left.set_hexpand(true);
        let collapse = icon_button("go-down-symbolic", "Close");
        collapse.set_action_name(Some("win.collapse-player"));
        collapse.set_halign(gtk::Align::Start);
        left.append(&collapse);
        imp.art_wide.set_vexpand(true);
        left.append(&imp.art_wide);
        imp.tabs_wide.set_size_request(380, -1);
        imp.tabs_wide.set_hexpand(true);
        wide.append(&left);
        wide.append(&imp.tabs_wide);

        // Not homogeneous: the hidden desktop layout must not widen the phone one.
        imp.layout.set_hhomogeneous(false);
        imp.layout.set_vhomogeneous(false);
        imp.layout.add_named(&imp.sheet, Some("narrow"));
        imp.layout.add_named(&wide, Some("wide"));
        let tint = TintBin::new(&imp.layout, 1.0);
        imp.tint.replace(Some(tint.clone()));
        self.set_child(Some(&tint));

        self.bind_playback();
        self.set_narrow(true);
    }

    fn build_column(&self) {
        let imp = self.imp();
        let col = &imp.column;
        col.set_orientation(gtk::Orientation::Vertical);
        col.set_spacing(0);
        col.add_css_class("np-column");

        // Top: close, "Playing from", menu.
        let top = &imp.top_narrow;
        top.add_css_class("np-top");
        let collapse = icon_button("go-down-symbolic", "Close");
        collapse.set_action_name(Some("win.collapse-player"));
        let from_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        from_box.set_hexpand(true);
        let from_caption = gtk::Label::new(Some("PLAYING FROM"));
        from_caption.add_css_class("caption");
        from_caption.add_css_class("dimmed");
        imp.from.add_css_class("caption-heading");
        imp.from.set_ellipsize(gtk::pango::EllipsizeMode::End);
        from_box.append(&from_caption);
        from_box.append(&imp.from);
        let menu = gtk::MenuButton::new();
        menu.set_icon_name("view-more-symbolic");
        menu.add_css_class("flat");
        menu.add_css_class("circular");
        menu.set_create_popup_func(|mb| {
            if let Some(t) = Playback::get().current_track() {
                mb.set_menu_model(Some(&actions::menu_for(&Item::Track(t))));
            }
        });
        actions::install(&menu, Rc::new(|| Playback::get().current_track().map(Item::Track)));
        top.append(&collapse);
        top.append(&from_box);
        top.append(&menu);
        col.append(top);

        imp.art_narrow.set_vexpand(true);
        imp.art_narrow.add_css_class("np-art-slot");
        col.append(&imp.art_narrow);

        // Title, artists, like.
        let info = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        info.add_css_class("np-info");
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 4);
        texts.set_hexpand(true);
        imp.title.set_xalign(0.0);
        imp.title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.title.add_css_class("np-title");
        let artists = artist_links(&[]);
        artists.set_xalign(0.0);
        artists.set_wrap(false);
        artists.set_ellipsize(gtk::pango::EllipsizeMode::End);
        artists.add_css_class("np-artist");
        texts.append(&imp.title);
        texts.append(&artists);
        imp.artists.replace(Some(artists));
        imp.like.set_icon_name("stm-thumbs-up-symbolic");
        imp.like.add_css_class("flat");
        imp.like.add_css_class("circular");
        imp.like.set_valign(gtk::Align::Center);
        imp.like.connect_clicked(|_| {
            if let Some(t) = Playback::get().current_track() {
                Library::get().toggle_liked(&t);
            }
        });
        info.append(&texts);
        info.append(&imp.like);
        col.append(&info);

        // Action pills.
        let pills = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pills.add_css_class("np-pills");
        let pill = |icon: &str, label: &str| {
            let b = gtk::Button::new();
            b.set_child(Some(&adw::ButtonContent::builder().icon_name(icon).label(label).build()));
            b.add_css_class("pill-small");
            b
        };
        let radio = pill("stm-radio-symbolic", "Radio");
        radio.connect_clicked(|_| Playback::get().radio_here());
        imp.album.set_child(Some(
            &adw::ButtonContent::builder()
                .icon_name("media-optical-symbolic")
                .label("Album")
                .build(),
        ));
        imp.album.add_css_class("pill-small");
        imp.album.connect_clicked(|b| {
            if let Some(a) = Playback::get().current_track().and_then(|t| t.album) {
                navigate(
                    b,
                    Nav::Page(
                        BrowseTarget {
                            id: a.id,
                            params: None,
                            kind: PageKind::Album,
                        },
                        a.name,
                    ),
                );
            }
        });
        let share = pill("send-to-symbolic", "Share");
        share.connect_clicked(|b| {
            if let Some(t) = Playback::get().current_track() {
                super::copy_text(b, &t.url(), "Link copied");
            }
        });
        pills.append(&radio);
        pills.append(&imp.album);
        pills.append(&share);
        let pills_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&pills)
            .build();
        col.append(&pills_scroll);

        // Seek bar.
        imp.scale.set_orientation(gtk::Orientation::Horizontal);
        imp.scale.set_range(0.0, 1.0);
        imp.scale.set_draw_value(false);
        imp.scale.add_css_class("np-seek");
        imp.scale.connect_change_value(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, v| {
                view.user_seek(v);
                glib::Propagation::Proceed
            }
        ));
        col.append(&imp.scale);
        let times = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        times.add_css_class("np-times");
        imp.position.add_css_class("numeric");
        imp.position.add_css_class("caption");
        imp.duration.add_css_class("numeric");
        imp.duration.add_css_class("caption");
        imp.position.set_hexpand(true);
        imp.position.set_xalign(0.0);
        imp.duration.set_xalign(1.0);
        imp.position.set_text("0:00");
        imp.duration.set_text("0:00");
        times.append(&imp.position);
        times.append(&imp.duration);
        col.append(&times);

        // Transport.
        let transport = gtk::CenterBox::new();
        transport.add_css_class("np-transport");
        let middle = gtk::Box::new(gtk::Orientation::Horizontal, 22);
        let prev = icon_button("media-skip-backward-symbolic", "Previous");
        prev.add_css_class("np-skip");
        prev.connect_clicked(|_| Playback::get().previous());
        let next = icon_button("media-skip-forward-symbolic", "Next");
        next.add_css_class("np-skip");
        next.connect_clicked(|_| Playback::get().next());
        imp.play.set_icon_name("media-playback-start-symbolic");
        imp.play.add_css_class("circular");
        imp.play.add_css_class("np-play");
        imp.play.set_tooltip_text(Some("Play"));
        imp.play.connect_clicked(|_| Playback::get().toggle());
        let play_overlay = gtk::Overlay::new();
        play_overlay.set_child(Some(&imp.play));
        imp.spinner.set_can_target(false);
        imp.spinner.add_css_class("np-spinner");
        imp.spinner.set_visible(false);
        play_overlay.add_overlay(&imp.spinner);
        middle.append(&prev);
        middle.append(&play_overlay);
        middle.append(&next);
        imp.shuffle.set_icon_name("media-playlist-shuffle-symbolic");
        imp.shuffle.set_tooltip_text(Some("Shuffle"));
        imp.shuffle.add_css_class("flat");
        imp.shuffle.add_css_class("circular");
        imp.shuffle.set_valign(gtk::Align::Center);
        imp.shuffle.connect_toggled(|b| Playback::get().set_shuffle(b.is_active()));
        imp.repeat.set_icon_name("media-playlist-repeat-symbolic");
        imp.repeat.add_css_class("flat");
        imp.repeat.add_css_class("circular");
        imp.repeat.set_valign(gtk::Align::Center);
        imp.repeat.connect_clicked(|_| cycle_repeat());
        transport.set_start_widget(Some(&imp.shuffle));
        transport.set_center_widget(Some(&middle));
        transport.set_end_widget(Some(&imp.repeat));
        col.append(&transport);
    }

    fn show_tab(&self, name: &str) {
        self.imp().tab_stack.set_visible_child_name(name);
        self.on_tab_shown();
    }

    fn on_tab_shown(&self) {
        let imp = self.imp();
        if imp.tab_stack.visible_child_name().as_deref() == Some("queue")
            && let Some(list) = imp.queue_list.borrow().as_ref() {
                let list = list.clone();
                glib::idle_add_local_once(move || scroll_to_current(&list));
            }
    }

    fn open_tab(&self, name: &str) {
        let imp = self.imp();
        if let Some(i) = TABS.iter().position(|(n, _)| *n == name) {
            imp.tab_buttons.borrow()[i].set_active(true);
        }
        self.show_tab(name);
        imp.sheet.set_open(true);
    }

    /// Close the tabs sheet (phone); returns whether it was open.
    pub fn close_sheet(&self) -> bool {
        let s = &self.imp().sheet;
        let open = s.is_open() && self.imp().narrow.get();
        s.set_open(false);
        open
    }

    pub fn set_narrow(&self, narrow: bool) {
        let imp = self.imp();
        imp.narrow.set(narrow);
        let art = self.art();
        if narrow {
            imp.art_wide.set_child(None::<&gtk::Widget>);
            imp.tabs_wide.set_child(None::<&gtk::Widget>);
            imp.art_narrow.set_child(Some(&art));
            imp.sheet.set_sheet(Some(&imp.tabs));
            imp.layout.set_visible_child_name("narrow");
        } else {
            imp.art_narrow.set_child(None::<&gtk::Widget>);
            imp.sheet.set_sheet(None::<&gtk::Widget>);
            imp.sheet.set_open(false);
            imp.art_wide.set_child(Some(&art));
            imp.tabs_wide.set_child(Some(&imp.tabs));
            imp.layout.set_visible_child_name("wide");
        }
    }

    fn user_seek(&self, v: f64) {
        let imp = self.imp();
        imp.seeking.set(true);
        imp.position.set_text(&format_time(v));
        if let Some(id) = imp.seek_commit.take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(180),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    let imp = view.imp();
                    imp.seek_commit.take();
                    Playback::get().seek(imp.scale.value());
                    // Let the player catch up before following it again.
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400),
                        glib::clone!(
                            #[weak]
                            view,
                            move || view.imp().seeking.set(false)
                        ),
                    );
                }
            ),
        );
        imp.seek_commit.replace(Some(id));
    }

    fn update_track(&self) {
        let imp = self.imp();
        let pb = Playback::get();
        let track = pb.current_track();
        let t = track.clone().unwrap_or_default();
        imp.title.set_text(&t.title);
        if let Some(a) = imp.artists.borrow().as_ref() {
            set_artist_links(a, &t.artists);
        }
        imp.album.set_visible(t.album.is_some());
        let art = self.art();
        art.set_thumbnails(&t.thumbnails);
        if let Some(d) = t.duration {
            imp.duration.set_text(&format_time(d as f64));
        }
        self.update_like();
        let tint = imp.tint.borrow().clone();
        let thumbs = t.thumbnails.clone();
        glib::spawn_future_local(async move {
            let c = image::dominant_color(&thumbs).await;
            if let Some(tint) = tint {
                tint.set_color(c);
            }
        });
    }

    fn update_like(&self) {
        let imp = self.imp();
        let liked = Playback::get()
            .current_track()
            .is_some_and(|t| Library::get().is_liked(&t.video_id));
        if liked {
            imp.like.add_css_class("liked");
        } else {
            imp.like.remove_css_class("liked");
        }
        imp.like.set_tooltip_text(Some(if liked { "Remove from liked songs" } else { "Like" }));
    }

    fn update_state(&self) {
        let imp = self.imp();
        let pb = Playback::get();
        let playing = pb.is_playing() && pb.player().state() != PlayState::Ended;
        imp.play.set_icon_name(if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
        imp.play.set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
        let busy = pb.loading() || pb.player().state() == PlayState::Buffering;
        imp.spinner.set_visible(busy);
    }

    fn bind_playback(&self) {
        let imp = self.imp();
        let pb = Playback::get();
        let player = pb.player();
        pb.connect_track_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.update_track()
        ));
        pb.connect_queue_title_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |pb| {
                let t = pb.queue_title();
                view.imp().from.set_text(if t.is_empty() { "Your queue" } else { &t });
            }
        ));
        let t = pb.queue_title();
        imp.from.set_text(if t.is_empty() { "Your queue" } else { &t });
        Library::get().connect_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, what| {
                if what == "liked" {
                    view.update_like();
                }
            }
        ));
        {
            let sig = "notify::loading";
            pb.connect_local(
                sig,
                false,
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    #[upgrade_or]
                    None,
                    move |_| {
                        view.update_state();
                        None
                    }
                ),
            );
        }
        player.connect_state_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.update_state()
        ));
        player.connect_position_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |p| {
                let imp = view.imp();
                if imp.seeking.get() {
                    return;
                }
                imp.scale.set_value(p.position());
                imp.position.set_text(&format_time(p.position()));
            }
        ));
        player.connect_duration_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |p| {
                let imp = view.imp();
                let d = p.duration();
                if d > 0.0 {
                    imp.scale.set_range(0.0, d);
                    imp.duration.set_text(&format_time(d));
                }
            }
        ));
        pb.connect_shuffle_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |pb| view.imp().shuffle.set_active(pb.shuffle())
        ));
        let update_repeat = glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |pb: &Playback| {
                let imp = view.imp();
                let r = pb.repeat();
                imp.repeat.set_icon_name(repeat_icon(r));
                imp.repeat.set_tooltip_text(Some(repeat_tooltip(r)));
                if r == RepeatMode::Off {
                    imp.repeat.remove_css_class("toggled");
                } else {
                    imp.repeat.add_css_class("toggled");
                }
            }
        );
        update_repeat(&pb);
        pb.connect_repeat_notify(update_repeat);
        self.update_track();
        self.update_state();
    }
}
