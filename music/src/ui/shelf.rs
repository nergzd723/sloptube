//! One shelf of a page: a carousel of tiles, columns of songs ("Quick
//! picks"), a list, a grid, mood buttons or a block of text.
//!
//! Shelf views are recycled by the row list, and keep their tiles and rows
//! in pools so rebinding mostly just updates labels and artwork.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use common::touch;
use gtk::glib;

use super::cards::{ListRow, MoodButton, RowStyle, TileCard};
use super::{Nav, actions, navigate};
use crate::api::{Item, Shelf, ShelfKind};

pub const TILE: i32 = 164;
/// Tiles of shelves that only hold videos are 16:9.
pub const WIDE_TILE: i32 = 272;

/// One artwork size for every tile of a shelf: square (videos cropped), or
/// 16:9 when everything is a video. Mixed sizes made carousels look ragged.
pub fn tile_size(items: &[Item]) -> (i32, i32) {
    let all_videos = !items.is_empty() && items.iter().all(|i| matches!(i, Item::Track(t) if t.is_video));
    if all_videos {
        (WIDE_TILE, WIDE_TILE * 9 / 16)
    } else {
        (TILE, TILE)
    }
}
const COLUMN_WIDTH: i32 = 330;
const COLUMN_ROWS: usize = 4;
/// Long lists inside a shelf show this many items (the rest is behind "More").
const LIST_MAX: usize = 6;

#[derive(Default)]
pub struct Pools {
    carousel: Option<(gtk::ScrolledWindow, gtk::Box)>,
    tiles: Vec<TileCard>,
    columns: Option<(gtk::ScrolledWindow, gtk::Box)>,
    column_boxes: Vec<gtk::Box>,
    column_rows: Vec<ListRow>,
    list: Option<gtk::Box>,
    list_rows: Vec<ListRow>,
    grid: Option<gtk::FlowBox>,
    grid_tiles: Vec<TileCard>,
    moods: Option<gtk::FlowBox>,
    mood_buttons: Vec<MoodButton>,
    text: Option<(gtk::Box, gtk::Label, gtk::Button)>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ShelfView {
        pub shelf: RefCell<Option<Rc<Shelf>>>,
        pub header: gtk::Box,
        pub strapline: gtk::Label,
        pub title: gtk::Label,
        pub more: gtk::Button,
        pub prev: gtk::Button,
        pub next: gtk::Button,
        pub content: adw::Bin,
        pub pools: RefCell<Pools>,
        pub scroller: RefCell<Option<gtk::ScrolledWindow>>,
        pub scroll_anim: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ShelfView {
        const NAME: &'static str = "StmShelfView";
        type Type = super::ShelfView;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for ShelfView {}
    impl WidgetImpl for ShelfView {}
    impl BoxImpl for ShelfView {}
}

glib::wrapper! {
    pub struct ShelfView(ObjectSubclass<imp::ShelfView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for ShelfView {
    fn default() -> Self {
        Self::new()
    }
}

fn hscroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .build();
    sw.add_css_class("shelf-scroller");
    touch::track_scrolling(&sw);
    sw
}

impl ShelfView {
    pub fn new() -> Self {
        let obj: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .property("spacing", 10)
            .build();
        obj.build();
        obj
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("shelf");
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
        titles.set_hexpand(true);
        titles.set_valign(gtk::Align::End);
        imp.strapline.set_xalign(0.0);
        imp.strapline.add_css_class("shelf-strapline");
        imp.title.set_xalign(0.0);
        imp.title.set_wrap(true);
        imp.title.add_css_class("shelf-title");
        titles.append(&imp.strapline);
        titles.append(&imp.title);
        imp.header.add_css_class("shelf-header");
        imp.header.set_spacing(6);
        imp.header.append(&titles);

        imp.more.set_label("More");
        imp.more.add_css_class("pill-small");
        imp.more.set_valign(gtk::Align::Center);
        imp.more.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let shelf = view.imp().shelf.borrow().clone();
                if let Some(s) = shelf
                    && let Some(t) = s.more.clone() {
                        navigate(&view, Nav::Page(t, s.title.clone()));
                    }
            }
        ));
        imp.header.append(&imp.more);
        for (b, icon, dir) in [(&imp.prev, "go-previous-symbolic", -1.0), (&imp.next, "go-next-symbolic", 1.0)] {
            b.set_icon_name(icon);
            b.add_css_class("circular");
            b.add_css_class("shelf-arrow");
            b.set_valign(gtk::Align::Center);
            b.connect_clicked(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| view.scroll_by(dir)
            ));
            imp.header.append(b);
        }
        self.append(&imp.header);
        self.append(&imp.content);
        super::on_layout(self, glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.update_arrows()
        ));
    }

    fn scroll_by(&self, dir: f64) {
        let imp = self.imp();
        let Some(sw) = imp.scroller.borrow().clone() else { return };
        let adj = sw.hadjustment();
        let to = (adj.value() + dir * adj.page_size() * 0.85).clamp(adj.lower(), adj.upper() - adj.page_size());
        if let Some(a) = imp.scroll_anim.take() {
            a.skip();
        }
        let target = adw::PropertyAnimationTarget::new(&adj, "value");
        let anim = adw::TimedAnimation::new(self, adj.value(), to, 350, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        anim.play();
        imp.scroll_anim.replace(Some(anim));
    }

    fn update_arrows(&self) {
        let imp = self.imp();
        let Some(sw) = imp.scroller.borrow().clone() else {
            imp.prev.set_visible(false);
            imp.next.set_visible(false);
            return;
        };
        let adj = sw.hadjustment();
        // Touch screens swipe; the arrows are for pointers on the desktop layout.
        let narrow = super::main_window().is_some_and(|w| w.narrow());
        let scrollable = !narrow && adj.upper() - adj.page_size() > 1.0;
        imp.prev.set_visible(scrollable);
        imp.next.set_visible(scrollable);
        imp.prev.set_sensitive(adj.value() > adj.lower() + 1.0);
        imp.next.set_sensitive(adj.value() < adj.upper() - adj.page_size() - 1.0);
    }

    fn watch_scroller(&self, sw: &gtk::ScrolledWindow) {
        let adj = sw.hadjustment();
        for signal in ["changed", "value-changed"] {
            adj.connect_local(
                signal,
                false,
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    #[upgrade_or]
                    None,
                    move |_| {
                        view.update_arrows();
                        None
                    }
                ),
            );
        }
    }

    pub fn bind(&self, shelf: &Rc<Shelf>) {
        let imp = self.imp();
        imp.shelf.replace(Some(shelf.clone()));
        imp.title.set_text(&shelf.title);
        imp.title.set_visible(!shelf.title.is_empty());
        imp.strapline.set_text(shelf.strapline.as_deref().unwrap_or(""));
        imp.strapline.set_visible(shelf.strapline.is_some());
        imp.more.set_visible(shelf.more.is_some());
        imp.header.set_visible(!shelf.title.is_empty() || shelf.more.is_some());
        imp.scroller.replace(None);
        match shelf.kind {
            ShelfKind::Carousel => self.bind_carousel(shelf),
            ShelfKind::TrackColumns => self.bind_columns(shelf),
            ShelfKind::List => self.bind_list(shelf),
            ShelfKind::Grid => self.bind_grid(shelf),
            ShelfKind::Moods => self.bind_moods(shelf),
            ShelfKind::Text => self.bind_text(shelf),
        }
        if let Some(sw) = imp.scroller.borrow().as_ref() {
            sw.hadjustment().set_value(0.0);
        }
        self.update_arrows();
    }

    fn bind_carousel(&self, shelf: &Shelf) {
        let imp = self.imp();
        let mut pools = imp.pools.borrow_mut();
        if pools.carousel.is_none() {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            b.add_css_class("carousel");
            let sw = hscroller(&b);
            self.watch_scroller(&sw);
            pools.carousel = Some((sw, b));
        }
        let (sw, b) = pools.carousel.clone().unwrap();
        let size = tile_size(&shelf.items);
        while pools.tiles.len() < shelf.items.len() {
            let t = TileCard::new(TILE);
            b.append(&t);
            pools.tiles.push(t);
        }
        for (i, tile) in pools.tiles.iter().enumerate() {
            match shelf.items.get(i) {
                Some(item) => {
                    tile.bind(item, size);
                    tile.set_visible(true);
                }
                None => tile.set_visible(false),
            }
        }
        drop(pools);
        imp.content.set_child(Some(&sw));
        imp.scroller.replace(Some(sw));
    }

    fn bind_columns(&self, shelf: &Shelf) {
        let imp = self.imp();
        let mut pools = imp.pools.borrow_mut();
        if pools.columns.is_none() {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            b.add_css_class("track-columns");
            let sw = hscroller(&b);
            self.watch_scroller(&sw);
            pools.columns = Some((sw, b));
        }
        let (sw, b) = pools.columns.clone().unwrap();
        let n_cols = shelf.items.len().div_ceil(COLUMN_ROWS);
        while pools.column_boxes.len() < n_cols {
            let c = gtk::Box::new(gtk::Orientation::Vertical, 0);
            c.set_size_request(COLUMN_WIDTH, -1);
            b.append(&c);
            pools.column_boxes.push(c);
        }
        while pools.column_rows.len() < shelf.items.len() {
            let r = ListRow::new();
            r.add_css_class("compact");
            let weak = r.downgrade();
            r.set_on_click(move || {
                if let Some(r) = weak.upgrade()
                    && let Some(item) = r.item() {
                        actions::activate(&r, &item);
                    }
            });
            pools.column_rows.push(r);
        }
        for (i, row) in pools.column_rows.iter().enumerate() {
            let col = &pools.column_boxes[(i / COLUMN_ROWS).min(pools.column_boxes.len() - 1)];
            if row.parent().as_ref() != Some(col.upcast_ref()) {
                if let Some(p) = row.parent() {
                    p.downcast_ref::<gtk::Box>().unwrap().remove(row);
                }
                col.append(row);
            }
            match shelf.items.get(i) {
                Some(item) => {
                    row.bind_item(item, RowStyle::Art);
                    row.set_visible(true);
                }
                None => row.set_visible(false),
            }
        }
        for (i, c) in pools.column_boxes.iter().enumerate() {
            c.set_visible(i < n_cols);
        }
        drop(pools);
        imp.content.set_child(Some(&sw));
        imp.scroller.replace(Some(sw));
    }

    fn bind_list(&self, shelf: &Shelf) {
        let imp = self.imp();
        let mut pools = imp.pools.borrow_mut();
        let b = pools
            .list
            .get_or_insert_with(|| {
                let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
                b.add_css_class("shelf-list");
                b
            })
            .clone();
        let n = if shelf.more.is_some() { shelf.items.len().min(LIST_MAX) } else { shelf.items.len() };
        while pools.list_rows.len() < n {
            let r = ListRow::new();
            let weak = r.downgrade();
            r.set_on_click(move || {
                if let Some(r) = weak.upgrade()
                    && let Some(item) = r.item() {
                        actions::activate(&r, &item);
                    }
            });
            b.append(&r);
            pools.list_rows.push(r);
        }
        for (i, row) in pools.list_rows.iter().enumerate() {
            match shelf.items.get(i).filter(|_| i < n) {
                Some(item) => {
                    row.bind_item(item, RowStyle::Art);
                    row.set_visible(true);
                }
                None => row.set_visible(false),
            }
        }
        drop(pools);
        imp.content.set_child(Some(&b));
    }

    fn bind_grid(&self, shelf: &Shelf) {
        let imp = self.imp();
        let mut pools = imp.pools.borrow_mut();
        let grid = pools
            .grid
            .get_or_insert_with(|| {
                let g = gtk::FlowBox::new();
                g.set_selection_mode(gtk::SelectionMode::None);
                g.set_homogeneous(true);
                g.set_min_children_per_line(2);
                g.set_max_children_per_line(12);
                g.set_column_spacing(4);
                g.set_row_spacing(12);
                g.add_css_class("tile-grid");
                g
            })
            .clone();
        let size = tile_size(&shelf.items);
        while pools.grid_tiles.len() < shelf.items.len() {
            let t = TileCard::new(TILE);
            grid.append(&t);
            pools.grid_tiles.push(t);
        }
        for (i, tile) in pools.grid_tiles.iter().enumerate() {
            let child = tile.parent();
            match shelf.items.get(i) {
                Some(item) => {
                    tile.bind(item, size);
                    if let Some(c) = child { c.set_visible(true) }
                }
                None => {
                    if let Some(c) = child { c.set_visible(false) }
                }
            }
        }
        drop(pools);
        imp.content.set_child(Some(&grid));
    }

    fn bind_moods(&self, shelf: &Shelf) {
        let imp = self.imp();
        let mut pools = imp.pools.borrow_mut();
        let flow = pools
            .moods
            .get_or_insert_with(|| {
                let g = gtk::FlowBox::new();
                g.set_selection_mode(gtk::SelectionMode::None);
                g.set_homogeneous(true);
                g.set_min_children_per_line(2);
                g.set_max_children_per_line(6);
                g.set_column_spacing(10);
                g.set_row_spacing(10);
                g.add_css_class("mood-grid");
                g
            })
            .clone();
        let moods: Vec<_> = shelf
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Mood(m) => Some(m.clone()),
                _ => None,
            })
            .collect();
        while pools.mood_buttons.len() < moods.len() {
            let b = MoodButton::new();
            flow.append(&b);
            pools.mood_buttons.push(b);
        }
        for (i, b) in pools.mood_buttons.iter().enumerate() {
            let child = b.parent();
            match moods.get(i) {
                Some(m) => {
                    b.bind(m);
                    if let Some(c) = child { c.set_visible(true) }
                }
                None => {
                    if let Some(c) = child { c.set_visible(false) }
                }
            }
        }
        drop(pools);
        imp.content.set_child(Some(&flow));
    }

    fn bind_text(&self, shelf: &Shelf) {
        let imp = self.imp();
        let mut pools = imp.pools.borrow_mut();
        let (b, label, toggle) = pools
            .text
            .get_or_insert_with(|| {
                let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
                let l = gtk::Label::new(None);
                l.set_wrap(true);
                l.set_xalign(0.0);
                l.set_selectable(true);
                l.add_css_class("shelf-text");
                touch::route_label_touch(&l, |_, _, _| {});
                let t = gtk::Button::with_label("Show more");
                t.add_css_class("flat");
                t.set_halign(gtk::Align::Start);
                t.connect_clicked(glib::clone!(
                    #[weak]
                    l,
                    move |btn| {
                        let expanded = l.lines() < 0;
                        l.set_lines(if expanded { 4 } else { -1 });
                        l.set_ellipsize(if expanded {
                            gtk::pango::EllipsizeMode::End
                        } else {
                            gtk::pango::EllipsizeMode::None
                        });
                        btn.set_label(if expanded { "Show more" } else { "Show less" });
                    }
                ));
                b.append(&l);
                b.append(&t);
                (b, l, t)
            })
            .clone();
        let text = shelf.text.clone().unwrap_or_default();
        label.set_text(&text);
        label.set_lines(4);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        toggle.set_label("Show more");
        toggle.set_visible(text.lines().count() > 4 || text.len() > 320);
        drop(pools);
        imp.content.set_child(Some(&b));
    }
}
