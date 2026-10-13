//! The Library tab: the account's YouTube Music library when signed in, and
//! what's kept on this computer.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::browse::{BrowseView, ChipBar};
use super::collection::CollectionView;
use super::rows::{Row, RowList};
use super::{LocalList, Nav, navigate};
use crate::account::Account;
use crate::api::{BrowseTarget, CardKind, Chip, Item, PageKind, Shelf, ShelfKind};
use crate::library::Library;

/// (chip, browse id) of the account's library sections; "" is this computer.
const SECTIONS: [(&str, &str); 8] = [
    ("Playlists", "FEmusic_liked_playlists"),
    ("Liked music", "VLLM"),
    ("Songs", "FEmusic_liked_videos"),
    ("Albums", "FEmusic_liked_albums"),
    ("Artists", "FEmusic_library_corpus_track_artists"),
    ("Subscriptions", "FEmusic_library_corpus_artists"),
    ("History", "FEmusic_history"),
    ("On this computer", ""),
];

fn shortcut(icon: &str, title: &str, nav: Nav) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("library-shortcut");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let i = gtk::Image::from_icon_name(icon);
    i.set_pixel_size(22);
    i.add_css_class("library-shortcut-icon");
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    l.add_css_class("heading");
    content.append(&i);
    content.append(&l);
    b.set_child(Some(&content));
    b.connect_clicked(move |b| navigate(b, nav.clone()));
    b
}

fn shortcuts() -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    b.set_homogeneous(true);
    b.add_css_class("library-shortcuts");
    b.append(&shortcut("stm-thumbs-up-symbolic", "Liked songs", Nav::Local(LocalList::Liked)));
    b.append(&shortcut("document-open-recent-symbolic", "History", Nav::Local(LocalList::History)));
    b.upcast()
}

fn rows() -> Vec<Row> {
    let lib = Library::get();
    let mut rows = Vec::new();
    let history: Vec<Item> = lib.history().into_iter().take(20).map(Item::Track).collect();
    if !history.is_empty() {
        rows.push(Row::Shelf(Rc::new(Shelf {
            title: "Recently played".into(),
            kind: ShelfKind::Carousel,
            items: history,
            ..Default::default()
        })));
    }
    let saved = lib.saved();
    let (artists, collections): (Vec<_>, Vec<_>) = saved
        .into_iter()
        .partition(|c| matches!(c.kind, CardKind::Artist | CardKind::Profile));
    if !collections.is_empty() {
        rows.push(Row::Shelf(Rc::new(Shelf {
            title: "Albums & playlists".into(),
            kind: ShelfKind::Grid,
            items: collections.into_iter().map(Item::Card).collect(),
            ..Default::default()
        })));
    }
    if !artists.is_empty() {
        rows.push(Row::Shelf(Rc::new(Shelf {
            title: "Artists".into(),
            kind: ShelfKind::Carousel,
            items: artists.into_iter().map(Item::Card).collect(),
            ..Default::default()
        })));
    }
    if rows.is_empty() {
        rows.push(Row::Status {
            icon: "stm-library-symbolic".into(),
            title: "Your library is empty".into(),
            description: "Like songs and save albums, playlists and artists to find them here. \
                          Everything is kept on this computer."
                .into(),
            retry: false,
        });
    }
    rows
}

/// The local library.
fn local_view() -> RowList {
    let list = RowList::new();
    list.set_prefix(vec![Row::Widget(shortcuts())]);
    list.set_rows(rows());
    Library::get().connect_changed(glib::clone!(
        #[weak]
        list,
        move |_, what| {
            // History changes with every song; refresh it only when it's looked at.
            if what != "history" || list.is_mapped() {
                list.set_rows(rows());
            }
        }
    ));
    list.connect_map(|l| l.set_rows(rows()));
    list
}

/// The account's library: section chips over the chosen section.
fn account_view() -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let chips = ChipBar::new();
    let content = adw::Bin::new();
    content.set_vexpand(true);
    root.append(&chips);
    root.append(&content);
    let chip_list: Vec<Chip> = SECTIONS
        .iter()
        .map(|(title, id)| Chip {
            title: (*title).to_owned(),
            params: Some((*id).to_owned()),
            selected: false,
        })
        .collect();
    let current: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let show = {
        let content = content.downgrade();
        let current = current.clone();
        let chips = chips.downgrade();
        let chip_list = chip_list.clone();
        move |title: &str| {
            let (Some(content), Some(chips)) = (content.upgrade(), chips.upgrade()) else { return };
            let Some((_, id)) = SECTIONS.iter().find(|(t, _)| *t == title) else { return };
            current.replace(title.to_owned());
            chips.set_chips(&chip_list, Some(title));
            // One section at a time: the previous one's widgets are freed.
            let widget: gtk::Widget = match *id {
                "" => local_view().upcast(),
                "VLLM" => CollectionView::new(BrowseTarget {
                    id: "VLLM".into(),
                    params: None,
                    kind: PageKind::Playlist,
                })
                .upcast(),
                id => {
                    let view = BrowseView::new(BrowseTarget::new(id), false);
                    view.rows().set_empty_state("stm-library-symbolic", "Nothing here yet", "");
                    view.ensure_loaded();
                    view.upcast()
                }
            };
            content.set_child(Some(&widget));
        }
    };
    let show = Rc::new(show);
    {
        let show = show.clone();
        let current = current.clone();
        chips.connect_select(move |chip| {
            // Clicking the selected chip again keeps it selected.
            let title = chip.map(|c| c.title).unwrap_or_else(|| current.borrow().clone());
            let show = show.clone();
            glib::idle_add_local_once(move || show(&title));
        });
    }
    show(SECTIONS[0].0);
    root.upcast()
}

/// The Library tab's content, following the sign-in state.
pub fn library_view() -> gtk::Widget {
    let bin = adw::Bin::new();
    let update = glib::clone!(
        #[weak]
        bin,
        move |acc: &Account| {
            let widget: gtk::Widget = if acc.signed_in() {
                account_view().upcast()
            } else {
                local_view().upcast()
            };
            bin.set_child(Some(&widget));
        }
    );
    let acc = Account::get();
    update(&acc);
    acc.connect_signed_in_notify(update);
    bin.upcast()
}
