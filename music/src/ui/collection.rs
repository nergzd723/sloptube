//! Album and playlist pages, and the local Liked songs and History lists.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::art::{ArtView, TintBin};
use super::browse::shelves_to_rows;
use super::cards::RowStyle;
use super::rows::{Loader, Row, RowList, TrackCtx, track_rows};
use super::{LocalList, Nav, actions, navigate};
use crate::api::{self, ArtistRef, BrowseTarget, Card, CollectionPage, Item, PageKind, Track, WatchTarget};
use crate::image;
use crate::library::Library;
use crate::playback::Playback;
use crate::runtime;

/// Clickable artist names ("Daft Punk & Pharrell Williams").
pub fn artist_links(artists: &[ArtistRef]) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_wrap(true);
    label.add_css_class("artist-links");
    set_artist_links(&label, artists);
    label.connect_activate_link(|l, uri| {
        if let Some(id) = uri.strip_prefix("artist:") {
            navigate(
                l,
                Nav::Page(
                    BrowseTarget {
                        id: id.to_owned(),
                        params: None,
                        kind: PageKind::Artist,
                    },
                    String::new(),
                ),
            );
        }
        glib::Propagation::Stop
    });
    label
}

pub fn set_artist_links(label: &gtk::Label, artists: &[ArtistRef]) {
    let n = artists.len();
    let mut markup = String::new();
    for (i, a) in artists.iter().enumerate() {
        if i > 0 {
            markup.push_str(if i + 1 == n { " &amp; " } else { ", " });
        }
        let name = glib::markup_escape_text(&a.name);
        match &a.id {
            Some(id) => markup.push_str(&format!("<a href=\"artist:{}\">{name}</a>", glib::markup_escape_text(id))),
            None => markup.push_str(&name),
        }
    }
    label.set_markup(&markup);
    label.set_visible(!artists.is_empty());
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CollectionView {
        pub target: RefCell<Option<BrowseTarget>>,
        pub local: RefCell<Option<LocalList>>,
        pub page: RefCell<Option<CollectionPage>>,
        pub tracks: Rc<RefCell<Vec<Track>>>,
        pub rows: RowList,
        pub tint: RefCell<Option<TintBin>>,
        pub layout: gtk::Box,
        pub art: RefCell<Option<ArtView>>,
        pub info: gtk::Box,
        pub title: gtk::Label,
        pub artists: RefCell<Option<gtk::Label>>,
        pub subtitle: gtk::Label,
        pub second: gtk::Label,
        pub description: gtk::Label,
        pub buttons: gtk::Box,
        pub save: gtk::Button,
        pub menu: gtk::MenuButton,
        pub on_title: RefCell<Option<Box<dyn Fn(&str)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionView {
        const NAME: &'static str = "StmCollectionView";
        type Type = super::CollectionView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for CollectionView {}
    impl WidgetImpl for CollectionView {}
    impl BinImpl for CollectionView {}
}

glib::wrapper! {
    pub struct CollectionView(ObjectSubclass<imp::CollectionView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollectionView {
    pub fn new(target: BrowseTarget) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().target.replace(Some(target));
        obj.build();
        obj.imp().rows.set_loader(obj.remote_loader());
        obj.imp().rows.reload();
        obj
    }

    pub fn new_local(list: LocalList) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().local.replace(Some(list));
        obj.build();
        obj.load_local();
        Library::get().connect_changed(glib::clone!(
            #[weak]
            obj,
            move |_, what| {
                let ours = match obj.imp().local.borrow().as_ref() {
                    Some(LocalList::Liked) => what == "liked",
                    Some(LocalList::History) => what == "history",
                    None => false,
                };
                if ours {
                    obj.load_local();
                }
            }
        ));
        obj
    }

    pub fn connect_title(&self, f: impl Fn(&str) + 'static) {
        self.imp().on_title.replace(Some(Box::new(f)));
    }

    fn build(&self) {
        let imp = self.imp();
        let art = ArtView::new(232, 232, 10.0);
        art.set_eager(true);
        art.add_css_class("collection-art");
        art.set_halign(gtk::Align::Center);
        imp.art.replace(Some(art.clone()));

        imp.title.set_wrap(true);
        imp.title.add_css_class("collection-title");
        let artists = artist_links(&[]);
        imp.subtitle.add_css_class("dimmed");
        imp.second.add_css_class("dimmed");
        imp.second.add_css_class("caption");
        imp.description.set_wrap(true);
        imp.description.set_lines(3);
        imp.description.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.description.add_css_class("dimmed");
        imp.description.add_css_class("collection-description");
        let expand = gtk::GestureClick::new();
        expand.connect_released(glib::clone!(
            #[weak(rename_to = d)]
            imp.description,
            move |_, _, _, _| {
                let open = d.lines() > 0;
                d.set_lines(if open { -1 } else { 3 });
                d.set_ellipsize(if open {
                    gtk::pango::EllipsizeMode::None
                } else {
                    gtk::pango::EllipsizeMode::End
                });
            }
        ));
        imp.description.add_controller(expand);

        // Buttons: save, shuffle, play, menu.
        imp.save.set_icon_name("stm-save-symbolic");
        imp.save.add_css_class("circular");
        imp.save.add_css_class("header-action");
        imp.save.set_valign(gtk::Align::Center);
        imp.save.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.toggle_saved()
        ));
        let shuffle = gtk::Button::from_icon_name("media-playlist-shuffle-symbolic");
        shuffle.add_css_class("circular");
        shuffle.add_css_class("header-action");
        shuffle.set_valign(gtk::Align::Center);
        shuffle.set_tooltip_text(Some("Shuffle"));
        shuffle.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.play(true)
        ));
        let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
        play.add_css_class("circular");
        play.add_css_class("big-play");
        play.set_tooltip_text(Some("Play"));
        play.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.play(false)
        ));
        imp.menu.set_icon_name("view-more-symbolic");
        imp.menu.add_css_class("circular");
        imp.menu.add_css_class("header-action");
        imp.menu.set_valign(gtk::Align::Center);
        imp.menu.set_create_popup_func(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |mb| {
                if let Some(card) = view.card() {
                    mb.set_menu_model(Some(&actions::menu_for(&Item::Card(card))));
                }
            }
        ));
        let get: Rc<dyn Fn() -> Option<Item>> = {
            let weak = self.downgrade();
            Rc::new(move || weak.upgrade().and_then(|v| v.card()).map(Item::Card))
        };
        actions::install(&imp.menu, get);
        imp.buttons.set_spacing(14);
        imp.buttons.add_css_class("collection-buttons");
        imp.buttons.append(&imp.save);
        imp.buttons.append(&shuffle);
        imp.buttons.append(&play);
        imp.buttons.append(&imp.menu);

        imp.info.set_orientation(gtk::Orientation::Vertical);
        imp.info.set_spacing(6);
        imp.info.set_valign(gtk::Align::End);
        imp.info.append(&imp.title);
        imp.info.append(&artists);
        imp.info.append(&imp.subtitle);
        imp.info.append(&imp.second);
        imp.info.append(&imp.description);
        imp.info.append(&imp.buttons);
        imp.artists.replace(Some(artists));

        imp.layout.add_css_class("collection-header");
        imp.layout.append(&art);
        imp.layout.append(&imp.info);
        let tint = TintBin::new(&imp.layout, 1.0);
        imp.tint.replace(Some(tint.clone()));
        imp.rows.set_prefix(vec![Row::Widget(tint.upcast())]);
        imp.rows.set_empty_state("stm-library-symbolic", "No songs", "");
        self.set_child(Some(&imp.rows));
        self.set_header_visible(false);

        super::on_layout(self, glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |narrow| view.apply_layout(narrow)
        ));
    }

    fn apply_layout(&self, narrow: bool) {
        let imp = self.imp();
        let art = imp.art.borrow().clone().unwrap();
        imp.layout.set_orientation(if narrow { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        imp.layout.set_spacing(if narrow { 16 } else { 28 });
        let size = if narrow { 232 } else { 248 };
        art.set_fixed_size(size, size);
        let align = if narrow { gtk::Align::Center } else { gtk::Align::Start };
        let xalign = if narrow { 0.5 } else { 0.0 };
        for l in [&imp.title, &imp.subtitle, &imp.second, &imp.description] {
            l.set_xalign(xalign);
            l.set_justify(if narrow { gtk::Justification::Center } else { gtk::Justification::Left });
        }
        if let Some(a) = imp.artists.borrow().as_ref() {
            a.set_xalign(xalign);
            a.set_justify(if narrow { gtk::Justification::Center } else { gtk::Justification::Left });
        }
        imp.buttons.set_halign(align);
        imp.info.set_halign(if narrow { gtk::Align::Fill } else { gtk::Align::Start });
    }

    fn set_header_visible(&self, v: bool) {
        if let Some(t) = self.imp().tint.borrow().as_ref() {
            t.set_visible(v);
        }
    }

    fn card(&self) -> Option<Card> {
        self.imp().page.borrow().as_ref().map(CollectionPage::card)
    }

    fn toggle_saved(&self) {
        let Some(card) = self.card() else { return };
        let lib = Library::get();
        let saved = lib.toggle_saved(&card);
        super::toast(if saved { "Saved to library" } else { "Removed from library" });
        self.update_saved();
    }

    fn update_saved(&self) {
        let imp = self.imp();
        let saved = self.card().is_some_and(|c| Library::get().is_saved(&c.target.id));
        imp.save.set_icon_name(if saved { "stm-saved-symbolic" } else { "stm-save-symbolic" });
        imp.save.set_tooltip_text(Some(if saved { "Remove from library" } else { "Save to library" }));
        imp.save.set_visible(imp.local.borrow().is_none());
    }

    fn title(&self) -> String {
        self.imp().title.text().to_string()
    }

    fn play(&self, shuffle: bool) {
        let imp = self.imp();
        let tracks = imp.tracks.borrow().clone();
        let title = self.title();
        let pb = Playback::get();
        if shuffle {
            pb.shuffle_tracks(tracks, &title);
            return;
        }
        // A long playlist plays through `next`, which pages the rest in as it goes.
        let page = imp.page.borrow();
        if let Some(p) = page.as_ref().filter(|p| p.continuation.is_some())
            && let Some(pl) = p.playlist_id.clone() {
                pb.set_shuffle(false);
                pb.play_watch(
                    WatchTarget {
                        playlist_id: Some(pl),
                        ..Default::default()
                    },
                    &title,
                );
                return;
            }
        pb.set_shuffle(false);
        pb.play_tracks(tracks, 0, &title);
    }

    fn ctx(&self, style: RowStyle) -> Rc<TrackCtx> {
        let tracks = self.imp().tracks.clone();
        let weak = self.downgrade();
        Rc::new(TrackCtx {
            style,
            on_activate: Some(Box::new(move |i| {
                let title = weak.upgrade().map(|v| v.title()).unwrap_or_default();
                Playback::get().play_tracks(tracks.borrow().clone(), i, &title);
            })),
        })
    }

    fn fill_header(&self, page: &CollectionPage) {
        let imp = self.imp();
        imp.title.set_text(&page.title);
        if let Some(a) = imp.artists.borrow().as_ref() {
            set_artist_links(a, &page.artists);
        }
        imp.subtitle.set_text(&page.subtitle);
        imp.subtitle.set_visible(!page.subtitle.is_empty());
        imp.second.set_text(&page.second_subtitle);
        imp.second.set_visible(!page.second_subtitle.is_empty());
        imp.description.set_text(page.description.as_deref().unwrap_or(""));
        imp.description.set_visible(page.description.is_some());
        if let Some(art) = imp.art.borrow().as_ref() {
            art.set_thumbnails(&page.thumbnails);
        }
        self.set_header_visible(true);
        if let Some(f) = imp.on_title.borrow().as_ref() {
            f(&page.title);
        }
        let thumbs = page.thumbnails.clone();
        let tint = imp.tint.borrow().clone();
        glib::spawn_future_local(async move {
            let c = image::dominant_color(&thumbs).await;
            if let Some(t) = tint {
                t.set_color(c);
            }
        });
    }

    fn remote_loader(&self) -> Loader {
        let weak = self.downgrade();
        Rc::new(move |token| {
            let weak = weak.clone();
            Box::pin(async move {
                let Some(view) = weak.upgrade() else {
                    return Ok((Vec::new(), None));
                };
                if let Some(t) = token {
                    let (tracks, cont) = runtime::run(async move { api::client().collection_more(&t).await }).await?;
                    let imp = view.imp();
                    let offset = imp.tracks.borrow().len();
                    imp.tracks.borrow_mut().extend(tracks.iter().cloned());
                    let style = if imp.page.borrow().as_ref().is_some_and(|p| p.kind == PageKind::Album) {
                        RowStyle::Numbered
                    } else {
                        RowStyle::Duration
                    };
                    return Ok((track_rows(&tracks, offset, &view.ctx(style)), cont));
                }
                let id = view.imp().target.borrow().as_ref().map(|t| t.id.clone()).unwrap_or_default();
                drop(view);
                let page = runtime::run(async move { api::client().collection(&id).await }).await?;
                let Some(view) = weak.upgrade() else {
                    return Ok((Vec::new(), None));
                };
                let imp = view.imp();
                view.fill_header(&page);
                imp.tracks.replace(page.tracks.clone());
                let style = if page.kind == PageKind::Album {
                    RowStyle::Numbered
                } else {
                    RowStyle::Duration
                };
                let mut rows = track_rows(&page.tracks, 0, &view.ctx(style));
                let cont = page.continuation.clone();
                if cont.is_none() {
                    rows.extend(shelves_to_rows(page.shelves.clone()));
                }
                imp.page.replace(Some(page));
                view.update_saved();
                Ok((rows, cont))
            })
        })
    }

    fn load_local(&self) {
        let imp = self.imp();
        let Some(list) = *imp.local.borrow() else { return };
        let lib = Library::get();
        let (title, tracks) = match list {
            LocalList::Liked => ("Liked songs", lib.liked()),
            LocalList::History => ("History", lib.history()),
        };
        let page = CollectionPage {
            id: String::new(),
            kind: PageKind::Playlist,
            title: title.into(),
            subtitle: "Saved on this computer".into(),
            second_subtitle: match tracks.len() {
                1 => "1 song".into(),
                n => format!("{n} songs"),
            },
            thumbnails: tracks.first().map(|t| t.thumbnails.clone()).unwrap_or_default(),
            tracks: tracks.clone(),
            ..Default::default()
        };
        self.fill_header(&page);
        imp.menu.set_visible(false);
        imp.tracks.replace(tracks.clone());
        let rows = if tracks.is_empty() {
            vec![Row::Status {
                icon: if list == LocalList::Liked { "stm-thumbs-up-symbolic" } else { "document-open-recent-symbolic" }.into(),
                title: if list == LocalList::Liked { "No liked songs yet" } else { "Nothing played yet" }.into(),
                description: if list == LocalList::Liked {
                    "Songs you like show up here."
                } else {
                    "Songs you play show up here."
                }
                .into(),
                retry: false,
            }]
        } else {
            track_rows(&tracks, 0, &self.ctx(RowStyle::Duration))
        };
        imp.rows.set_rows(rows);
        imp.page.replace(None);
        self.update_saved();
    }
}

/// A navigation page for an album, a playlist or a local list.
pub fn collection_page(nav: &Nav) -> adw::NavigationPage {
    let (view, title) = match nav {
        Nav::Local(list) => (
            CollectionView::new_local(*list),
            match list {
                LocalList::Liked => "Liked songs",
                LocalList::History => "History",
            }
            .to_owned(),
        ),
        Nav::Page(target, title) => (CollectionView::new(target.clone()), title.clone()),
        _ => unreachable!("not a collection"),
    };
    let chrome = super::page::page(&title, None, &view, false);
    let page = chrome.page.clone();
    view.connect_title(move |t| page.set_title(t));
    chrome.page
}
