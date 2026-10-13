//! Search: the shared search page (`common::search`) with SlopTube's results:
//! result filters over a feed of videos, channels and playlists.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use common::chips::ChipRow;
use common::search::{Search, SearchConfig, Suggestion};

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

/// A new search page, searching for `initial` right away if given.
pub fn search_page(initial: Option<&str>) -> adw::NavigationPage {
    let query = Rc::new(RefCell::new(String::new()));
    let filter = Rc::new(Cell::new(0usize));

    let feed = FeedView::new(CardStyle::GRID);
    feed.set_empty_state("system-search-symbolic", "No results");
    {
        let (query, filter) = (query.clone(), filter.clone());
        feed.set_loader(Rc::new(move |token: Option<String>| {
            let q = query.borrow().clone();
            let f = FILTERS[filter.get()].1;
            Box::pin(async move {
                runtime::run(async move {
                    let c = api::client();
                    match token {
                        None => c.search(&q, f).await,
                        Some(t) => c.search_more(&t).await,
                    }
                })
                .await
            })
        }));
    }

    let filters = ChipRow::new();
    for (i, (label, _)) in FILTERS.iter().enumerate() {
        let (filter, feed) = (filter.clone(), feed.downgrade());
        filters.add(label, i == 0, false, move |_| {
            if filter.replace(i) != i
                && let Some(feed) = feed.upgrade()
            {
                feed.reload();
            }
        });
    }
    let results = gtk::Box::new(gtk::Orientation::Vertical, 0);
    results.append(filters.widget());
    results.append(&feed);

    let feed_weak = feed.downgrade();
    let search = Search::new(
        SearchConfig {
            placeholder: "Search YouTube",
            history: Box::new(|| settings::get().search_history),
            remember: Box::new(settings::add_search_history),
            forget: Box::new(|q| settings::update(|s| s.search_history.retain(|h| h != q))),
            suggest: Box::new(|text| {
                Box::pin(async move {
                    let found = runtime::run(async move { api::client().suggestions(&text).await }).await;
                    found.unwrap_or_default().into_iter().map(Suggestion::Query).collect()
                })
            }),
            on_search: Box::new(move |q| {
                query.replace(q.to_owned());
                if let Some(feed) = feed_weak.upgrade() {
                    feed.reload();
                    feed.grab_focus();
                }
            }),
        },
        &results,
    );
    let page = search.page();
    if let Some(q) = initial.filter(|q| !q.is_empty()) {
        search.search(q);
    }
    page
}
