//! Search: suggestions while typing, history, results with filter chips.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::browse::{ChipBar, shelves_to_rows};
use super::cards::{ListRow, RowStyle};
use super::rows::{Loader, Row, RowList, TrackCtx};
use super::actions;
use crate::api::{self, Item};
use crate::runtime;
use crate::settings;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SearchView {
        pub entry: gtk::SearchEntry,
        pub stack: gtk::Stack,
        pub suggestions: gtk::ListBox,
        pub rows: RowList,
        pub chips: ChipBar,
        pub query: RefCell<String>,
        pub params: RefCell<Option<String>>,
        pub chip_title: RefCell<Option<String>>,
        pub suggest_gen: Cell<u64>,
        pub debounce: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchView {
        const NAME: &'static str = "StmSearchView";
        type Type = super::SearchView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for SearchView {}
    impl WidgetImpl for SearchView {}
    impl BinImpl for SearchView {}
}

glib::wrapper! {
    pub struct SearchView(ObjectSubclass<imp::SearchView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SearchView {
    pub fn new() -> Self {
        let obj: Self = glib::Object::new();
        obj.build();
        obj
    }

    pub fn entry(&self) -> gtk::SearchEntry {
        self.imp().entry.clone()
    }

    fn build(&self) {
        let imp = self.imp();
        imp.entry.set_placeholder_text(Some("Search songs, albums, artists"));
        imp.entry.set_hexpand(true);
        imp.entry.set_search_delay(0);
        imp.entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |e| view.on_changed(&e.text())
        ));
        imp.entry.connect_activate(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |e| {
                let q = e.text().to_string();
                view.search(&q);
            }
        ));
        imp.entry.connect_stop_search(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |e| {
                if e.text().is_empty() {
                    view.activate_action("navigation.pop", None).ok();
                } else {
                    e.set_text("");
                }
            }
        ));

        imp.suggestions.add_css_class("suggestions");
        imp.suggestions.set_selection_mode(gtk::SelectionMode::None);
        imp.suggestions.connect_row_activated(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, row| {
                let name = row.widget_name();
                if let Some(q) = name.strip_prefix("q:") {
                    view.imp().entry.set_text(q);
                    view.search(q);
                }
            }
        ));
        let clamp = adw::Clamp::builder().maximum_size(800).child(&imp.suggestions).build();
        let sw = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build();
        imp.stack.add_named(&sw, Some("suggestions"));

        imp.chips.connect_select(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |chip| {
                let imp = view.imp();
                imp.chip_title.replace(chip.as_ref().map(|c| c.title.clone()));
                imp.params.replace(chip.and_then(|c| c.params));
                imp.rows.scroll_to_top();
                imp.rows.reload();
            }
        ));
        imp.rows.set_prefix(vec![Row::Widget(imp.chips.clone().upcast())]);
        imp.rows.set_empty_state("system-search-symbolic", "No results", "Try different words.");
        imp.rows.set_loader(self.loader());
        imp.stack.add_named(&imp.rows, Some("results"));
        imp.stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        self.set_child(Some(&imp.stack));
        self.show_suggestions_for("");
    }

    fn on_changed(&self, text: &str) {
        let imp = self.imp();
        if let Some(id) = imp.debounce.take() {
            id.remove();
        }
        if text.trim().is_empty() {
            self.show_suggestions_for("");
            return;
        }
        // Don't flip back to suggestions for the text a search just filled in.
        if imp.stack.visible_child_name().as_deref() == Some("results") && *imp.query.borrow() == text {
            return;
        }
        let text = text.to_owned();
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(140),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    view.imp().debounce.take();
                    view.show_suggestions_for(&text);
                }
            ),
        );
        imp.debounce.replace(Some(id));
    }

    fn query_row(&self, query: &str, history: bool) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&format!("q:{query}"));
        row.add_css_class("suggestion");
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 14);
        let icon = gtk::Image::from_icon_name(if history {
            "document-open-recent-symbolic"
        } else {
            "system-search-symbolic"
        });
        icon.add_css_class("dimmed");
        let label = gtk::Label::new(Some(query));
        label.set_xalign(0.0);
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        b.append(&icon);
        b.append(&label);
        if history {
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            remove.add_css_class("flat");
            remove.add_css_class("circular");
            remove.set_tooltip_text(Some("Remove from history"));
            let q = query.to_owned();
            remove.connect_clicked(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| {
                    settings::update(|s| s.search_history.retain(|h| *h != q));
                    let text = view.imp().entry.text().to_string();
                    view.show_suggestions_for(&text);
                }
            ));
            b.append(&remove);
        } else {
            let fill = gtk::Button::from_icon_name("go-up-symbolic");
            fill.add_css_class("flat");
            fill.add_css_class("circular");
            fill.add_css_class("fill-arrow");
            fill.set_tooltip_text(Some("Edit"));
            let q = query.to_owned();
            fill.connect_clicked(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |_| {
                    let e = &view.imp().entry;
                    e.set_text(&format!("{q} "));
                    e.set_position(-1);
                    e.grab_focus();
                }
            ));
            b.append(&fill);
        }
        row.set_child(Some(&b));
        row
    }

    fn show_suggestions_for(&self, text: &str) {
        let imp = self.imp();
        imp.stack.set_visible_child_name("suggestions");
        let generation = imp.suggest_gen.get() + 1;
        imp.suggest_gen.set(generation);
        let history: Vec<String> = settings::get()
            .search_history
            .into_iter()
            .filter(|h| text.is_empty() || h.to_lowercase().starts_with(&text.to_lowercase()))
            .take(if text.is_empty() { 12 } else { 3 })
            .collect();
        super::clear(&imp.suggestions);
        for h in &history {
            imp.suggestions.append(&self.query_row(h, true));
        }
        if text.trim().is_empty() {
            return;
        }
        let q = text.to_owned();
        let view = self.downgrade();
        glib::spawn_future_local(async move {
            let res = runtime::run(async move { api::client().suggestions(&q).await }).await;
            let Some(view) = view.upgrade() else { return };
            let imp = view.imp();
            if imp.suggest_gen.get() != generation {
                return;
            }
            let Ok(s) = res else { return };
            for q in s.queries.iter().filter(|q| !history.contains(q)) {
                imp.suggestions.append(&view.query_row(q, false));
            }
            for item in s.items {
                let row = gtk::ListBoxRow::new();
                row.set_activatable(false);
                let lr = ListRow::new();
                lr.bind_item(&item, RowStyle::Art);
                let weak = lr.downgrade();
                lr.set_on_click(move || {
                    if let Some(lr) = weak.upgrade()
                        && let Some(item) = lr.item() {
                            actions::activate(&lr, &item);
                        }
                });
                row.set_child(Some(&lr));
                imp.suggestions.append(&row);
            }
        });
    }

    pub fn search(&self, query: &str) {
        let imp = self.imp();
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        if let Some(id) = imp.debounce.take() {
            id.remove();
        }
        imp.suggest_gen.set(imp.suggest_gen.get() + 1);
        settings::add_search_history(q);
        imp.query.replace(q.to_owned());
        imp.params.replace(None);
        imp.chip_title.replace(None);
        if imp.entry.text() != query {
            imp.entry.set_text(q);
        }
        imp.stack.set_visible_child_name("results");
        imp.rows.scroll_to_top();
        imp.rows.reload();
        imp.rows.grab_focus();
    }

    fn loader(&self) -> Loader {
        let weak = self.downgrade();
        Rc::new(move |token| {
            let weak = weak.clone();
            Box::pin(async move {
                let ctx = TrackCtx::radio(RowStyle::Art);
                if let Some(t) = token {
                    let (items, cont) = runtime::run(async move { api::client().search_more(&t).await }).await?;
                    let rows = items
                        .into_iter()
                        .enumerate()
                        .map(|(i, item)| match item {
                            Item::Track(track) => Row::Track {
                                track,
                                index: i,
                                ctx: ctx.clone(),
                            },
                            other => Row::Item(other),
                        })
                        .collect();
                    return Ok((rows, cont));
                }
                let Some(view) = weak.upgrade() else {
                    return Ok((Vec::new(), None));
                };
                let q = view.imp().query.borrow().clone();
                let params = view.imp().params.borrow().clone();
                drop(view);
                let page = runtime::run(async move { api::client().search(&q, params.as_deref()).await }).await?;
                if let Some(view) = weak.upgrade() {
                    let imp = view.imp();
                    imp.chips.set_chips(&page.chips, imp.chip_title.borrow().as_deref());
                }
                let cont = page
                    .shelves
                    .iter()
                    .find_map(|s| s.continuation.clone())
                    .or(page.continuation.clone());
                Ok((shelves_to_rows(page.shelves), cont))
            })
        })
    }
}

impl Default for SearchView {
    fn default() -> Self {
        Self::new()
    }
}

pub fn search_page() -> (adw::NavigationPage, SearchView) {
    let view = SearchView::new();
    let header = super::page::header_bar();
    let clamp = adw::Clamp::builder().maximum_size(640).child(&view.entry()).build();
    clamp.set_hexpand(true);
    header.set_title_widget(Some(&clamp));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&view));
    let page = adw::NavigationPage::builder()
        .title("Search")
        .tag("search")
        .child(&toolbar)
        .build();
    let entry = view.entry();
    page.connect_shown(move |_| {
        entry.grab_focus();
    });
    (page, view)
}
