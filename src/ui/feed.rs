//! A paginated, virtualised grid (or list) of feed items with optional chips.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};

use super::card::{CardStyle, ItemCard};
use crate::api::{Chip, FeedPage, Item};

pub type LoadFuture = Pin<Box<dyn Future<Output = anyhow::Result<FeedPage>>>>;
/// Loads the first page (`None`) or a continuation/chip token.
pub type Loader = Rc<dyn Fn(Option<String>) -> LoadFuture>;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FeedView {
        pub style: Cell<CardStyle>,
        pub model: RefCell<Option<gio::ListStore>>,
        pub view: RefCell<Option<gtk::Widget>>,
        pub scrolled: gtk::ScrolledWindow,
        pub stack: gtk::Stack,
        pub status: adw::StatusPage,
        pub retry: gtk::Button,
        pub chips_box: gtk::Box,
        pub chips_revealer: gtk::Revealer,
        pub more_spinner: gtk::Revealer,
        pub loader: RefCell<Option<Loader>>,
        pub continuation: RefCell<Option<String>>,
        pub loading: Cell<bool>,
        pub generation: Cell<u64>,
        pub loaded_once: Cell<bool>,
        pub empty_title: RefCell<String>,
        pub empty_icon: RefCell<String>,
        pub on_page: RefCell<Vec<Box<dyn Fn(&FeedPage, bool)>>>,
        /// Items of a further page still waiting to be appended (about one per frame).
        pub pending: RefCell<std::collections::VecDeque<glib::BoxedAnyObject>>,
        pub draining: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FeedView {
        const NAME: &'static str = "SlopFeedView";
        type Type = super::FeedView;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for FeedView {}
    impl WidgetImpl for FeedView {}
    impl BoxImpl for FeedView {}
}

glib::wrapper! {
    pub struct FeedView(ObjectSubclass<imp::FeedView>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl FeedView {
    pub fn new(style: CardStyle) -> Self {
        let obj: Self = glib::Object::builder()
            .property("orientation", gtk::Orientation::Vertical)
            .build();
        obj.imp().style.set(style);
        obj.build();
        obj
    }

    fn build(&self) {
        let imp = self.imp();
        let style = imp.style.get();
        self.set_vexpand(true);
        self.set_hexpand(true);
        imp.empty_title.replace("Nothing here".into());
        imp.empty_icon.replace("view-grid-symbolic".into());

        // Chips
        imp.chips_box.set_spacing(8);
        imp.chips_box.add_css_class("chips");
        let chips_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&imp.chips_box)
            .build();
        imp.chips_revealer.set_child(Some(&chips_scroll));
        imp.chips_revealer.set_reveal_child(false);
        self.append(&imp.chips_revealer);

        // Items
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(move |_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
            li.set_child(Some(&ItemCard::new(style)));
        });
        factory.connect_bind(|_, obj| {
            let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
            let (Some(card), Some(item)) = (
                li.child().and_downcast::<ItemCard>(),
                li.item().and_downcast::<glib::BoxedAnyObject>(),
            ) else {
                return;
            };
            card.bind(&item.borrow::<Item>());
        });
        let selection = gtk::NoSelection::new(Some(model.clone()));
        let view: gtk::Widget = if style.compact {
            let list = gtk::ListView::new(Some(selection), Some(factory));
            list.set_single_click_activate(true);
            list.add_css_class("feed-list");
            list.connect_activate(|list, pos| activate(list.upcast_ref(), pos));
            list.upcast()
        } else {
            let grid = gtk::GridView::new(Some(selection), Some(factory));
            grid.set_max_columns(6);
            grid.set_min_columns(1);
            grid.set_single_click_activate(true);
            grid.add_css_class("feed-grid");
            grid.connect_activate(|grid, pos| activate(grid.upcast_ref(), pos));
            grid.upcast()
        };
        imp.scrolled.set_child(Some(&view));
        imp.scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        imp.scrolled.set_vexpand(true);
        // Infinite scroll: fetch the next page when within ~2 screens of the end,
        // also when the content doesn't fill the view yet (changed = new layout).
        let adj = imp.scrolled.vadjustment();
        adj.connect_value_changed(glib::clone!(
            #[weak(rename_to = feed)]
            self,
            move |adj| feed.maybe_load_more(adj)
        ));
        adj.connect_changed(glib::clone!(
            #[weak(rename_to = feed)]
            self,
            move |adj| feed.maybe_load_more(adj)
        ));
        imp.model.replace(Some(model));
        imp.view.replace(Some(view));

        let spinner = adw::Spinner::new();
        spinner.set_size_request(24, 24);
        spinner.set_margin_bottom(12);
        imp.more_spinner.set_child(Some(&spinner));
        imp.more_spinner.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        imp.more_spinner.set_valign(gtk::Align::End);
        imp.more_spinner.set_halign(gtk::Align::Center);
        imp.more_spinner.set_can_target(false);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&imp.scrolled));
        overlay.add_overlay(&imp.more_spinner);

        // Loading / status
        let loading = adw::Spinner::new();
        loading.set_size_request(32, 32);
        loading.set_halign(gtk::Align::Center);
        loading.set_valign(gtk::Align::Center);
        imp.retry.set_label("Retry");
        imp.retry.set_halign(gtk::Align::Center);
        imp.retry.add_css_class("pill");
        imp.retry.connect_clicked(glib::clone!(
            #[weak(rename_to = feed)]
            self,
            move |_| feed.reload()
        ));
        imp.status.set_vexpand(true);
        let status_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        status_box.append(&imp.status);

        imp.stack.add_named(&loading, Some("loading"));
        imp.stack.add_named(&status_box, Some("status"));
        imp.stack.add_named(&overlay, Some("content"));
        imp.stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        imp.stack.set_vexpand(true);
        self.append(&imp.stack);
    }

    pub fn set_empty_state(&self, icon: &str, title: &str) {
        self.imp().empty_icon.replace(icon.to_owned());
        self.imp().empty_title.replace(title.to_owned());
    }

    pub fn set_loader(&self, loader: Loader) {
        self.imp().loader.replace(Some(loader));
    }

    pub fn vadjustment(&self) -> gtk::Adjustment {
        self.imp().scrolled.vadjustment()
    }

    /// Called after every page (`true` for the first page / a reload).
    pub fn connect_page_loaded(&self, f: impl Fn(&FeedPage, bool) + 'static) {
        self.imp().on_page.borrow_mut().push(Box::new(f));
    }

    pub fn is_loaded(&self) -> bool {
        self.imp().loaded_once.get()
    }

    /// Load the first page unless already loaded.
    pub fn ensure_loaded(&self) {
        if !self.imp().loaded_once.get() && !self.imp().loading.get() {
            self.reload();
        }
    }

    pub fn reload(&self) {
        self.load(None, true);
    }

    fn reload_with_token(&self, token: String) {
        self.load(Some(token), true);
    }

    fn maybe_load_more(&self, adj: &gtk::Adjustment) {
        // Unallocated views report zero sizes; wait for a real layout.
        if adj.page_size() <= 0.0 || adj.upper() <= 0.0 || !self.is_mapped() {
            return;
        }
        // While a page is still being appended the end isn't real yet.
        if !self.imp().pending.borrow().is_empty() {
            return;
        }
        if adj.value() + adj.page_size() * 2.5 >= adj.upper() {
            self.load_more();
        }
    }

    pub fn load_more(&self) {
        let imp = self.imp();
        if imp.loading.get() {
            return;
        }
        let Some(token) = imp.continuation.borrow().clone() else {
            return;
        };
        self.load(Some(token), false);
    }

    /// Show items supplied by the caller (no network).
    pub fn set_items(&self, items: Vec<Item>) {
        let page = FeedPage {
            items,
            ..Default::default()
        };
        self.imp().generation.set(self.imp().generation.get() + 1);
        self.apply_page(page, true);
    }

    fn load(&self, token: Option<String>, replace: bool) {
        let imp = self.imp();
        let Some(loader) = imp.loader.borrow().clone() else {
            return;
        };
        let ticket = imp.generation.get() + 1;
        imp.generation.set(ticket);
        imp.loading.set(true);
        if replace {
            imp.continuation.replace(None);
            if !imp.loaded_once.get() || token.is_none() {
                imp.stack.set_visible_child_name("loading");
            } else {
                imp.more_spinner.set_reveal_child(true);
            }
        } else {
            imp.more_spinner.set_reveal_child(true);
        }
        let fut = loader(token);
        let feed = self.downgrade();
        log::debug!("feed load #{ticket} (replace={replace})");
        glib::spawn_future_local(async move {
            let res = fut.await;
            let Some(feed) = feed.upgrade() else { return };
            let imp = feed.imp();
            log::debug!("feed load #{ticket} finished (current #{}): ok={}", imp.generation.get(), res.is_ok());
            if imp.generation.get() != ticket {
                return;
            }
            imp.loading.set(false);
            imp.more_spinner.set_reveal_child(false);
            match res {
                Ok(page) => feed.apply_page(page, replace),
                Err(e) => {
                    log::warn!("feed load failed: {e:#}");
                    if replace || !imp.loaded_once.get() {
                        imp.status.set_icon_name(Some("network-error-symbolic"));
                        imp.status.set_title("Couldn't load this page");
                        imp.status.set_description(Some(&glib::markup_escape_text(&format!("{e}"))));
                        imp.status.set_child(Some(&imp.retry));
                        imp.stack.set_visible_child_name("status");
                    } else {
                        super::toast(&feed, &format!("Couldn't load more: {e}"));
                    }
                }
            }
        });
    }

    fn apply_page(&self, page: FeedPage, replace: bool) {
        let imp = self.imp();
        let model = imp.model.borrow().clone().unwrap();
        let objs: Vec<glib::BoxedAnyObject> = page
            .items
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        if replace {
            imp.pending.borrow_mut().clear();
            model.splice(0, model.n_items(), &objs);
            imp.scrolled.vadjustment().set_value(0.0);
        } else {
            imp.pending.borrow_mut().extend(objs);
            self.drain_pending();
        }
        imp.continuation.replace(page.continuation.clone());
        imp.loaded_once.set(true);
        if !page.chips.is_empty() {
            self.set_chips(&page.chips);
        }
        if model.n_items() == 0 {
            imp.status.set_icon_name(Some(&imp.empty_icon.borrow()));
            imp.status.set_title(&imp.empty_title.borrow());
            imp.status
                .set_description(page.message.as_deref().map(|m| glib::markup_escape_text(m).to_string()).as_deref());
            imp.status.set_child(None::<&gtk::Widget>);
            imp.stack.set_visible_child_name("status");
        } else {
            imp.stack.set_visible_child_name("content");
        }
        for cb in imp.on_page.borrow().iter() {
            cb(&page, replace);
        }
    }

    /// Append pending items about one per frame. The grid view creates and binds a
    /// card for every new item right away (it keeps ~30 rows of widgets), so a
    /// whole page at once stalled scrolling for a couple of frames.
    fn drain_pending(&self) {
        let imp = self.imp();
        if imp.draining.get() {
            return;
        }
        if !self.is_mapped() {
            // Nothing on screen to keep smooth.
            let objs: Vec<_> = imp.pending.borrow_mut().drain(..).collect();
            if let Some(model) = imp.model.borrow().as_ref() {
                model.extend_from_slice(&objs);
            }
            return;
        }
        imp.draining.set(true);
        self.add_tick_callback(|feed, _| {
            let imp = feed.imp();
            let Some(model) = imp.model.borrow().clone() else {
                imp.draining.set(false);
                return glib::ControlFlow::Break;
            };
            let start = std::time::Instant::now();
            loop {
                let Some(obj) = imp.pending.borrow_mut().pop_front() else { break };
                model.append(&obj);
                if start.elapsed() >= std::time::Duration::from_millis(1) {
                    break;
                }
            }
            if imp.pending.borrow().is_empty() {
                imp.draining.set(false);
                // Re-check now that the end is real.
                feed.maybe_load_more(&imp.scrolled.vadjustment());
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    fn set_chips(&self, chips: &[Chip]) {
        let imp = self.imp();
        while let Some(c) = imp.chips_box.first_child() {
            imp.chips_box.remove(&c);
        }
        for chip in chips {
            let b = gtk::ToggleButton::with_label(&chip.title);
            b.add_css_class("chip");
            b.set_active(chip.selected);
            if let Some(token) = chip.token.clone() {
                b.connect_clicked(glib::clone!(
                    #[weak(rename_to = feed)]
                    self,
                    move |btn| {
                        // Behave like radio buttons.
                        let mut child = feed.imp().chips_box.first_child();
                        while let Some(c) = child {
                            if let Some(t) = c.downcast_ref::<gtk::ToggleButton>() {
                                t.set_active(t == btn);
                            }
                            child = c.next_sibling();
                        }
                        feed.reload_with_token(token.clone());
                    }
                ));
            } else {
                b.set_sensitive(chip.selected);
            }
            imp.chips_box.append(&b);
        }
        imp.chips_revealer.set_reveal_child(!chips.is_empty());
    }
}

fn activate(view: &gtk::Widget, pos: u32) {
    let model = if let Some(g) = view.downcast_ref::<gtk::GridView>() {
        g.model()
    } else {
        view.downcast_ref::<gtk::ListView>().and_then(|l| l.model())
    };
    let Some(obj) = model.and_then(|m| m.item(pos)).and_downcast::<glib::BoxedAnyObject>() else {
        return;
    };
    let item = obj.borrow::<Item>().clone();
    super::navigate(view, super::card::nav_for_item(&item));
}
