//! Cards for videos, playlists and channels (grid and compact list layouts).

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use super::aspect::AspectBin;
use super::{Nav, navigate};
use crate::api::{self, Item};
use crate::image;
use crate::runtime;

#[derive(Debug, Clone, Copy, Default)]
pub struct CardStyle {
    /// Horizontal row (thumbnail left) instead of a grid card.
    pub compact: bool,
    pub show_avatar: bool,
    pub show_channel: bool,
}

impl CardStyle {
    pub const GRID: CardStyle = CardStyle {
        compact: false,
        show_avatar: true,
        show_channel: true,
    };
    pub const COMPACT: CardStyle = CardStyle {
        compact: true,
        show_avatar: false,
        show_channel: true,
    };
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ItemCard {
        pub item: RefCell<Option<Item>>,
        pub style: Cell<CardStyle>,
        pub picture: gtk::Picture,
        pub thumb_frame: RefCell<Option<AspectBin>>,
        pub channel_avatar_big: adw::Avatar,
        pub duration: gtk::Label,
        pub badge: gtk::Label,
        pub progress: gtk::ProgressBar,
        pub avatar: adw::Avatar,
        pub title: gtk::Label,
        pub subtitle: gtk::Label,
        pub detail: gtk::Label,
        pub menu_button: gtk::MenuButton,
        pub actions: gio::SimpleActionGroup,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ItemCard {
        const NAME: &'static str = "SlopItemCard";
        type Type = super::ItemCard;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for ItemCard {}
    impl WidgetImpl for ItemCard {}
    impl BoxImpl for ItemCard {}
}

glib::wrapper! {
    pub struct ItemCard(ObjectSubclass<imp::ItemCard>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl ItemCard {
    pub fn new(style: CardStyle) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().style.set(style);
        obj.build();
        obj
    }

    fn build(&self) {
        let imp = self.imp();
        let style = imp.style.get();
        self.add_css_class("item-card");

        // Thumbnail
        imp.picture.set_content_fit(gtk::ContentFit::Cover);
        imp.picture.set_can_shrink(true);
        imp.picture.add_css_class("thumbnail");
        let thumb_overlay = gtk::Overlay::new();
        thumb_overlay.add_css_class("thumb-frame");
        thumb_overlay.set_overflow(gtk::Overflow::Hidden);
        thumb_overlay.set_child(Some(&imp.picture));

        imp.channel_avatar_big.set_size(if style.compact { 72 } else { 112 });
        imp.channel_avatar_big.set_show_initials(true);
        imp.channel_avatar_big.set_halign(gtk::Align::Center);
        imp.channel_avatar_big.set_valign(gtk::Align::Center);
        imp.channel_avatar_big.set_visible(false);
        thumb_overlay.add_overlay(&imp.channel_avatar_big);

        imp.duration.add_css_class("duration-badge");
        imp.duration.set_halign(gtk::Align::End);
        imp.duration.set_valign(gtk::Align::End);
        thumb_overlay.add_overlay(&imp.duration);

        imp.badge.add_css_class("live-badge");
        imp.badge.set_halign(gtk::Align::End);
        imp.badge.set_valign(gtk::Align::End);
        thumb_overlay.add_overlay(&imp.badge);

        imp.progress.add_css_class("watched-bar");
        imp.progress.set_valign(gtk::Align::End);
        imp.progress.set_can_target(false);
        thumb_overlay.add_overlay(&imp.progress);

        let frame = AspectBin::new(&thumb_overlay, 16.0 / 9.0);
        imp.thumb_frame.replace(Some(frame.clone()));

        // Text
        imp.title.set_xalign(0.0);
        imp.title.set_wrap(true);
        imp.title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        imp.title.set_lines(2);
        imp.title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.title.add_css_class("card-title");
        for l in [&imp.subtitle, &imp.detail] {
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_lines(2);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.add_css_class("card-subtitle");
            l.add_css_class("dim-label");
        }
        imp.detail.set_lines(1);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        text.append(&imp.title);
        text.append(&imp.subtitle);
        text.append(&imp.detail);

        imp.menu_button.set_icon_name("view-more-symbolic");
        imp.menu_button.add_css_class("flat");
        imp.menu_button.add_css_class("card-menu");
        imp.menu_button.set_valign(gtk::Align::Start);
        imp.menu_button.set_tooltip_text(Some("More"));
        self.setup_actions();

        if style.compact {
            self.set_orientation(gtk::Orientation::Horizontal);
            self.set_spacing(10);
            self.add_css_class("compact");
            frame.set_size_request(168, -1);
            frame.set_valign(gtk::Align::Start);
            self.append(&frame);
            self.append(&text);
            self.append(&imp.menu_button);
        } else {
            self.set_orientation(gtk::Orientation::Vertical);
            self.set_spacing(10);
            self.set_size_request(272, -1);
            self.append(&frame);
            let info = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            info.add_css_class("card-info");
            imp.avatar.set_size(36);
            imp.avatar.set_show_initials(true);
            imp.avatar.set_valign(gtk::Align::Start);
            imp.avatar.set_visible(style.show_avatar);
            imp.avatar.set_cursor_from_name(Some("pointer"));
            let click = gtk::GestureClick::new();
            click.connect_pressed(glib::clone!(
                #[weak(rename_to = card)]
                self,
                move |g, _, _, _| {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(id) = card.channel_id() {
                        navigate(&card, Nav::Channel(id));
                    }
                }
            ));
            imp.avatar.add_controller(click);
            info.append(&imp.avatar);
            info.append(&text);
            info.append(&imp.menu_button);
            self.append(&info);
        }
    }

    fn setup_actions(&self) {
        let imp = self.imp();
        let group = &imp.actions;
        let add = |name: &str, f: fn(&ItemCard)| {
            let action = gio::SimpleAction::new(name, None);
            let card = self.downgrade();
            action.connect_activate(move |_, _| {
                if let Some(card) = card.upgrade() {
                    f(&card);
                }
            });
            group.add_action(&action);
        };
        add("watch-later", |c| c.watch_later());
        add("download", |c| {
            if let Some(Item::Video(v)) = c.item() {
                if let Some(win) = super::main_window() {
                    win.start_download(&v);
                }
            }
        });
        add("copy-link", |c| {
            if let Some(url) = c.item().map(|i| item_url(&i)) {
                super::copy_to_clipboard(c, &url);
            }
        });
        add("channel", |c| {
            if let Some(id) = c.channel_id() {
                navigate(c, Nav::Channel(id));
            }
        });
        add("play-audio", |c| {
            if let Some(Item::Video(v)) = c.item() {
                if let Some(win) = super::main_window() {
                    win.playback().set_audio_only(true);
                    win.navigate(Nav::video(v));
                }
            }
        });
        self.insert_action_group("card", Some(group));
    }

    fn watch_later(&self) {
        let Some(Item::Video(v)) = self.item() else { return };
        let card = self.downgrade();
        super::spawn(async move {
            let id = v.id.clone();
            let res = runtime::run(async move { api::client().set_watch_later(&id, true).await }).await;
            if let Some(card) = card.upgrade() {
                match res {
                    Ok(()) => super::toast(&card, "Saved to Watch later"),
                    Err(e) => super::toast(&card, &format!("Couldn't save: {e}")),
                }
            }
        });
    }

    fn menu_for(item: &Item) -> gio::Menu {
        let menu = gio::Menu::new();
        if let Item::Video(_) = item {
            if api::client().is_signed_in() {
                menu.append(Some("Save to Watch later"), Some("card.watch-later"));
            }
            menu.append(Some("Play as audio"), Some("card.play-audio"));
            menu.append(Some("Download"), Some("card.download"));
            menu.append(Some("Go to channel"), Some("card.channel"));
        }
        menu.append(Some("Copy link"), Some("card.copy-link"));
        menu
    }

    pub fn item(&self) -> Option<Item> {
        self.imp().item.borrow().clone()
    }

    fn channel_id(&self) -> Option<String> {
        match self.imp().item.borrow().as_ref()? {
            Item::Video(v) => v.channel_id.clone(),
            Item::Playlist(p) => p.channel_id.clone(),
            Item::Channel(c) => Some(c.id.clone()),
        }
    }

    pub fn bind(&self, item: &Item) {
        let imp = self.imp();
        let style = imp.style.get();
        imp.item.replace(Some(item.clone()));
        let scale = self.scale_factor().max(1) as u32;
        let thumb_width = if style.compact { 168 } else { 400 } * scale;
        imp.menu_button.set_menu_model(Some(&Self::menu_for(item)));
        imp.channel_avatar_big.set_visible(false);
        imp.picture.set_visible(true);
        imp.badge.set_visible(false);
        imp.progress.set_visible(false);
        imp.detail.set_visible(false);
        self.remove_css_class("channel-card");

        match item {
            Item::Video(v) => {
                imp.title.set_text(&v.title);
                image::set_picture(&imp.picture, Some(&v.thumbnail_url(thumb_width)), thumb_width);
                match (&v.duration, v.live, v.upcoming) {
                    (_, true, _) => {
                        imp.badge.set_text("LIVE");
                        imp.badge.set_visible(true);
                        imp.duration.set_visible(false);
                    }
                    (_, _, true) => {
                        imp.duration.set_text("UPCOMING");
                        imp.duration.set_visible(true);
                    }
                    (Some(d), _, _) => {
                        imp.duration.set_text(d);
                        imp.duration.set_visible(true);
                    }
                    _ => imp.duration.set_visible(false),
                }
                if let Some(f) = v.watched_fraction.filter(|f| *f > 0.01) {
                    imp.progress.set_fraction(f);
                    imp.progress.set_visible(true);
                }
                if style.compact {
                    imp.subtitle.set_text(v.channel_name.as_deref().unwrap_or_default());
                    imp.subtitle.set_visible(style.show_channel && v.channel_name.is_some());
                    let detail = v.subtitle(false);
                    imp.detail.set_text(&detail);
                    imp.detail.set_visible(!detail.is_empty());
                } else {
                    imp.subtitle.set_text(&v.subtitle(style.show_channel));
                    imp.subtitle.set_visible(true);
                }
                imp.avatar.set_text(v.channel_name.as_deref());
                image::set_avatar(
                    &imp.avatar,
                    v.channel_avatar.as_deref().map(|u| image::sized_avatar_url(u, 36 * scale)).as_deref(),
                );
                imp.avatar.set_visible(style.show_avatar && !style.compact);
            }
            Item::Playlist(p) => {
                imp.title.set_text(&p.title);
                let url = api::pick_thumbnail(&p.thumbnails, thumb_width).map(|t| t.url.clone());
                image::set_picture(&imp.picture, url.as_deref(), thumb_width);
                let count = if p.is_mix {
                    "Mix".to_owned()
                } else {
                    p.video_count.clone().unwrap_or_else(|| "Playlist".into())
                };
                imp.duration.set_text(&count);
                imp.duration.set_visible(true);
                let mut parts: Vec<String> = Vec::new();
                if let Some(c) = &p.channel_name {
                    parts.push(c.clone());
                }
                parts.push(if p.is_mix { "Mix".into() } else { "Playlist".into() });
                imp.subtitle.set_text(&parts.join(" · "));
                imp.subtitle.set_visible(true);
                imp.avatar.set_visible(false);
            }
            Item::Channel(c) => {
                self.add_css_class("channel-card");
                imp.title.set_text(&c.name);
                image::set_picture(&imp.picture, None, 0);
                imp.channel_avatar_big.set_visible(true);
                imp.channel_avatar_big.set_text(Some(&c.name));
                image::set_avatar(
                    &imp.channel_avatar_big,
                    c.avatar.as_deref().map(|u| image::sized_avatar_url(u, 176 * scale)).as_deref(),
                );
                imp.duration.set_visible(false);
                let parts: Vec<&str> = [c.handle.as_deref(), c.subscribers.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect();
                imp.subtitle.set_text(&parts.join(" · "));
                imp.subtitle.set_visible(!parts.is_empty());
                imp.avatar.set_visible(false);
            }
        }
    }

    /// Open the item when clicked (for cards used outside list views).
    pub fn make_clickable(&self) {
        self.set_cursor_from_name(Some("pointer"));
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = card)]
            self,
            move |g, _, _, _| {
                g.set_state(gtk::EventSequenceState::Claimed);
                card.activate_item();
            }
        ));
        self.add_controller(click);
    }

    pub fn activate_item(&self) {
        if let Some(item) = self.item() {
            navigate(self, nav_for_item(&item));
        }
    }
}

pub fn nav_for_item(item: &Item) -> Nav {
    match item {
        Item::Video(v) => Nav::video(v.clone()),
        Item::Channel(c) => Nav::Channel(c.id.clone()),
        Item::Playlist(p) => {
            if p.is_mix {
                if let Some(first) = &p.first_video_id {
                    return Nav::Video {
                        item: api::VideoItem {
                            id: first.clone(),
                            ..Default::default()
                        },
                        playlist: Some(p.id.clone()),
                        start: None,
                    };
                }
            }
            Nav::Playlist(p.id.clone())
        }
    }
}

pub fn item_url(item: &Item) -> String {
    match item {
        Item::Video(v) => format!("https://www.youtube.com/watch?v={}", v.id),
        Item::Channel(c) => format!("https://www.youtube.com/channel/{}", c.id),
        Item::Playlist(p) => format!("https://www.youtube.com/playlist?list={}", p.id),
    }
}
