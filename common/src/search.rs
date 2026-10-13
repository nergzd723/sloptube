//! The search page both apps use: an entry, suggestions while typing (the
//! search history first), and the app's results. The app says where history
//! is kept, what to suggest, and how to search.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

/// Something to show under the entry while typing.
pub enum Suggestion {
    /// A query to search for.
    Query(String),
    /// Anything else (a song, an artist, ...), handling its own clicks.
    Widget(gtk::Widget),
}

type Suggest = Box<dyn Fn(String) -> Pin<Box<dyn Future<Output = Vec<Suggestion>>>>>;

const PAGE_KEY: &str = "sloptube-search";

/// What the app provides.
pub struct SearchConfig {
    pub placeholder: &'static str,
    /// The search history, newest first.
    pub history: Box<dyn Fn() -> Vec<String>>,
    pub remember: Box<dyn Fn(&str)>,
    pub forget: Box<dyn Fn(&str)>,
    /// Suggestions for what has been typed (called after a short pause).
    pub suggest: Suggest,
    /// Run a search: load the results.
    pub on_search: Box<dyn Fn(&str)>,
}

struct Inner {
    config: SearchConfig,
    entry: gtk::SearchEntry,
    stack: gtk::Stack,
    suggestions: gtk::ListBox,
    query: RefCell<String>,
    generation: Cell<u64>,
    debounce: RefCell<Option<glib::SourceId>>,
}

#[derive(Clone)]
pub struct Search(Rc<Inner>);

impl Search {
    /// `results` is shown once a search has run.
    pub fn new(config: SearchConfig, results: &impl IsA<gtk::Widget>) -> Self {
        let search = Search(Rc::new(Inner {
            config,
            entry: gtk::SearchEntry::new(),
            stack: gtk::Stack::new(),
            suggestions: gtk::ListBox::new(),
            query: RefCell::default(),
            generation: Cell::new(0),
            debounce: RefCell::default(),
        }));
        let i = &search.0;
        i.entry.set_placeholder_text(Some(i.config.placeholder));
        i.entry.set_hexpand(true);
        i.entry.set_search_delay(0);
        let weak = Rc::downgrade(&search.0);
        i.entry.connect_search_changed(move |e| {
            if let Some(i) = weak.upgrade() {
                Search(i).on_changed(&e.text());
            }
        });
        let weak = Rc::downgrade(&search.0);
        i.entry.connect_activate(move |e| {
            if let Some(i) = weak.upgrade() {
                Search(i).search(&e.text());
            }
        });
        // Escape clears what was typed, then leaves the page.
        i.entry.connect_stop_search(|e| {
            if e.text().is_empty() {
                e.activate_action("navigation.pop", None).ok();
            } else {
                e.set_text("");
            }
        });

        i.suggestions.add_css_class("suggestions");
        i.suggestions.set_selection_mode(gtk::SelectionMode::None);
        let weak = Rc::downgrade(&search.0);
        i.suggestions.connect_row_activated(move |_, row| {
            if let Some(i) = weak.upgrade() {
                Search(i).search(&row.widget_name());
            }
        });
        let sw = gtk::ScrolledWindow::builder()
            .child(&adw::Clamp::builder().maximum_size(720).child(&i.suggestions).build())
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        i.stack.add_named(&sw, Some("suggestions"));
        i.stack.add_named(results, Some("results"));
        i.stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        search.show_suggestions_for("");
        search
    }

    pub fn entry(&self) -> &gtk::SearchEntry {
        &self.0.entry
    }

    /// The suggestions and the results, to put into a page.
    pub fn widget(&self) -> &gtk::Stack {
        &self.0.stack
    }

    /// What was last searched for.
    pub fn query(&self) -> String {
        self.0.query.borrow().clone()
    }

    /// A navigation page (tagged "search") with the entry in its header bar.
    /// Coming back to it focuses the entry only while suggestions show, so
    /// returning to results doesn't pop up an on-screen keyboard.
    pub fn page(&self) -> adw::NavigationPage {
        let clamp = adw::Clamp::builder().maximum_size(640).child(&self.0.entry).build();
        clamp.set_hexpand(true);
        let header = crate::shell::header_bar();
        header.set_title_widget(Some(&clamp));
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&self.0.stack));
        let page = adw::NavigationPage::builder().title("Search").tag("search").child(&toolbar).build();
        // SAFETY: the key is only ever used with `Search`.
        unsafe { page.set_data(PAGE_KEY, self.clone()) };
        let weak = Rc::downgrade(&self.0);
        page.connect_shown(move |_| {
            if let Some(i) = weak.upgrade()
                && i.stack.visible_child_name().as_deref() == Some("suggestions")
            {
                i.entry.grab_focus();
            }
        });
        page
    }

    /// The search a page made by [`Search::page`] shows.
    pub fn of_page(page: &adw::NavigationPage) -> Option<Search> {
        // SAFETY: see `page`.
        unsafe { page.data::<Search>(PAGE_KEY).map(|s| s.as_ref().clone()) }
    }

    /// Show what was searched for as the page's title (back buttons use it).
    fn retitle(&self, query: &str) {
        let page = self.0.entry.ancestor(adw::NavigationPage::static_type()).and_downcast::<adw::NavigationPage>();
        if let Some(page) = page {
            page.set_title(query);
        }
    }

    pub fn focus_entry(&self) {
        self.0.entry.grab_focus();
        self.0.entry.select_region(0, -1);
    }

    pub fn search(&self, query: &str) {
        let i = &self.0;
        let q = query.trim();
        if q.is_empty() {
            return;
        }
        if let Some(id) = i.debounce.take() {
            id.remove();
        }
        // Suggestions still on their way are for the old text.
        i.generation.set(i.generation.get() + 1);
        (i.config.remember)(q);
        i.query.replace(q.to_owned());
        if i.entry.text() != q {
            i.entry.set_text(q);
        }
        i.stack.set_visible_child_name("results");
        self.retitle(q);
        (i.config.on_search)(q);
    }

    fn on_changed(&self, text: &str) {
        let i = &self.0;
        if let Some(id) = i.debounce.take() {
            id.remove();
        }
        if text.trim().is_empty() {
            self.show_suggestions_for("");
            return;
        }
        // Don't flip back to suggestions for the text a search just filled in.
        if i.stack.visible_child_name().as_deref() == Some("results") && *i.query.borrow() == text {
            return;
        }
        let text = text.to_owned();
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_local_once(Duration::from_millis(140), move || {
            if let Some(i) = weak.upgrade() {
                i.debounce.take();
                Search(i).show_suggestions_for(&text);
            }
        });
        i.debounce.replace(Some(id));
    }

    fn show_suggestions_for(&self, text: &str) {
        let i = &self.0;
        i.stack.set_visible_child_name("suggestions");
        let generation = i.generation.get() + 1;
        i.generation.set(generation);
        let lower = text.to_lowercase();
        let history: Vec<String> = (i.config.history)()
            .into_iter()
            .filter(|h| text.is_empty() || h.to_lowercase().starts_with(&lower))
            .take(if text.is_empty() { 15 } else { 4 })
            .collect();
        i.suggestions.remove_all();
        for h in &history {
            i.suggestions.append(&self.query_row(h, true));
        }
        if text.trim().is_empty() {
            return;
        }
        let fut = (i.config.suggest)(text.to_owned());
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let found = fut.await;
            let Some(i) = weak.upgrade() else { return };
            if i.generation.get() != generation {
                return;
            }
            let search = Search(i);
            for s in found {
                match s {
                    Suggestion::Query(q) if !history.contains(&q) => search.0.suggestions.append(&search.query_row(&q, false)),
                    Suggestion::Query(_) => {}
                    Suggestion::Widget(w) => {
                        let row = gtk::ListBoxRow::new();
                        row.set_activatable(false);
                        row.set_child(Some(&w));
                        search.0.suggestions.append(&row);
                    }
                }
            }
        });
    }

    fn query_row(&self, query: &str, history: bool) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(query);
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.add_css_class("suggestion");
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
            let weak = Rc::downgrade(&self.0);
            remove.connect_clicked(move |_| {
                if let Some(i) = weak.upgrade() {
                    (i.config.forget)(&q);
                    let text = i.entry.text().to_string();
                    Search(i).show_suggestions_for(&text);
                }
            });
            b.append(&remove);
        }
        let fill = gtk::Button::from_icon_name("go-up-symbolic");
        fill.add_css_class("flat");
        fill.add_css_class("circular");
        fill.add_css_class("fill-arrow");
        fill.set_tooltip_text(Some("Edit this search"));
        let q = query.to_owned();
        let weak = Rc::downgrade(&self.0);
        fill.connect_clicked(move |_| {
            if let Some(i) = weak.upgrade() {
                i.entry.set_text(&format!("{q} "));
                i.entry.grab_focus();
                i.entry.set_position(-1);
            }
        });
        b.append(&fill);
        row.set_child(Some(&b));
        row
    }
}
