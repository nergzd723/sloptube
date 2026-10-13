//! Pages of shelves: Home (with mood chips), Explore, moods & genres,
//! charts, new releases and the "More" pages of shelves.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use common::touch;
use gtk::{gio, glib};

use super::cards::{RowStyle, TileCard};
use super::rows::{Loader, Row, RowList, TrackCtx};
use super::shelf::{TILE, tile_size};
use crate::api::{self, BrowseTarget, Chip, Item, Shelf, ShelfKind};
use crate::runtime;

/// A grid this big gets a virtualised grid view of its own.
const BIG_GRID: usize = 18;

/// Turn shelves into list rows. Plain lists become rows of their own, so
/// long result lists are virtualised like everything else.
pub fn shelves_to_rows(shelves: Vec<Shelf>) -> Vec<Row> {
    let mut rows = Vec::new();
    for s in shelves {
        if s.kind == ShelfKind::List && s.more.is_none() {
            if !s.title.is_empty() {
                rows.push(Row::Title(s.title.clone()));
            }
            let ctx = TrackCtx::radio(RowStyle::Art);
            for (i, item) in s.items.into_iter().enumerate() {
                rows.push(match item {
                    Item::Track(track) => Row::Track {
                        track,
                        index: i,
                        ctx: ctx.clone(),
                    },
                    other => Row::Item(other),
                });
            }
        } else {
            rows.push(Row::Shelf(Rc::new(s)));
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// Chips
// ---------------------------------------------------------------------------

mod chips_imp {
    use super::*;

    #[derive(Default)]
    pub struct ChipBar {
        pub row: gtk::Box,
        pub on_select: RefCell<Option<Rc<dyn Fn(Option<Chip>)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChipBar {
        const NAME: &'static str = "StmChipBar";
        type Type = super::ChipBar;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for ChipBar {}
    impl WidgetImpl for ChipBar {}
    impl BinImpl for ChipBar {}
}

glib::wrapper! {
    /// A sideways-scrolling row of filter chips (moods on Home, result types in Search).
    pub struct ChipBar(ObjectSubclass<chips_imp::ChipBar>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ChipBar {
    fn default() -> Self {
        Self::new()
    }
}

impl ChipBar {
    pub fn new() -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.row.set_spacing(8);
        imp.row.add_css_class("chips");
        let sw = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&imp.row)
            .build();
        touch::track_scrolling(&sw);
        obj.set_child(Some(&sw));
        obj
    }

    pub fn connect_select(&self, f: impl Fn(Option<Chip>) + 'static) {
        self.imp().on_select.replace(Some(Rc::new(f)));
    }

    /// Show chips; `selected` names the chip to mark when the response doesn't say.
    pub fn set_chips(&self, chips: &[Chip], selected: Option<&str>) {
        let imp = self.imp();
        super::clear(&imp.row);
        let any_selected = chips.iter().any(|c| c.selected);
        for chip in chips {
            let b = gtk::ToggleButton::with_label(&chip.title);
            b.add_css_class("chip");
            b.set_active(chip.selected || (!any_selected && selected == Some(chip.title.as_str())));
            let chip = chip.clone();
            b.connect_clicked(glib::clone!(
                #[weak(rename_to = bar)]
                self,
                move |btn| {
                    let active = btn.is_active();
                    let mut c = bar.imp().row.first_child();
                    while let Some(w) = c {
                        if let Some(t) = w.downcast_ref::<gtk::ToggleButton>()
                            && t != btn {
                                t.set_active(false);
                            }
                        c = w.next_sibling();
                    }
                    let f = bar.imp().on_select.borrow().clone();
                    if let Some(f) = f {
                        f(active.then(|| chip.clone()));
                    }
                }
            ));
            imp.row.append(&b);
        }
        self.set_visible(!chips.is_empty());
    }
}

// ---------------------------------------------------------------------------
// Tile grid
// ---------------------------------------------------------------------------

/// A virtualised grid of tiles (new releases, discographies).
pub fn tile_grid(items: Vec<Item>) -> gtk::ScrolledWindow {
    let size = tile_size(&items);
    let model = gio::ListStore::new::<glib::BoxedAnyObject>();
    let objs: Vec<glib::BoxedAnyObject> = items.into_iter().map(glib::BoxedAnyObject::new).collect();
    model.extend_from_slice(&objs);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        li.set_child(Some(&TileCard::new(TILE)));
        li.set_activatable(false);
        li.set_selectable(false);
    });
    factory.connect_bind(move |_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(tile), Some(item)) = (
            li.child().and_downcast::<TileCard>(),
            li.item().and_downcast::<glib::BoxedAnyObject>(),
        ) {
            tile.bind(&item.borrow::<Item>(), size);
        }
    });
    let grid = gtk::GridView::new(Some(gtk::NoSelection::new(Some(model))), Some(factory));
    grid.set_min_columns(2);
    grid.set_max_columns(10);
    grid.add_css_class("tile-gridview");
    let sw = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&grid)
        .vexpand(true)
        .build();
    touch::track_scrolling(&sw);
    sw
}

// ---------------------------------------------------------------------------
// BrowseView
// ---------------------------------------------------------------------------

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct BrowseView {
        pub target: RefCell<BrowseTarget>,
        pub params: RefCell<Option<String>>,
        pub chip_title: RefCell<Option<String>>,
        pub stack: gtk::Stack,
        pub rows: RowList,
        pub chips: RefCell<Option<ChipBar>>,
        pub title: RefCell<Option<Box<dyn Fn(&str)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for BrowseView {
        const NAME: &'static str = "StmBrowseView";
        type Type = super::BrowseView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for BrowseView {}
    impl WidgetImpl for BrowseView {}
    impl BinImpl for BrowseView {}
}

glib::wrapper! {
    pub struct BrowseView(ObjectSubclass<imp::BrowseView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BrowseView {
    pub fn new(target: BrowseTarget, with_chips: bool) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.params.replace(target.params.clone());
        imp.target.replace(target);
        imp.stack.add_named(&imp.rows, Some("rows"));
        imp.stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        obj.set_child(Some(&imp.stack));
        if with_chips {
            let chips = ChipBar::new();
            chips.set_visible(false);
            chips.connect_select(glib::clone!(
                #[weak]
                obj,
                move |chip| {
                    let imp = obj.imp();
                    imp.chip_title.replace(chip.as_ref().map(|c| c.title.clone()));
                    let base = imp.target.borrow().params.clone();
                    imp.params.replace(chip.and_then(|c| c.params).or(base));
                    imp.rows.scroll_to_top();
                    imp.rows.reload();
                }
            ));
            imp.rows.set_prefix(vec![Row::Widget(chips.clone().upcast())]);
            imp.chips.replace(Some(chips));
        }
        imp.rows.set_loader(obj.loader());
        obj
    }

    /// Called with the page's own title once it's known.
    pub fn connect_title(&self, f: impl Fn(&str) + 'static) {
        self.imp().title.replace(Some(Box::new(f)));
    }

    pub fn ensure_loaded(&self) {
        self.imp().rows.ensure_loaded();
    }

    pub fn reload(&self) {
        self.imp().rows.reload();
    }

    pub fn rows(&self) -> RowList {
        self.imp().rows.clone()
    }

    fn loader(&self) -> Loader {
        let weak = self.downgrade();
        Rc::new(move |token| {
            let weak = weak.clone();
            Box::pin(async move {
                let Some(view) = weak.upgrade() else {
                    return Ok((Vec::new(), None));
                };
                if let Some(t) = token {
                    let page = runtime::run(async move { api::client().browse_more(&t).await }).await?;
                    return Ok((shelves_to_rows(page.shelves), page.continuation));
                }
                let imp = view.imp();
                let id = imp.target.borrow().id.clone();
                let params = imp.params.borrow().clone();
                drop(view);
                let page = runtime::run(async move { api::client().browse_page(&id, params.as_deref()).await }).await?;
                let Some(view) = weak.upgrade() else {
                    return Ok((Vec::new(), None));
                };
                let imp = view.imp();
                if let Some(chips) = imp.chips.borrow().as_ref() {
                    chips.set_chips(&page.chips, imp.chip_title.borrow().as_deref());
                }
                if let (Some(t), Some(f)) = (page.title.as_deref(), imp.title.borrow().as_ref()) {
                    f(t);
                }
                let big_grid = page.shelves.len() == 1
                    && page.shelves[0].kind == ShelfKind::Grid
                    && page.shelves[0].items.len() > BIG_GRID;
                if big_grid {
                    let items = page.shelves.into_iter().next().unwrap().items;
                    if let Some(old) = imp.stack.child_by_name("grid") {
                        imp.stack.remove(&old);
                    }
                    imp.stack.add_named(&tile_grid(items), Some("grid"));
                    imp.stack.set_visible_child_name("grid");
                    return Ok((Vec::new(), None));
                }
                imp.stack.set_visible_child_name("rows");
                Ok((shelves_to_rows(page.shelves), page.continuation))
            })
        })
    }
}
