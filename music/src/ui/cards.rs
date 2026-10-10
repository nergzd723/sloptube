//! Item widgets: tiles for carousels and grids, rows for lists, mood tiles.
//!
//! All of them are rebindable, so lists and shelves recycle them while
//! scrolling instead of building new widgets.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};

use super::actions;
use super::art::{ArtView, Swatch, argb};
use crate::api::{Card, CardKind, Item, MoodTile as Mood, Track};
use crate::playback::Playback;

// ---------------------------------------------------------------------------
// Tile
// ---------------------------------------------------------------------------

mod tile_imp {
    use super::*;

    #[derive(Default)]
    pub struct TileCard {
        pub item: RefCell<Option<Item>>,
        pub art: RefCell<Option<ArtView>>,
        pub title: gtk::Label,
        pub subtitle: gtk::Label,
        pub play: gtk::Button,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TileCard {
        const NAME: &'static str = "StmTileCard";
        type Type = super::TileCard;
        type ParentType = gtk::Button;
    }

    impl ObjectImpl for TileCard {}
    impl WidgetImpl for TileCard {}
    impl ButtonImpl for TileCard {}
}

glib::wrapper! {
    /// Square artwork with a title and subtitle (album, playlist, artist, video).
    pub struct TileCard(ObjectSubclass<tile_imp::TileCard>)
        @extends gtk::Button, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl TileCard {
    pub fn new(width: i32) -> Self {
        let obj: Self = glib::Object::new();
        obj.build(width);
        obj
    }

    fn build(&self, width: i32) {
        let imp = self.imp();
        self.add_css_class("tile");
        self.add_css_class("flat");
        let art = ArtView::new(width, width, 8.0);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&art));
        imp.play.set_icon_name("media-playback-start-symbolic");
        imp.play.add_css_class("circular");
        imp.play.add_css_class("tile-play");
        imp.play.set_halign(gtk::Align::End);
        imp.play.set_valign(gtk::Align::End);
        imp.play.set_margin_end(8);
        imp.play.set_margin_bottom(8);
        imp.play.set_tooltip_text(Some("Play"));
        imp.play.connect_clicked(glib::clone!(
            #[weak(rename_to = tile)]
            self,
            move |_| {
                match tile.item() {
                    Some(Item::Card(c)) => actions::play_card(&c, false),
                    Some(Item::Track(t)) => Playback::get().play_song(t),
                    _ => {}
                }
            }
        ));
        overlay.add_overlay(&imp.play);

        for l in [&imp.title, &imp.subtitle] {
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.set_lines(2);
            // Tiny natural width: the artwork decides how wide the tile is.
            l.set_max_width_chars(1);
            l.set_hexpand(true);
        }
        imp.title.add_css_class("tile-title");
        imp.subtitle.add_css_class("tile-subtitle");
        imp.subtitle.add_css_class("dimmed");

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.append(&overlay);
        vbox.append(&imp.title);
        vbox.append(&imp.subtitle);
        self.set_child(Some(&vbox));
        imp.art.replace(Some(art));

        self.connect_clicked(|tile| {
            if let Some(item) = tile.item() {
                actions::activate(tile, &item);
            }
        });
        let get: Rc<dyn Fn() -> Option<Item>> = {
            let weak = self.downgrade();
            Rc::new(move || weak.upgrade().and_then(|t| t.item()))
        };
        actions::install(self, get.clone());
        actions::attach_context_menu(self, get);
    }

    pub fn item(&self) -> Option<Item> {
        self.imp().item.borrow().clone()
    }

    pub fn bind(&self, item: &Item, width: i32) {
        let imp = self.imp();
        let art = imp.art.borrow().clone().unwrap();
        imp.item.replace(Some(item.clone()));
        let center = |c: bool| {
            for l in [&imp.title, &imp.subtitle] {
                l.set_xalign(if c { 0.5 } else { 0.0 });
                l.set_justify(if c { gtk::Justification::Center } else { gtk::Justification::Left });
            }
        };
        match item {
            Item::Track(t) => {
                let h = if t.is_video { width * 9 / 16 } else { width };
                art.set_fixed_size(width, h);
                art.set_round(false);
                art.set_thumbnails(&t.thumbnails);
                imp.title.set_text(&t.title);
                let sub = [t.artist_names(), t.extra.clone().unwrap_or_default()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" • ");
                imp.subtitle.set_text(&sub);
                center(false);
            }
            Item::Card(c) => {
                art.set_fixed_size(width, width);
                art.set_round(c.kind.round());
                art.set_thumbnails(&c.thumbnails);
                imp.title.set_text(&c.title);
                imp.subtitle.set_text(&c.subtitle);
                center(c.kind.round());
            }
            Item::Mood(_) => {}
        }
        imp.subtitle.set_visible(!imp.subtitle.text().is_empty());
        imp.play.set_visible(actions::playable(item));
        self.set_tooltip_text(Some(&imp.title.text()));
    }
}

// ---------------------------------------------------------------------------
// List row
// ---------------------------------------------------------------------------

/// How a song row looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RowStyle {
    /// Artwork, title, artists • album.
    #[default]
    Art,
    /// Track number instead of artwork, duration on the right (albums).
    Numbered,
    /// Artwork, title, artists, duration (playlists, queue).
    Duration,
}

mod row_imp {
    use super::*;

    #[derive(Default)]
    pub struct ListRow {
        pub item: RefCell<Option<Item>>,
        pub art: RefCell<Option<ArtView>>,
        pub number: gtk::Label,
        pub playing: gtk::Image,
        pub title: gtk::Label,
        pub explicit: gtk::Label,
        pub subtitle: gtk::Label,
        pub duration: gtk::Label,
        pub menu: gtk::MenuButton,
        pub style: Cell<RowStyle>,
        pub handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Position in the queue (queue rows get "Remove from queue").
        pub queue_index: Cell<i32>,
        pub on_click: RefCell<Option<Rc<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ListRow {
        const NAME: &'static str = "StmListRow";
        type Type = super::ListRow;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for ListRow {
        fn dispose(&self) {
            if let Some(h) = self.handler.take() {
                Playback::get().disconnect(h);
            }
        }
    }

    impl WidgetImpl for ListRow {}
    impl BoxImpl for ListRow {}
}

glib::wrapper! {
    /// A song, album, playlist or artist as a row.
    pub struct ListRow(ObjectSubclass<row_imp::ListRow>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl ListRow {
    pub fn new() -> Self {
        let obj: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Horizontal)
            .property("spacing", 12)
            .build();
        obj.build();
        obj
    }

    fn build(&self) {
        let imp = self.imp();
        imp.queue_index.set(-1);
        self.add_css_class("list-row");
        let art = ArtView::new(48, 48, 4.0);
        let lead = gtk::Overlay::new();
        lead.set_child(Some(&art));
        imp.number.add_css_class("track-number");
        imp.number.add_css_class("dimmed");
        imp.number.add_css_class("numeric");
        imp.number.set_width_chars(3);
        imp.number.set_halign(gtk::Align::Center);
        imp.number.set_valign(gtk::Align::Center);
        lead.add_overlay(&imp.number);
        imp.playing.set_icon_name(Some("stm-playing-symbolic"));
        imp.playing.add_css_class("playing-indicator");
        imp.playing.set_halign(gtk::Align::Fill);
        imp.playing.set_valign(gtk::Align::Fill);
        imp.playing.set_visible(false);
        lead.add_overlay(&imp.playing);
        lead.set_valign(gtk::Align::Center);
        self.append(&lead);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_valign(gtk::Align::Center);
        text.set_hexpand(true);
        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        imp.title.set_xalign(0.0);
        imp.title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.title.add_css_class("row-title");
        imp.explicit.set_text("E");
        imp.explicit.add_css_class("explicit");
        imp.explicit.set_valign(gtk::Align::Center);
        imp.explicit.set_tooltip_text(Some("Explicit"));
        title_box.append(&imp.title);
        title_box.append(&imp.explicit);
        imp.subtitle.set_xalign(0.0);
        imp.subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.subtitle.add_css_class("dimmed");
        imp.subtitle.add_css_class("row-subtitle");
        text.append(&title_box);
        text.append(&imp.subtitle);
        self.append(&text);

        imp.duration.add_css_class("dimmed");
        imp.duration.add_css_class("numeric");
        imp.duration.set_valign(gtk::Align::Center);
        self.append(&imp.duration);

        imp.menu.set_icon_name("view-more-symbolic");
        imp.menu.add_css_class("flat");
        imp.menu.add_css_class("circular");
        imp.menu.set_valign(gtk::Align::Center);
        imp.menu.set_tooltip_text(Some("More"));
        // Built when opened: a menu model creates a popover, which made binding slow.
        imp.menu.set_create_popup_func(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |mb| {
                if let Some(item) = row.item() {
                    let menu = actions::menu_for(&item);
                    let qi = row.imp().queue_index.get();
                    if qi >= 0 && qi != Playback::get().current() {
                        let q = gio::Menu::new();
                        q.append(Some("Remove from queue"), Some("row.remove"));
                        menu.append_section(None, &q);
                    }
                    mb.set_menu_model(Some(&menu));
                }
            }
        ));
        self.append(&imp.menu);
        imp.art.replace(Some(art));

        let get: Rc<dyn Fn() -> Option<Item>> = {
            let weak = self.downgrade();
            Rc::new(move || weak.upgrade().and_then(|r| r.item()))
        };
        actions::install(self, get.clone());
        actions::attach_context_menu(self, get);
        let group = gio::SimpleActionGroup::new();
        let remove = gio::SimpleAction::new("remove", None);
        remove.connect_activate(glib::clone!(
            #[weak(rename_to = row)]
            self,
            move |_, _| {
                let qi = row.imp().queue_index.get();
                if qi >= 0 {
                    Playback::get().remove_at(qi as u32);
                }
            }
        ));
        group.add_action(&remove);
        self.insert_action_group("row", Some(&group));
    }

    pub fn item(&self) -> Option<Item> {
        self.imp().item.borrow().clone()
    }

    /// Make the row clickable by itself (outside list views).
    pub fn set_on_click(&self, f: impl Fn() + 'static) {
        let first = self.imp().on_click.replace(Some(Rc::new(f))).is_none();
        if first {
            self.set_cursor_from_name(Some("pointer"));
            let click = gtk::GestureClick::new();
            click.connect_released(glib::clone!(
                #[weak(rename_to = row)]
                self,
                move |g, _, x, y| {
                    // Only a release on the row counts (the press may have been elsewhere).
                    if x < 0.0 || y < 0.0 || x > row.width() as f64 || y > row.height() as f64 {
                        return;
                    }
                    // Not when the menu button was hit.
                    if let Some(w) = row.pick(x, y, gtk::PickFlags::DEFAULT)
                        && (w.is_ancestor(&row.imp().menu) || w == *row.imp().menu.upcast_ref::<gtk::Widget>()) {
                            return;
                        }
                    g.set_state(gtk::EventSequenceState::Claimed);
                    let f = row.imp().on_click.borrow().clone();
                    if let Some(f) = f {
                        f();
                    }
                }
            ));
            self.add_controller(click);
        }
    }

    pub fn set_queue_index(&self, index: i32) {
        self.imp().queue_index.set(index);
    }

    fn track_playing(&self) {
        let imp = self.imp();
        let pb = Playback::get();
        let playing = match imp.item.borrow().as_ref() {
            Some(Item::Track(t)) => pb.active() && pb.video_id() == t.video_id,
            _ => false,
        };
        let numbered = imp.style.get() == RowStyle::Numbered;
        imp.playing.set_visible(playing);
        imp.number.set_visible(numbered && !playing);
        if playing {
            self.add_css_class("playing");
        } else {
            self.remove_css_class("playing");
        }
    }

    pub fn bind_track(&self, t: &Track, style: RowStyle) {
        let imp = self.imp();
        let art = imp.art.borrow().clone().unwrap();
        imp.item.replace(Some(Item::Track(t.clone())));
        imp.style.set(style);
        imp.menu.set_menu_model(None::<&gio::MenuModel>);
        imp.title.set_text(&t.title);
        imp.explicit.set_visible(t.explicit);
        let numbered = style == RowStyle::Numbered;
        if numbered {
            art.set_fixed_size(32, 40);
            art.show_none();
            imp.number.set_text(&t.index.map(|i| i.to_string()).unwrap_or_default());
        } else {
            art.set_fixed_size(48, 48);
            art.set_round(false);
            art.set_thumbnails(&t.thumbnails);
        }
        art.set_opacity(if numbered { 0.0 } else { 1.0 });
        let sub = match style {
            RowStyle::Art => {
                let mut parts = Vec::new();
                if t.is_video {
                    parts.push("Video".to_owned());
                }
                let a = t.artist_names();
                if !a.is_empty() {
                    parts.push(a);
                }
                if let Some(al) = &t.album {
                    parts.push(al.name.clone());
                } else if let Some(e) = &t.extra {
                    parts.push(e.clone());
                }
                parts.join(" • ")
            }
            RowStyle::Numbered => t.extra.clone().unwrap_or_else(|| t.artist_names()),
            RowStyle::Duration => t.artist_names(),
        };
        imp.subtitle.set_text(&sub);
        imp.subtitle.set_visible(!sub.is_empty());
        let show_duration = style != RowStyle::Art && t.duration.is_some();
        imp.duration.set_visible(show_duration);
        if let Some(d) = t.duration {
            imp.duration.set_text(&super::format_time(d as f64));
        }
        if t.unavailable {
            self.add_css_class("unavailable");
        } else {
            self.remove_css_class("unavailable");
        }
        if imp.handler.borrow().is_none() {
            let h = Playback::get().connect_video_id_notify(glib::clone!(
                #[weak(rename_to = row)]
                self,
                move |_| row.track_playing()
            ));
            imp.handler.replace(Some(h));
        }
        self.track_playing();
    }

    pub fn bind_card(&self, c: &Card) {
        let imp = self.imp();
        let art = imp.art.borrow().clone().unwrap();
        imp.item.replace(Some(Item::Card(c.clone())));
        imp.style.set(RowStyle::Art);
        imp.menu.set_menu_model(None::<&gio::MenuModel>);
        art.set_fixed_size(48, 48);
        art.set_opacity(1.0);
        art.set_round(c.kind.round());
        art.set_thumbnails(&c.thumbnails);
        imp.title.set_text(&c.title);
        imp.explicit.set_visible(false);
        let sub = if c.subtitle.is_empty() { c.kind.label().to_owned() } else { c.subtitle.clone() };
        imp.subtitle.set_text(&sub);
        imp.subtitle.set_visible(true);
        imp.duration.set_visible(false);
        imp.number.set_visible(false);
        imp.playing.set_visible(false);
        self.remove_css_class("playing");
        self.remove_css_class("unavailable");
        imp.menu.set_visible(c.kind != CardKind::Mix || c.play.is_some());
    }

    pub fn bind_item(&self, item: &Item, style: RowStyle) {
        match item {
            Item::Track(t) => self.bind_track(t, style),
            Item::Card(c) => self.bind_card(c),
            Item::Mood(_) => {}
        }
    }
}

impl Default for ListRow {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtView {
    fn show_none(&self) {
        use crate::image::ArtTarget;
        self.show_texture(None);
        crate::image::set_url(self, None, 0);
    }
}

// ---------------------------------------------------------------------------
// Mood tile
// ---------------------------------------------------------------------------

mod mood_imp {
    use super::*;

    #[derive(Default)]
    pub struct MoodButton {
        pub mood: RefCell<Option<Mood>>,
        pub swatch: Swatch,
        pub label: gtk::Label,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MoodButton {
        const NAME: &'static str = "StmMoodButton";
        type Type = super::MoodButton;
        type ParentType = gtk::Button;
    }

    impl ObjectImpl for MoodButton {}
    impl WidgetImpl for MoodButton {}
    impl ButtonImpl for MoodButton {}
}

glib::wrapper! {
    /// A "moods & genres" button with its colour stripe.
    pub struct MoodButton(ObjectSubclass<mood_imp::MoodButton>)
        @extends gtk::Button, gtk::Widget,
        @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl MoodButton {
    pub fn new() -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        obj.add_css_class("mood-tile");
        imp.swatch.set_size_request(6, -1);
        imp.label.set_xalign(0.0);
        imp.label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.label.set_hexpand(true);
        imp.label.set_max_width_chars(18);
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.append(&imp.swatch);
        b.append(&imp.label);
        obj.set_child(Some(&b));
        obj.connect_clicked(|btn| {
            if let Some(m) = btn.imp().mood.borrow().clone() {
                super::navigate(btn, super::Nav::Page(m.target, m.title));
            }
        });
        obj
    }

    pub fn bind(&self, m: &Mood) {
        let imp = self.imp();
        imp.label.set_text(&m.title);
        imp.swatch.set_color(m.color.map(argb));
        imp.swatch.set_visible(m.color.is_some());
        imp.mood.replace(Some(m.clone()));
    }
}

impl Default for MoodButton {
    fn default() -> Self {
        Self::new()
    }
}
