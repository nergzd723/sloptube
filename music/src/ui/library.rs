//! The Library tab: what's kept on this computer.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::rows::{Row, RowList};
use super::{LocalList, Nav, navigate};
use crate::api::{CardKind, Item, Shelf, ShelfKind};
use crate::library::Library;

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

pub fn library_view() -> RowList {
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
