//! The collapsed player bar shown above the navigation while browsing.

use std::cell::RefCell;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::glib;

use crate::image;
use crate::playback::Playback;
use crate::player::PlayState;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct MiniPlayer {
        pub playback: RefCell<Option<Playback>>,
        pub video: gtk::Picture,
        pub poster: gtk::Picture,
        pub title: gtk::Label,
        pub subtitle: gtk::Label,
        pub play: gtk::Button,
        pub progress: gtk::ProgressBar,
        pub row: RefCell<Option<gtk::Box>>,
        pub thumb: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MiniPlayer {
        const NAME: &'static str = "SlopMiniPlayer";
        type Type = super::MiniPlayer;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for MiniPlayer {}
    impl WidgetImpl for MiniPlayer {}
    impl BoxImpl for MiniPlayer {}
}

glib::wrapper! {
    pub struct MiniPlayer(ObjectSubclass<imp::MiniPlayer>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl MiniPlayer {
    pub fn new(playback: &Playback) -> Self {
        let obj: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .build();
        obj.imp().playback.replace(Some(playback.clone()));
        obj.build();
        obj
    }

    fn playback(&self) -> Playback {
        self.imp().playback.borrow().clone().unwrap()
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("mini-player");
        // Never take extra vertical space from the page above.
        self.set_vexpand(false);
        let playback = self.playback();
        let player = playback.player();

        imp.progress.add_css_class("mini-progress");
        imp.progress.set_can_target(false);
        self.append(&imp.progress);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("mini-row");

        let overlay = gtk::Overlay::new();
        imp.poster.set_content_fit(gtk::ContentFit::Cover);
        imp.poster.set_can_shrink(true);
        imp.video.set_content_fit(gtk::ContentFit::Contain);
        imp.video.set_can_shrink(true);
        imp.video.set_paintable(player.paintable().as_ref());
        overlay.set_child(Some(&imp.poster));
        overlay.add_overlay(&imp.video);
        // Fixed 96×54: the video's own pixel size must not inflate the bar.
        let thumb = crate::ui::aspect::AspectBin::new(&overlay, 16.0 / 9.0);
        thumb.set_size_request(96, -1);
        thumb.set_valign(gtk::Align::Center);
        thumb.set_overflow(gtk::Overflow::Hidden);
        thumb.add_css_class("mini-thumb");
        row.append(&thumb);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.set_valign(gtk::Align::Center);
        imp.title.set_xalign(0.0);
        imp.title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.title.add_css_class("mini-title");
        imp.subtitle.set_xalign(0.0);
        imp.subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.subtitle.add_css_class("dim-label");
        imp.subtitle.add_css_class("caption");
        text.append(&imp.title);
        text.append(&imp.subtitle);
        row.append(&text);

        imp.play.set_icon_name("media-playback-start-symbolic");
        imp.play.add_css_class("flat");
        imp.play.add_css_class("circular");
        imp.play.set_valign(gtk::Align::Center);
        imp.play.connect_clicked(glib::clone!(
            #[weak]
            playback,
            move |_| playback.toggle()
        ));
        row.append(&imp.play);
        let next = gtk::Button::from_icon_name("media-skip-forward-symbolic");
        next.add_css_class("flat");
        next.add_css_class("circular");
        next.set_valign(gtk::Align::Center);
        next.set_tooltip_text(Some("Next"));
        next.connect_clicked(glib::clone!(
            #[weak]
            playback,
            move |_| {
                playback.next();
            }
        ));
        row.append(&next);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.add_css_class("circular");
        close.set_valign(gtk::Align::Center);
        close.set_tooltip_text(Some("Stop"));
        close.connect_clicked(glib::clone!(
            #[weak]
            playback,
            move |_| playback.stop()
        ));
        row.append(&close);
        self.append(&row);

        imp.row.replace(Some(row.clone()));
        imp.thumb.replace(Some(thumb.upcast()));

        playback.connect_title_notify(glib::clone!(
            #[weak(rename_to = mini)]
            self,
            move |pb| mini.imp().title.set_text(&pb.title())
        ));
        playback.connect_channel_name_notify(glib::clone!(
            #[weak(rename_to = mini)]
            self,
            move |pb| mini.imp().subtitle.set_text(&pb.channel_name())
        ));
        playback.connect_thumbnail_notify(glib::clone!(
            #[weak(rename_to = mini)]
            self,
            move |pb| image::set_picture(&mini.imp().poster, Some(&pb.thumbnail()), 96 * mini.scale_factor().max(1) as u32)
        ));
        playback.connect_audio_only_notify(glib::clone!(
            #[weak(rename_to = mini)]
            self,
            move |pb| mini.imp().video.set_visible(!pb.audio_only())
        ));
        player.connect_state_notify(glib::clone!(
            #[weak(rename_to = mini)]
            self,
            move |p| mini.update_state(p.state(), p.is_playing_intent())
        ));
        player.connect_position_notify(glib::clone!(
            #[weak(rename_to = mini)]
            self,
            move |p| {
                let d = p.duration();
                mini.imp()
                    .progress
                    .set_fraction(if d > 0.0 { (p.position() / d).clamp(0.0, 1.0) } else { 0.0 });
            }
        ));
    }

    /// The video thumbnail (the sheet animates the video into it).
    pub fn thumb(&self) -> Option<gtk::Widget> {
        self.imp().thumb.borrow().clone()
    }

    /// The tappable/draggable bar.
    pub fn row(&self) -> Option<gtk::Box> {
        self.imp().row.borrow().clone()
    }

    fn update_state(&self, state: PlayState, playing: bool) {
        let imp = self.imp();
        imp.play.set_icon_name(if playing && state != PlayState::Ended {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        });
    }
}
