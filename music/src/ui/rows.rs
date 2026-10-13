//! `RowList`: the virtualised list every page is built on.
//!
//! A page is one `GtkListView` over a list of `Row`s: its header, chips,
//! shelves (each a carousel or grid), songs, section titles and status rows.
//! Only rows near the viewport have widgets, and those are recycled while
//! scrolling, so a long playlist or a home feed costs the same few widgets.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use common::touch;
use gtk::{gio, glib};

use super::actions;
use super::cards::{ListRow, RowStyle};
use super::shelf::ShelfView;
use crate::api::{Item, Shelf, Track};
use crate::playback::Playback;

/// What activating a song row does.
pub struct TrackCtx {
    pub style: RowStyle,
    /// Called with the row's index; `None` plays the song's radio.
    pub on_activate: Option<Box<dyn Fn(usize)>>,
}

impl TrackCtx {
    pub fn radio(style: RowStyle) -> Rc<Self> {
        Rc::new(Self {
            style,
            on_activate: None,
        })
    }
}

#[derive(Clone)]
pub enum Row {
    /// A widget owned by the page (header, chips).
    Widget(gtk::Widget),
    Shelf(Rc<Shelf>),
    Track { track: Track, index: usize, ctx: Rc<TrackCtx> },
    Item(Item),
    Title(String),
    Spinner,
    Status { icon: String, title: String, description: String, retry: bool },
}

pub type PageFuture = Pin<Box<dyn Future<Output = anyhow::Result<(Vec<Row>, Option<String>)>>>>;
/// Loads the first page (`None`) or a continuation.
pub type Loader = Rc<dyn Fn(Option<String>) -> PageFuture>;

/// Song rows for a list of tracks sharing one context.
pub fn track_rows(tracks: &[Track], offset: usize, ctx: &Rc<TrackCtx>) -> Vec<Row> {
    tracks
        .iter()
        .enumerate()
        .map(|(i, t)| Row::Track {
            track: t.clone(),
            index: offset + i,
            ctx: ctx.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Row host: the recycled widget of one list item
// ---------------------------------------------------------------------------

mod host_imp {
    use super::*;

    #[derive(Default)]
    pub struct RowHost {
        pub clamp: adw::Clamp,
        pub list_row: RefCell<Option<ListRow>>,
        pub shelf: RefCell<Option<ShelfView>>,
        pub title: RefCell<Option<gtk::Label>>,
        pub spinner: RefCell<Option<gtk::Widget>>,
        pub status: RefCell<Option<(gtk::Box, gtk::Image, gtk::Label, gtk::Label, gtk::Button)>>,
        pub retry: RefCell<Option<Rc<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowHost {
        const NAME: &'static str = "StmRowHost";
        type Type = super::RowHost;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("rowhost");
        }
    }

    impl ObjectImpl for RowHost {
        fn constructed(&self) {
            self.parent_constructed();
            self.clamp.set_maximum_size(1400);
            self.clamp.set_tightening_threshold(1200);
            self.clamp.set_parent(&*self.obj());
        }

        fn dispose(&self) {
            self.clamp.unparent();
        }
    }

    impl WidgetImpl for RowHost {}
}

glib::wrapper! {
    pub struct RowHost(ObjectSubclass<host_imp::RowHost>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl RowHost {
    fn new() -> Self {
        glib::Object::new()
    }

    fn show(&self, w: &impl IsA<gtk::Widget>) {
        let w = w.upcast_ref::<gtk::Widget>();
        let clamp = &self.imp().clamp;
        if clamp.child().as_ref() == Some(w) {
            return;
        }
        // A page-owned widget may still sit in the host that showed it before.
        if let Some(parent) = w.parent()
            && let Some(other) = parent.downcast_ref::<adw::Clamp>() {
                other.set_child(None::<&gtk::Widget>);
            }
        clamp.set_child(Some(w));
    }

    fn bind(&self, row: &Row, retry: Rc<dyn Fn()>) {
        let imp = self.imp();
        self.remove_css_class("shelf-host");
        match row {
            Row::Widget(w) => self.show(w),
            Row::Shelf(s) => {
                let view = imp.shelf.borrow_mut().get_or_insert_with(ShelfView::new).clone();
                view.bind(s);
                self.add_css_class("shelf-host");
                self.show(&view);
            }
            Row::Track { track, ctx, .. } => {
                let r = imp.list_row.borrow_mut().get_or_insert_with(ListRow::new).clone();
                r.bind_track(track, ctx.style);
                self.show(&r);
            }
            Row::Item(item) => {
                let r = imp.list_row.borrow_mut().get_or_insert_with(ListRow::new).clone();
                r.bind_item(item, RowStyle::Art);
                self.show(&r);
            }
            Row::Title(t) => {
                let l = imp
                    .title
                    .borrow_mut()
                    .get_or_insert_with(|| {
                        let l = gtk::Label::new(None);
                        l.set_xalign(0.0);
                        l.add_css_class("section-title");
                        l
                    })
                    .clone();
                l.set_text(t);
                self.show(&l);
            }
            Row::Spinner => {
                let s = imp
                    .spinner
                    .borrow_mut()
                    .get_or_insert_with(|| {
                        let s = adw::Spinner::new();
                        s.set_size_request(32, 32);
                        s.set_halign(gtk::Align::Center);
                        s.set_valign(gtk::Align::Center);
                        let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
                        b.set_height_request(128);
                        b.append(&s);
                        b.upcast()
                    })
                    .clone();
                self.show(&s);
            }
            Row::Status {
                icon,
                title,
                description,
                retry: show_retry,
            } => {
                let (b, i, t, d, r) = imp
                    .status
                    .borrow_mut()
                    .get_or_insert_with(|| {
                        let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
                        b.add_css_class("inline-status");
                        let i = gtk::Image::new();
                        i.set_pixel_size(64);
                        i.add_css_class("dimmed");
                        let t = gtk::Label::new(None);
                        t.add_css_class("title-2");
                        t.set_wrap(true);
                        t.set_justify(gtk::Justification::Center);
                        let d = gtk::Label::new(None);
                        d.set_wrap(true);
                        d.set_justify(gtk::Justification::Center);
                        d.add_css_class("dimmed");
                        let r = gtk::Button::with_label("Retry");
                        r.add_css_class("pill");
                        r.set_halign(gtk::Align::Center);
                        let host = self.downgrade();
                        r.connect_clicked(move |_| {
                            let f = host.upgrade().and_then(|h| h.imp().retry.borrow().clone());
                            if let Some(f) = f {
                                f();
                            }
                        });
                        b.append(&i);
                        b.append(&t);
                        b.append(&d);
                        b.append(&r);
                        (b, i, t, d, r)
                    })
                    .clone();
                i.set_icon_name(Some(icon));
                t.set_text(title);
                d.set_text(description);
                d.set_visible(!description.is_empty());
                r.set_visible(*show_retry);
                imp.retry.replace(Some(retry));
                self.show(&b);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RowList
// ---------------------------------------------------------------------------

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RowList {
        pub model: RefCell<Option<gio::ListStore>>,
        pub list: RefCell<Option<gtk::ListView>>,
        pub scrolled: gtk::ScrolledWindow,
        pub prefix: Cell<u32>,
        pub loader: RefCell<Option<Loader>>,
        pub continuation: RefCell<Option<String>>,
        pub loading: Cell<bool>,
        pub generation: Cell<u64>,
        pub loaded: Cell<bool>,
        pub empty: RefCell<(String, String, String)>,
        pub on_loaded: RefCell<Vec<Box<dyn Fn(bool)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowList {
        const NAME: &'static str = "StmRowList";
        type Type = super::RowList;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for RowList {}
    impl WidgetImpl for RowList {}
    impl BoxImpl for RowList {}
}

glib::wrapper! {
    pub struct RowList(ObjectSubclass<imp::RowList>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for RowList {
    fn default() -> Self {
        Self::new()
    }
}

impl RowList {
    pub fn new() -> Self {
        let obj: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .build();
        obj.build();
        obj
    }

    fn build(&self) {
        let imp = self.imp();
        self.set_vexpand(true);
        imp.empty.replace((
            "sloptube-library-symbolic".into(),
            "Nothing here".into(),
            String::new(),
        ));
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
            li.set_child(Some(&RowHost::new()));
        });
        let weak = self.downgrade();
        factory.connect_bind(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
            let (Some(host), Some(item)) = (
                li.child().and_downcast::<RowHost>(),
                li.item().and_downcast::<glib::BoxedAnyObject>(),
            ) else {
                return;
            };
            let row = item.borrow::<Row>().clone();
            let activatable = matches!(row, Row::Track { .. } | Row::Item(_));
            li.set_activatable(activatable);
            li.set_selectable(false);
            li.set_focusable(activatable);
            let weak = weak.clone();
            host.bind(
                &row,
                Rc::new(move || {
                    if let Some(l) = weak.upgrade() {
                        l.reload();
                    }
                }),
            );
        });
        let selection = gtk::NoSelection::new(Some(model.clone()));
        let list = gtk::ListView::new(Some(selection), Some(factory));
        list.set_single_click_activate(true);
        list.add_css_class("row-list");
        list.connect_activate(|list, pos| {
            // A tap that only stopped the list gliding doesn't play or open anything.
            if touch::press_stopped_scrolling(list) {
                return;
            }
            let Some(obj) = list.model().and_then(|m| m.item(pos)).and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let row = obj.borrow::<Row>().clone();
            match row {
                Row::Track { track, index, ctx } => match &ctx.on_activate {
                    Some(f) => f(index),
                    None => Playback::get().play_song(track),
                },
                Row::Item(item) => actions::activate(list, &item),
                _ => {}
            }
        });
        imp.scrolled.set_child(Some(&list));
        touch::track_scrolling(&imp.scrolled);
        imp.scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        imp.scrolled.set_vexpand(true);
        let adj = imp.scrolled.vadjustment();
        adj.connect_value_changed(glib::clone!(
            #[weak(rename_to = rl)]
            self,
            move |adj| rl.maybe_load_more(adj)
        ));
        adj.connect_changed(glib::clone!(
            #[weak(rename_to = rl)]
            self,
            move |adj| rl.maybe_load_more(adj)
        ));
        self.append(&imp.scrolled);
        imp.model.replace(Some(model));
        imp.list.replace(Some(list));
    }

    fn model(&self) -> gio::ListStore {
        self.imp().model.borrow().clone().unwrap()
    }

    pub fn vadjustment(&self) -> gtk::Adjustment {
        self.imp().scrolled.vadjustment()
    }

    /// Report the content's height as natural height (lists inside sheets).
    pub fn set_propagate_natural_height(&self, propagate: bool) {
        self.imp().scrolled.set_propagate_natural_height(propagate);
    }

    pub fn scroll_to_top(&self) {
        self.vadjustment().set_value(0.0);
    }

    /// Rows kept above the loaded content (a header, chips).
    pub fn set_prefix(&self, rows: Vec<Row>) {
        let imp = self.imp();
        let model = self.model();
        let objs: Vec<glib::BoxedAnyObject> = rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        model.splice(0, imp.prefix.get(), &objs);
        imp.prefix.set(objs.len() as u32);
    }

    /// Replace the content below the prefix.
    pub fn set_rows(&self, rows: Vec<Row>) {
        let imp = self.imp();
        let model = self.model();
        let objs: Vec<glib::BoxedAnyObject> = rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        let prefix = imp.prefix.get();
        model.splice(prefix, model.n_items() - prefix, &objs);
    }

    pub fn append_rows(&self, rows: Vec<Row>) {
        let objs: Vec<glib::BoxedAnyObject> = rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.model().extend_from_slice(&objs);
    }

    /// Re-bind every row (e.g. after the layout changed between phone and desktop).
    pub fn rebind(&self) {
        let model = self.model();
        let n = model.n_items();
        model.items_changed(0, n, n);
    }

    pub fn set_empty_state(&self, icon: &str, title: &str, description: &str) {
        self.imp()
            .empty
            .replace((icon.to_owned(), title.to_owned(), description.to_owned()));
    }

    pub fn set_loader(&self, loader: Loader) {
        self.imp().loader.replace(Some(loader));
    }

    /// Called after a load: `true` for the first page.
    pub fn connect_loaded(&self, f: impl Fn(bool) + 'static) {
        self.imp().on_loaded.borrow_mut().push(Box::new(f));
    }

    pub fn is_loaded(&self) -> bool {
        self.imp().loaded.get()
    }

    pub fn ensure_loaded(&self) {
        let imp = self.imp();
        if !imp.loaded.get() && !imp.loading.get() {
            self.reload();
        }
    }

    pub fn reload(&self) {
        self.load(None, true);
    }

    fn maybe_load_more(&self, adj: &gtk::Adjustment) {
        if adj.page_size() <= 0.0 || adj.upper() <= 0.0 || !self.is_mapped() {
            return;
        }
        if adj.value() + adj.page_size() * 2.5 >= adj.upper() {
            let token = self.imp().continuation.borrow().clone();
            if let Some(t) = token
                && !self.imp().loading.get() {
                    self.load(Some(t), false);
                }
        }
    }

    fn load(&self, token: Option<String>, replace: bool) {
        let imp = self.imp();
        let Some(loader) = imp.loader.borrow().clone() else { return };
        let ticket = imp.generation.get() + 1;
        imp.generation.set(ticket);
        imp.loading.set(true);
        if replace {
            imp.continuation.replace(None);
            self.set_rows(vec![Row::Spinner]);
        } else {
            self.append_rows(vec![Row::Spinner]);
        }
        let fut = loader(token);
        let rl = self.downgrade();
        glib::spawn_future_local(async move {
            let res = fut.await;
            let Some(rl) = rl.upgrade() else { return };
            let imp = rl.imp();
            if imp.generation.get() != ticket {
                return;
            }
            imp.loading.set(false);
            if !replace {
                // Drop the trailing spinner.
                let model = rl.model();
                if model.n_items() > imp.prefix.get() {
                    model.remove(model.n_items() - 1);
                }
            }
            match res {
                Ok((rows, cont)) => {
                    let empty = rows.is_empty() && replace;
                    if replace {
                        if empty {
                            let (icon, title, desc) = imp.empty.borrow().clone();
                            rl.set_rows(vec![Row::Status {
                                icon,
                                title,
                                description: desc,
                                retry: false,
                            }]);
                        } else {
                            rl.set_rows(rows);
                        }
                    } else {
                        rl.append_rows(rows);
                    }
                    imp.continuation.replace(cont);
                    imp.loaded.set(true);
                    for f in imp.on_loaded.borrow().iter() {
                        f(replace);
                    }
                    // The rows went in before the token: check again whether the
                    // page is short enough to need the next part right away.
                    let weak = rl.downgrade();
                    glib::idle_add_local_once(move || {
                        if let Some(rl) = weak.upgrade() {
                            rl.maybe_load_more(&rl.vadjustment());
                        }
                    });
                }
                Err(e) => {
                    log::warn!("page load failed: {e:#}");
                    if replace {
                        rl.set_rows(vec![Row::Status {
                            icon: "network-error-symbolic".into(),
                            title: "Couldn't load this page".into(),
                            description: format!("{e}"),
                            retry: true,
                        }]);
                    } else {
                        super::toast(&format!("Couldn't load more: {e}"));
                    }
                }
            }
        });
    }
}
