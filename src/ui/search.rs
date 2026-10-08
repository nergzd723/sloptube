//! Search with suggestions, history and result filters.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::card::CardStyle;
use super::feed::FeedView;
use crate::api::{self, SearchFilter};
use crate::runtime;
use crate::settings;

const FILTERS: [(&str, SearchFilter); 5] = [
    ("All", SearchFilter::All),
    ("Videos", SearchFilter::Videos),
    ("Channels", SearchFilter::Channels),
    ("Playlists", SearchFilter::Playlists),
    ("Live", SearchFilter::Live),
];

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SearchPage {
        pub entry: gtk::SearchEntry,
        pub stack: gtk::Stack,
        pub suggestions: gtk::ListBox,
        pub feed: RefCell<Option<FeedView>>,
        pub query: RefCell<String>,
        pub filter: Cell<u8>,
        pub filter_buttons: RefCell<Vec<gtk::ToggleButton>>,
        pub suggest_gen: Cell<u64>,
        pub suppress_suggest: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchPage {
        const NAME: &'static str = "SlopSearchPage";
        type Type = super::SearchPage;
        type ParentType = adw::NavigationPage;
    }

    impl ObjectImpl for SearchPage {}
    impl WidgetImpl for SearchPage {}
    impl NavigationPageImpl for SearchPage {
        fn shown(&self) {
            if self.stack.visible_child_name().as_deref() == Some("suggestions") {
                self.entry.grab_focus();
            }
        }
    }
}

glib::wrapper! {
    pub struct SearchPage(ObjectSubclass<imp::SearchPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SearchPage {
    pub fn new(initial: Option<&str>) -> Self {
        let obj: Self = glib::Object::builder()
            .property("title", "Search")
            .property("tag", "search")
            .build();
        obj.build();
        match initial {
            Some(q) if !q.is_empty() => obj.search(q),
            _ => obj.show_suggestions(),
        }
        obj
    }

    fn build(&self) {
        let imp = self.imp();
        imp.entry.set_placeholder_text(Some("Search YouTube"));
        imp.entry.set_hexpand(true);
        imp.entry.set_search_delay(120);
        let clamp = adw::Clamp::builder().maximum_size(640).child(&imp.entry).build();
        clamp.set_hexpand(true);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&clamp));

        imp.entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |entry| {
                // `search-changed` fires after a delay, also for text we set ourselves.
                if page.imp().suppress_suggest.get() || *page.imp().query.borrow() == entry.text().as_str() {
                    return;
                }
                page.show_suggestions();
            }
        ));
        imp.entry.connect_activate(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |e| {
                let q = e.text().to_string();
                page.search(&q);
            }
        ));
        imp.entry.connect_stop_search(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                if let Some(nav) = page.ancestor(adw::NavigationView::static_type()).and_downcast::<adw::NavigationView>() {
                    nav.pop();
                }
            }
        ));

        // Suggestions
        imp.suggestions.set_selection_mode(gtk::SelectionMode::None);
        imp.suggestions.add_css_class("suggestions");
        imp.suggestions.connect_row_activated(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_, row| {
                let q = row.widget_name().to_string();
                page.search(&q);
            }
        ));
        let sugg_scroll = gtk::ScrolledWindow::builder()
            .child(&adw::Clamp::builder().maximum_size(640).child(&imp.suggestions).build())
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        imp.stack.add_named(&sugg_scroll, Some("suggestions"));

        // Results
        let results = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let filters = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        filters.add_css_class("chips");
        let mut first: Option<gtk::ToggleButton> = None;
        for (i, (label, _)) in FILTERS.iter().enumerate() {
            let b = gtk::ToggleButton::with_label(label);
            b.add_css_class("chip");
            if let Some(f) = &first {
                b.set_group(Some(f));
            } else {
                first = Some(b.clone());
                b.set_active(true);
            }
            b.connect_toggled(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |b| {
                    if b.is_active() && page.imp().filter.get() != i as u8 {
                        page.imp().filter.set(i as u8);
                        if let Some(f) = page.imp().feed.borrow().as_ref() {
                            f.reload();
                        }
                    }
                }
            ));
            filters.append(&b);
            imp.filter_buttons.borrow_mut().push(b);
        }
        let filter_scroll = gtk::ScrolledWindow::builder()
            .child(&filters)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();
        results.append(&filter_scroll);

        let feed = FeedView::new(CardStyle::GRID);
        feed.set_empty_state("system-search-symbolic", "No results");
        let page_weak = self.downgrade();
        feed.set_loader(Rc::new(move |token: Option<String>| {
            let page = page_weak.upgrade();
            let query = page.as_ref().map(|p| p.imp().query.borrow().clone()).unwrap_or_default();
            let filter = page
                .as_ref()
                .map(|p| FILTERS[p.imp().filter.get() as usize].1)
                .unwrap_or_default();
            Box::pin(async move {
                runtime::run(async move {
                    let c = api::client();
                    match token {
                        None => c.search(&query, filter).await,
                        Some(t) => c.search_more(&t).await,
                    }
                })
                .await
            })
        }));
        results.append(&feed);
        imp.feed.replace(Some(feed));
        imp.stack.add_named(&results, Some("results"));
        imp.stack.set_transition_type(gtk::StackTransitionType::Crossfade);

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&imp.stack));
        self.set_child(Some(&tv));
    }

    pub fn search(&self, query: &str) {
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        let imp = self.imp();
        settings::add_search_history(q);
        imp.query.replace(q.to_owned());
        imp.suppress_suggest.set(true);
        imp.entry.set_text(q);
        imp.suppress_suggest.set(false);
        self.set_title(q);
        imp.stack.set_visible_child_name("results");
        if let Some(f) = imp.feed.borrow().as_ref() {
            f.reload();
            f.grab_focus();
        }
    }

    fn suggestion_row(&self, text: &str, history: bool) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(text);
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.add_css_class("suggestion");
        b.append(&gtk::Image::from_icon_name(if history {
            "document-open-recent-symbolic"
        } else {
            "system-search-symbolic"
        }));
        let l = gtk::Label::new(Some(text));
        l.set_xalign(0.0);
        l.set_hexpand(true);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        b.append(&l);
        if history {
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            remove.add_css_class("flat");
            remove.add_css_class("circular");
            remove.set_tooltip_text(Some("Remove from history"));
            let t = text.to_owned();
            remove.connect_clicked(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_| {
                    settings::update(|s| s.search_history.retain(|h| h != &t));
                    page.show_suggestions();
                }
            ));
            b.append(&remove);
        }
        let fill = gtk::Button::from_icon_name("go-up-symbolic");
        fill.add_css_class("flat");
        fill.add_css_class("circular");
        fill.set_tooltip_text(Some("Edit this search"));
        let t = text.to_owned();
        fill.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| {
                let e = &page.imp().entry;
                e.set_text(&format!("{t} "));
                e.grab_focus();
                e.set_position(-1);
            }
        ));
        b.append(&fill);
        row.set_child(Some(&b));
        row
    }

    fn show_suggestions(&self) {
        let imp = self.imp();
        imp.stack.set_visible_child_name("suggestions");
        let text = imp.entry.text().to_string();
        let ticket = imp.suggest_gen.get() + 1;
        imp.suggest_gen.set(ticket);
        imp.suggestions.remove_all();
        let history: Vec<String> = settings::get()
            .search_history
            .into_iter()
            .filter(|h| text.is_empty() || h.to_lowercase().starts_with(&text.to_lowercase()))
            .take(if text.is_empty() { 15 } else { 4 })
            .collect();
        for h in &history {
            imp.suggestions.append(&self.suggestion_row(h, true));
        }
        if text.trim().is_empty() {
            return;
        }
        let page = self.downgrade();
        super::spawn(async move {
            let q = text.clone();
            let res = runtime::run(async move { api::client().suggestions(&q).await }).await;
            let Some(page) = page.upgrade() else { return };
            if page.imp().suggest_gen.get() != ticket {
                return;
            }
            if let Ok(list) = res {
                for s in list.into_iter().filter(|s| !history.contains(s)) {
                    page.imp().suggestions.append(&page.suggestion_row(&s, false));
                }
            }
        });
    }

    pub fn focus_entry(&self) {
        let imp = self.imp();
        imp.entry.grab_focus();
        imp.entry.select_region(0, -1);
    }
}
