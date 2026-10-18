//! The player bar under the pages: a mini player on phones, a full transport
//! bar (like music.youtube.com) on desktops.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use common::sheet::MINI_ART_HEIGHT;

use super::actions;
use super::art::ArtView;
use super::now_playing::{cycle_repeat, repeat_icon, repeat_tooltip};
use super::format_time;
use crate::api::Item;
use crate::library::Library;
use crate::playback::{Playback, RepeatMode};
use crate::player::PlayState;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PlayerBar {
        pub mini: gtk::Box,
        pub mini_row: gtk::Box,
        pub mini_art: RefCell<Option<ArtView>>,
        pub mini_title: gtk::Label,
        pub mini_artist: gtk::Label,
        pub mini_play: gtk::Button,
        pub mini_spinner: adw::Spinner,
        pub mini_progress: gtk::ProgressBar,

        pub full: gtk::Box,
        pub seek: gtk::Scale,
        pub art: RefCell<Option<ArtView>>,
        pub title: gtk::Label,
        pub artist: gtk::Label,
        pub play: gtk::Button,
        pub spinner: adw::Spinner,
        pub time: gtk::Label,
        pub like: gtk::Button,
        pub volume: gtk::Scale,
        pub mute: gtk::Button,
        pub shuffle: gtk::ToggleButton,
        pub repeat: gtk::Button,
        pub expand: gtk::Button,
        pub seeking: Cell<bool>,
        pub seek_commit: RefCell<Option<glib::SourceId>>,
        pub narrow: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerBar {
        const NAME: &'static str = "StmPlayerBar";
        type Type = super::PlayerBar;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for PlayerBar {}
    impl WidgetImpl for PlayerBar {}
    impl BoxImpl for PlayerBar {}
}

glib::wrapper! {
    pub struct PlayerBar(ObjectSubclass<imp::PlayerBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for PlayerBar {
    fn default() -> Self {
        Self::new()
    }
}

fn flat_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.add_css_class("circular");
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(tooltip));
    b
}

fn texts(title: &gtk::Label, artist: &gtk::Label) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    b.set_valign(gtk::Align::Center);
    b.set_hexpand(true);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("bar-title");
    artist.set_xalign(0.0);
    artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
    artist.add_css_class("dimmed");
    artist.add_css_class("bar-artist");
    b.append(title);
    b.append(artist);
    b
}

fn volume_icon(v: f64, muted: bool) -> &'static str {
    if muted || v <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if v < 0.34 {
        "audio-volume-low-symbolic"
    } else if v < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

impl PlayerBar {
    pub fn new() -> Self {
        let obj: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .build();
        obj.build();
        obj
    }

    /// The mini player's draggable row.
    pub fn mini_row(&self) -> gtk::Box {
        self.imp().mini_row.clone()
    }

    /// The artwork the Now playing page shrinks into.
    pub fn art(&self) -> ArtView {
        let imp = self.imp();
        if imp.narrow.get() {
            imp.mini_art.borrow().clone().unwrap()
        } else {
            imp.art.borrow().clone().unwrap()
        }
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("player-bar");
        self.set_vexpand(false);

        // Mini player.
        // As tall as SlopTube's video thumbnail, so both mini players match.
        let art = ArtView::new(MINI_ART_HEIGHT, MINI_ART_HEIGHT, 4.0);
        art.set_eager(true);
        art.set_valign(gtk::Align::Center);
        imp.mini_row.set_spacing(10);
        imp.mini_row.add_css_class("mini-row");
        imp.mini_row.append(&art);
        imp.mini_row.append(&texts(&imp.mini_title, &imp.mini_artist));
        imp.mini_play.set_icon_name("media-playback-start-symbolic");
        imp.mini_play.add_css_class("flat");
        imp.mini_play.add_css_class("circular");
        imp.mini_play.add_css_class("mini-play");
        imp.mini_play.set_valign(gtk::Align::Center);
        imp.mini_play.connect_clicked(|_| Playback::get().toggle());
        let play_overlay = gtk::Overlay::new();
        play_overlay.set_child(Some(&imp.mini_play));
        imp.mini_spinner.set_can_target(false);
        imp.mini_spinner.set_visible(false);
        play_overlay.add_overlay(&imp.mini_spinner);
        imp.mini_row.append(&play_overlay);
        let next = flat_button("media-skip-forward-symbolic", "Next");
        next.connect_clicked(|_| Playback::get().next());
        imp.mini_row.append(&next);
        imp.mini_progress.add_css_class("mini-progress");
        imp.mini_progress.set_can_target(false);
        imp.mini.set_orientation(gtk::Orientation::Vertical);
        imp.mini.add_css_class("mini-player");
        imp.mini.append(&imp.mini_row);
        imp.mini.append(&imp.mini_progress);
        imp.mini_art.replace(Some(art));
        self.append(&imp.mini);

        // Desktop bar.
        imp.seek.set_orientation(gtk::Orientation::Horizontal);
        imp.seek.set_range(0.0, 1.0);
        imp.seek.set_draw_value(false);
        imp.seek.add_css_class("bar-seek");
        imp.seek.connect_change_value(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, v| {
                bar.user_seek(v);
                glib::Propagation::Proceed
            }
        ));
        let row = gtk::CenterBox::new();
        row.add_css_class("bar-row");

        let left = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let prev = flat_button("media-skip-backward-symbolic", "Previous");
        prev.connect_clicked(|_| Playback::get().previous());
        imp.play.set_icon_name("media-playback-start-symbolic");
        imp.play.add_css_class("circular");
        imp.play.add_css_class("bar-play");
        imp.play.set_valign(gtk::Align::Center);
        imp.play.connect_clicked(|_| Playback::get().toggle());
        let play_overlay = gtk::Overlay::new();
        play_overlay.set_child(Some(&imp.play));
        imp.spinner.set_can_target(false);
        imp.spinner.set_visible(false);
        play_overlay.add_overlay(&imp.spinner);
        let next = flat_button("media-skip-forward-symbolic", "Next");
        next.connect_clicked(|_| Playback::get().next());
        imp.time.add_css_class("numeric");
        imp.time.add_css_class("dimmed");
        imp.time.add_css_class("caption");
        imp.time.set_margin_start(8);
        left.append(&prev);
        left.append(&play_overlay);
        left.append(&next);
        left.append(&imp.time);

        let center = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        center.add_css_class("bar-center");
        let art = ArtView::new(42, 42, 4.0);
        art.set_eager(true);
        art.set_valign(gtk::Align::Center);
        center.append(&art);
        let info = texts(&imp.title, &imp.artist);
        info.set_size_request(220, -1);
        center.append(&info);
        imp.like.set_icon_name("sloptube-thumbs-up-symbolic");
        imp.like.add_css_class("flat");
        imp.like.add_css_class("circular");
        imp.like.set_valign(gtk::Align::Center);
        imp.like.connect_clicked(|_| {
            if let Some(t) = Playback::get().current_track() {
                Library::get().toggle_liked(&t);
            }
        });
        center.append(&imp.like);
        let menu = gtk::MenuButton::new();
        menu.set_icon_name("view-more-symbolic");
        menu.add_css_class("flat");
        menu.add_css_class("circular");
        menu.set_valign(gtk::Align::Center);
        menu.set_create_popup_func(|mb| {
            if let Some(t) = Playback::get().current_track() {
                mb.set_menu_model(Some(&actions::menu_for(&Item::Track(t))));
            }
        });
        actions::install(&menu, Rc::new(|| Playback::get().current_track().map(Item::Track)));
        center.append(&menu);
        // Clicking the track opens Now playing.
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |g, _, x, y| {
                if let Some(w) = g.widget() {
                    if x < 0.0 || y < 0.0 || x > w.width() as f64 || y > w.height() as f64 {
                        return;
                    }
                    if let Some(hit) = w.pick(x, y, gtk::PickFlags::DEFAULT)
                        && hit.ancestor(gtk::Button::static_type()).is_some() {
                            return;
                        }
                }
                bar.activate_action("win.toggle-player", None).ok();
            }
        ));
        center.add_controller(click);
        imp.art.replace(Some(art));

        let right = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        imp.mute.add_css_class("flat");
        imp.mute.add_css_class("circular");
        imp.mute.set_valign(gtk::Align::Center);
        imp.mute.set_tooltip_text(Some("Mute"));
        imp.mute.connect_clicked(|_| {
            let p = Playback::get().player();
            p.set_muted(!p.muted());
        });
        imp.volume.set_orientation(gtk::Orientation::Horizontal);
        imp.volume.set_range(0.0, 1.0);
        imp.volume.set_increments(0.05, 0.1);
        imp.volume.set_draw_value(false);
        imp.volume.set_size_request(96, -1);
        imp.volume.add_css_class("bar-volume");
        imp.volume.connect_value_changed(|s| {
            let p = Playback::get().player();
            if (p.volume() - s.value()).abs() > 0.001 {
                p.set_volume(s.value());
                if p.muted() {
                    p.set_muted(false);
                }
            }
        });
        imp.repeat.add_css_class("flat");
        imp.repeat.add_css_class("circular");
        imp.repeat.set_valign(gtk::Align::Center);
        imp.repeat.connect_clicked(|_| cycle_repeat());
        imp.shuffle.set_icon_name("media-playlist-shuffle-symbolic");
        imp.shuffle.add_css_class("flat");
        imp.shuffle.add_css_class("circular");
        imp.shuffle.set_valign(gtk::Align::Center);
        imp.shuffle.set_tooltip_text(Some("Shuffle"));
        imp.shuffle.connect_toggled(|b| Playback::get().set_shuffle(b.is_active()));
        imp.expand.set_icon_name("go-up-symbolic");
        imp.expand.add_css_class("flat");
        imp.expand.add_css_class("circular");
        imp.expand.set_valign(gtk::Align::Center);
        imp.expand.set_tooltip_text(Some("Now playing"));
        imp.expand.set_action_name(Some("win.toggle-player"));
        right.append(&imp.mute);
        right.append(&imp.volume);
        right.append(&imp.repeat);
        right.append(&imp.shuffle);
        right.append(&imp.expand);

        row.set_start_widget(Some(&left));
        row.set_center_widget(Some(&center));
        row.set_end_widget(Some(&right));
        imp.full.set_orientation(gtk::Orientation::Vertical);
        imp.full.add_css_class("desktop-bar");
        imp.full.append(&imp.seek);
        imp.full.append(&row);
        self.append(&imp.full);

        self.bind_playback();
        self.set_narrow(true);
    }

    pub fn set_narrow(&self, narrow: bool) {
        let imp = self.imp();
        imp.narrow.set(narrow);
        imp.mini.set_visible(narrow);
        imp.full.set_visible(!narrow);
    }

    /// Point the expand button the right way.
    pub fn set_expanded(&self, expanded: bool) {
        let imp = self.imp();
        imp.expand.set_icon_name(if expanded { "go-down-symbolic" } else { "go-up-symbolic" });
        imp.expand.set_tooltip_text(Some(if expanded { "Close Now playing" } else { "Now playing" }));
    }

    fn user_seek(&self, v: f64) {
        let imp = self.imp();
        imp.seeking.set(true);
        let total = imp.time.text().split(" / ").nth(1).map(str::to_owned).unwrap_or_default();
        imp.time.set_text(&format!("{} / {total}", format_time(v)));
        if let Some(id) = imp.seek_commit.take() {
            id.remove();
        }
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(180),
            glib::clone!(
                #[weak(rename_to = bar)]
                self,
                move || {
                    let imp = bar.imp();
                    imp.seek_commit.take();
                    Playback::get().seek(imp.seek.value());
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(400),
                        glib::clone!(
                            #[weak]
                            bar,
                            move || bar.imp().seeking.set(false)
                        ),
                    );
                }
            ),
        );
        imp.seek_commit.replace(Some(id));
    }

    fn update_track(&self) {
        let imp = self.imp();
        let t = Playback::get().current_track().unwrap_or_default();
        for (title, artist, art) in [
            (&imp.mini_title, &imp.mini_artist, imp.mini_art.borrow().clone()),
            (&imp.title, &imp.artist, imp.art.borrow().clone()),
        ] {
            title.set_text(&t.title);
            artist.set_text(&t.artist_names());
            if let Some(a) = art {
                a.set_thumbnails(&t.thumbnails);
            }
        }
        let byline = t.byline();
        imp.artist.set_text(&byline);
        self.update_like();
        self.update_time();
    }

    fn update_like(&self) {
        let imp = self.imp();
        let liked = Playback::get()
            .current_track()
            .is_some_and(|t| Library::get().is_track_liked(&t));
        if liked {
            imp.like.add_css_class("liked");
        } else {
            imp.like.remove_css_class("liked");
        }
    }

    fn update_time(&self) {
        let imp = self.imp();
        let pb = Playback::get();
        let p = pb.player();
        let dur = if p.duration() > 0.0 {
            p.duration()
        } else {
            pb.current_track().and_then(|t| t.duration).unwrap_or(0) as f64
        };
        imp.time.set_text(&format!("{} / {}", format_time(p.position()), format_time(dur)));
        imp.mini_progress
            .set_fraction(if dur > 0.0 { (p.position() / dur).clamp(0.0, 1.0) } else { 0.0 });
        if !imp.seeking.get() {
            if dur > 0.0 {
                imp.seek.set_range(0.0, dur);
            }
            imp.seek.set_value(p.position());
        }
    }

    fn update_state(&self) {
        let imp = self.imp();
        let pb = Playback::get();
        let playing = pb.is_playing() && pb.player().state() != PlayState::Ended;
        let icon = if playing {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        };
        imp.mini_play.set_icon_name(icon);
        imp.play.set_icon_name(icon);
        imp.play.set_tooltip_text(Some(if playing { "Pause" } else { "Play" }));
        let busy = pb.loading() || pb.player().state() == PlayState::Buffering;
        imp.mini_spinner.set_visible(busy);
        imp.spinner.set_visible(busy);
    }

    fn bind_playback(&self) {
        let pb = Playback::get();
        let player = pb.player();
        pb.connect_track_changed(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.update_track()
        ));
        pb.connect_loading_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.update_state()
        ));
        player.connect_state_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.update_state()
        ));
        player.connect_position_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.update_time()
        ));
        player.connect_duration_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |_| bar.update_time()
        ));
        Library::get().connect_changed(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |_, what| {
                if what == "liked" {
                    bar.update_like();
                }
            }
        ));
        let update_volume = glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |p: &crate::player::Player| {
                let imp = bar.imp();
                if (imp.volume.value() - p.volume()).abs() > 0.001 {
                    imp.volume.set_value(p.volume());
                }
                imp.mute.set_icon_name(volume_icon(p.volume(), p.muted()));
            }
        );
        update_volume(&player);
        let uv = update_volume.clone();
        player.connect_volume_notify(move |p| uv(p));
        player.connect_muted_notify(move |p| update_volume(p));
        pb.connect_shuffle_notify(glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |pb| bar.imp().shuffle.set_active(pb.shuffle())
        ));
        let update_repeat = glib::clone!(
            #[weak(rename_to = bar)]
            self,
            move |pb: &Playback| {
                let imp = bar.imp();
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
