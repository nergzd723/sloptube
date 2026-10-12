//! Explore: YouTube's destination pages (Music, Live, Gaming, ...) rendered as shelves.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::card::{CardStyle, ItemCard};
use crate::api::{self, FeedPage, Shelf};
use crate::runtime;

/// (title, browse id, icon, css class)
pub const DESTINATIONS: &[(&str, &str, &str, &str)] = &[
    ("Music", "UC-9-kyTW8ZkZNDHQJ6FgpwQ", "sloptube-music-symbolic", "music"),
    ("Live", "UC4R8DWoMoI7CAwX8_LjQHig", "sloptube-live-symbolic", "live"),
    ("Gaming", "UCOpNcN46UbXVtpKMrmU4Abg", "sloptube-gaming-symbolic", "gaming"),
    ("News", "UCYfdidRxbB8Qhf0Nx7ioOYw", "sloptube-news-symbolic", "news"),
    ("Sports", "UCEgdi0XIXXZ-qJOFPf4JSKw", "sloptube-sports-symbolic", "sports"),
    ("Learning", "UCtFRv9O2AHqOZjjynzrv-xg", "sloptube-learning-symbolic", "learning"),
    ("Fashion & Beauty", "UCrpQ4p1Ql_hG8rKXIKM1MOQ", "sloptube-fashion-symbolic", "fashion"),
];

/// Render shelves (title + horizontally scrolling cards).
pub fn shelves_widget(page: &FeedPage) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 18);
    b.add_css_class("shelves");
    let mut shelves: Vec<Shelf> = page.shelves.clone();
    if !page.items.is_empty() {
        shelves.push(Shelf {
            title: "More".into(),
            items: page.items.clone(),
        });
    }
    for shelf in shelves {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        if !shelf.title.is_empty() {
            let title = gtk::Label::new(Some(&shelf.title));
            title.set_xalign(0.0);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.add_css_class("title-4");
            title.add_css_class("shelf-title");
            section.append(&title);
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("shelf-row");
        for item in &shelf.items {
            let card = ItemCard::new(CardStyle::GRID);
            card.set_size_request(272, -1);
            card.bind(item);
            card.make_clickable();
            row.append(&card);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .child(&row)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();
        super::track_scrolling(&scroll);
        section.append(&scroll);
        b.append(&section);
    }
    b
}

pub fn explore_page(win: &super::window::Window) -> adw::NavigationPage {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.add_css_class("explore");

    let tiles = gtk::FlowBox::new();
    tiles.set_selection_mode(gtk::SelectionMode::None);
    tiles.set_min_children_per_line(2);
    tiles.set_max_children_per_line(7);
    tiles.set_column_spacing(8);
    tiles.set_row_spacing(8);
    tiles.set_homogeneous(true);
    tiles.add_css_class("explore-tiles");
    content.append(&tiles);

    let body = gtk::Stack::new();
    body.set_transition_type(gtk::StackTransitionType::Crossfade);
    body.set_vhomogeneous(false);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    spinner.set_margin_top(48);
    body.add_named(&spinner, Some("loading"));
    let error = adw::StatusPage::builder()
        .icon_name("network-error-symbolic")
        .title("Couldn't load this category")
        .build();
    body.add_named(&error, Some("error"));
    content.append(&body);

    let cache: Rc<RefCell<std::collections::HashMap<String, gtk::Widget>>> = Rc::default();
    let buttons: Rc<RefCell<Vec<gtk::ToggleButton>>> = Rc::default();
    let select: Rc<dyn Fn(usize)> = {
        let body = body.downgrade();
        let error = error.downgrade();
        let cache = cache.clone();
        let buttons = buttons.clone();
        Rc::new(move |idx: usize| {
            let (Some(body), Some(error)) = (body.upgrade(), error.upgrade()) else { return };
            for (i, b) in buttons.borrow().iter().enumerate() {
                b.set_active(i == idx);
            }
            let (_, id, _, _) = DESTINATIONS[idx];
            if let Some(w) = cache.borrow().get(id) {
                body.set_visible_child(w);
                return;
            }
            body.set_visible_child_name("loading");
            let cache = cache.clone();
            let body_weak = body.downgrade();
            let error = error.downgrade();
            super::spawn(async move {
                let res = runtime::run(async move { api::client().shelves(id, None).await }).await;
                let (Some(body), Some(error)) = (body_weak.upgrade(), error.upgrade()) else { return };
                match res {
                    Ok(page) => {
                        let w = shelves_widget(&page).upcast::<gtk::Widget>();
                        body.add_child(&w);
                        body.set_visible_child(&w);
                        cache.borrow_mut().insert(id.to_owned(), w);
                    }
                    Err(e) => {
                        error.set_description(Some(&glib::markup_escape_text(&e.to_string())));
                        body.set_visible_child_name("error");
                    }
                }
            });
        })
    };
    for (i, (title, _, icon, class)) in DESTINATIONS.iter().enumerate() {
        let b = gtk::ToggleButton::new();
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let img = gtk::Image::from_icon_name(icon);
        img.set_pixel_size(24);
        let l = gtk::Label::new(Some(title));
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        inner.append(&img);
        inner.append(&l);
        b.set_child(Some(&inner));
        b.add_css_class("explore-tile");
        b.add_css_class(class);
        let select = select.clone();
        b.connect_clicked(move |btn| {
            if btn.is_active() {
                select(i);
            } else {
                btn.set_active(true);
            }
        });
        tiles.append(&b);
        buttons.borrow_mut().push(b);
    }

    let clamp = adw::Clamp::builder().maximum_size(1600).tightening_threshold(1200).child(&content).build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    super::track_scrolling(&scroll);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&super::pages::tab_header(win));
    tv.set_content(Some(&scroll));
    let page = adw::NavigationPage::builder().title("Explore").tag("explore").child(&tv).build();
    // Only fetch once the page is really on screen (not while it sits in a hidden tab).
    let loaded = std::cell::Cell::new(false);
    content.connect_map(move |_| {
        if !loaded.replace(true) {
            select(0);
        }
    });
    page
}
