//! Playlist page (also used for Watch later and Liked videos).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::card::CardStyle;
use super::feed::FeedView;
use super::{Nav, navigate};
use crate::api::{self, FeedPage, Item, PlaylistPage as Data};
use crate::image;
use crate::runtime;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PlaylistPage {
        pub id: RefCell<String>,
        pub data: RefCell<Option<Data>>,
        pub stack: gtk::Stack,
        pub header_revealer: gtk::Revealer,
        pub status: adw::StatusPage,
        pub feed: RefCell<Option<FeedView>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistPage {
        const NAME: &'static str = "SlopPlaylistPage";
        type Type = super::PlaylistPage;
        type ParentType = adw::NavigationPage;
    }

    impl ObjectImpl for PlaylistPage {}
    impl WidgetImpl for PlaylistPage {}
    impl NavigationPageImpl for PlaylistPage {}
}

glib::wrapper! {
    pub struct PlaylistPage(ObjectSubclass<imp::PlaylistPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn tag_items(page: &mut FeedPage, id: &str) {
    for item in &mut page.items {
        if let Item::Video(v) = item {
            v.playlist_id = Some(id.to_owned());
        }
    }
}

impl PlaylistPage {
    pub fn new(id: &str) -> Self {
        let title = match id {
            "WL" => "Watch later",
            "LL" => "Liked videos",
            _ => "Playlist",
        };
        let obj: Self = glib::Object::builder().property("title", title).build();
        obj.imp().id.replace(id.to_owned());
        obj.build();
        obj.load();
        obj
    }

    fn build(&self) {
        let imp = self.imp();
        let header = common::shell::header_bar();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        imp.stack.add_named(&spinner, Some("loading"));
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("pill");
        retry.set_halign(gtk::Align::Center);
        retry.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.load()
        ));
        imp.status.set_icon_name(Some("network-error-symbolic"));
        imp.status.set_title("Couldn't load this playlist");
        imp.status.set_child(Some(&retry));
        imp.stack.add_named(&imp.status, Some("error"));
        tv.set_content(Some(&imp.stack));
        self.set_child(Some(&tv));
    }

    fn load(&self) {
        let imp = self.imp();
        imp.stack.set_visible_child_name("loading");
        let id = imp.id.borrow().clone();
        let page = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().playlist(&id).await }).await;
            let Some(page) = page.upgrade() else { return };
            match res {
                Ok(d) => page.show(d),
                Err(e) => {
                    page.imp().status.set_description(Some(&glib::markup_escape_text(&e.to_string())));
                    page.imp().stack.set_visible_child_name("error");
                }
            }
        });
    }

    fn show(&self, mut d: Data) {
        let imp = self.imp();
        let id = imp.id.borrow().clone();
        if !d.title.is_empty() {
            self.set_title(&d.title);
        }
        tag_items(&mut d.content, &id);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        header.add_css_class("playlist-header");
        let thumb = gtk::Picture::new();
        thumb.set_content_fit(gtk::ContentFit::Cover);
        thumb.set_can_shrink(true);
        let scale = self.scale_factor().max(1) as u32;
        image::set_picture(
            &thumb,
            api::pick_thumbnail(&d.thumbnails, 240 * scale).map(|t| t.url.as_str()),
            240 * scale,
        );
        let frame = super::aspect::AspectBin::new(&thumb, 16.0 / 9.0);
        frame.set_size_request(160, -1);
        frame.set_valign(gtk::Align::Start);
        frame.add_css_class("thumb-frame");
        frame.set_overflow(gtk::Overflow::Hidden);
        header.append(&frame);
        let info = gtk::Box::new(gtk::Orientation::Vertical, 6);
        info.set_hexpand(true);
        let title = gtk::Label::new(Some(&d.title));
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.add_css_class("title-3");
        info.append(&title);
        if let Some(owner) = &d.owner {
            let b = gtk::Button::with_label(owner);
            b.add_css_class("flat");
            b.add_css_class("owner-button");
            b.set_halign(gtk::Align::Start);
            if let Some(oid) = d.owner_id.clone() {
                b.connect_clicked(move |b| navigate(b, Nav::Channel(oid.clone())));
            }
            info.append(&b);
        }
        let stats: Vec<&str> = d.stats.iter().map(String::as_str).filter(|s| *s != "Playlist").collect();
        if !stats.is_empty() {
            let l = gtk::Label::new(Some(&stats.join(" · ")));
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.add_css_class("dim-label");
            l.add_css_class("caption");
            info.append(&l);
        }
        if let Some(desc) = d.description.as_deref().filter(|d| !d.is_empty()) {
            let l = gtk::Label::new(Some(desc));
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_lines(2);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.add_css_class("dim-label");
            l.add_css_class("caption");
            info.append(&l);
        }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let play = gtk::Button::new();
        let pc = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        pc.append(&gtk::Image::from_icon_name("media-playback-start-symbolic"));
        pc.append(&gtk::Label::new(Some("Play all")));
        play.set_child(Some(&pc));
        play.add_css_class("pill");
        play.add_css_class("suggested-action");
        play.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.play(false)
        ));
        let shuffle = gtk::Button::from_icon_name("media-playlist-shuffle-symbolic");
        shuffle.add_css_class("pill");
        shuffle.set_tooltip_text(Some("Shuffle"));
        shuffle.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.play(true)
        ));
        buttons.append(&play);
        buttons.append(&shuffle);
        info.append(&buttons);
        header.append(&info);
        imp.header_revealer.set_child(Some(&header));
        imp.header_revealer.set_reveal_child(true);

        let feed = FeedView::new(CardStyle::COMPACT);
        feed.set_empty_state("view-list-symbolic", "This playlist is empty");
        let initial = Rc::new(RefCell::new(Some(d.content.clone())));
        let pid = id.clone();
        feed.set_loader(Rc::new(move |token: Option<String>| {
            let cached = if token.is_none() { initial.borrow_mut().take() } else { None };
            let pid = pid.clone();
            Box::pin(async move {
                if let Some(p) = cached {
                    return Ok(p);
                }
                let pid2 = pid.clone();
                let mut page = runtime::run(async move {
                    let c = api::client();
                    match token {
                        Some(t) => c.feed_more(&t).await,
                        None => c.playlist(&pid2).await.map(|p| p.content),
                    }
                })
                .await?;
                tag_items(&mut page, &pid);
                Ok(page)
            })
        }));
        let reveal = imp.header_revealer.downgrade();
        feed.vadjustment().connect_value_changed(move |adj| {
            if let Some(r) = reveal.upgrade() {
                if adj.value() > 120.0 && r.reveals_child() {
                    r.set_reveal_child(false);
                } else if adj.value() < 4.0 && !r.reveals_child() {
                    r.set_reveal_child(true);
                }
            }
        });
        feed.ensure_loaded();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&imp.header_revealer);
        content.append(&feed);
        imp.stack.add_named(&content, Some("content"));
        imp.stack.set_visible_child_name("content");
        imp.feed.replace(Some(feed));
        imp.data.replace(Some(d));
    }

    fn play(&self, shuffle: bool) {
        let imp = self.imp();
        let Some(d) = imp.data.borrow().clone() else { return };
        let videos: Vec<api::VideoItem> = d
            .content
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Video(v) => Some(v.clone()),
                _ => None,
            })
            .collect();
        if videos.is_empty() {
            return;
        }
        let pick = if shuffle {
            videos[rand::random_range(0..videos.len())].clone()
        } else {
            videos[0].clone()
        };
        navigate(
            self,
            Nav::Video {
                item: pick,
                playlist: Some(imp.id.borrow().clone()),
                start: Some(0.0),
            },
        );
    }
}
