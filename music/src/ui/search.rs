//! Search: the shared search page (`common::search`) with Music's results:
//! the filter chips YouTube Music offers over a list of shelves and songs, and
//! songs, albums and artists among the suggestions.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use common::search::{Search, SearchConfig, Suggestion};

use super::actions;
use super::browse::{ChipBar, shelves_to_rows};
use super::cards::{ListRow, RowStyle};
use super::rows::{Loader, Row, RowList, TrackCtx};
use crate::api::{self, Item};
use crate::runtime;
use crate::settings;

/// What the results are for: the query, and the filter chip picked.
#[derive(Default)]
struct Filter {
    query: String,
    params: Option<String>,
    chip_title: Option<String>,
}

/// A new search page, searching for `initial` right away if given.
pub fn search_page(initial: Option<&str>) -> adw::NavigationPage {
    let state = Rc::new(RefCell::new(Filter::default()));
    let rows = RowList::new();
    let chips = ChipBar::new();
    {
        let (state, rows) = (state.clone(), rows.downgrade());
        chips.connect_select(move |chip| {
            {
                let mut st = state.borrow_mut();
                st.chip_title = chip.as_ref().map(|c| c.title.clone());
                st.params = chip.and_then(|c| c.params);
            }
            if let Some(rows) = rows.upgrade() {
                rows.scroll_to_top();
                rows.reload();
            }
        });
    }
    rows.set_prefix(vec![Row::Widget(chips.clone().upcast())]);
    rows.set_empty_state("system-search-symbolic", "No results", "Try different words.");
    rows.set_loader(loader(state.clone(), chips.downgrade()));

    let rows_weak = rows.downgrade();
    let search = Search::new(
        SearchConfig {
            placeholder: "Search songs, albums, artists",
            history: Box::new(|| settings::get().search_history),
            remember: Box::new(settings::add_search_history),
            forget: Box::new(|q| settings::update(|s| s.search_history.retain(|h| h != q))),
            suggest: Box::new(|text| Box::pin(suggest(text))),
            on_search: Box::new(move |q| {
                *state.borrow_mut() = Filter {
                    query: q.to_owned(),
                    ..Default::default()
                };
                if let Some(rows) = rows_weak.upgrade() {
                    rows.scroll_to_top();
                    rows.reload();
                    rows.grab_focus();
                }
            }),
        },
        &rows,
    );
    let page = search.page();
    if let Some(q) = initial.filter(|q| !q.is_empty()) {
        search.search(q);
    }
    page
}

/// Queries, then songs, albums and artists that match `text`.
async fn suggest(text: String) -> Vec<Suggestion> {
    let Ok(s) = runtime::run(async move { api::client().suggestions(&text).await }).await else {
        return Vec::new();
    };
    let mut out: Vec<Suggestion> = s.queries.into_iter().map(Suggestion::Query).collect();
    for item in s.items {
        let lr = ListRow::new();
        lr.bind_item(&item, RowStyle::Art);
        let weak = lr.downgrade();
        lr.set_on_click(move || {
            if let Some(lr) = weak.upgrade()
                && let Some(item) = lr.item()
            {
                actions::activate(&lr, &item);
            }
        });
        out.push(Suggestion::Widget(lr.upcast()));
    }
    out
}

fn loader(state: Rc<RefCell<Filter>>, chips: gtk::glib::WeakRef<ChipBar>) -> Loader {
    Rc::new(move |token| {
        let state = state.clone();
        let chips = chips.clone();
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
            let (q, params) = {
                let st = state.borrow();
                (st.query.clone(), st.params.clone())
            };
            let page = runtime::run(async move { api::client().search(&q, params.as_deref()).await }).await?;
            if let Some(chips) = chips.upgrade() {
                chips.set_chips(&page.chips, state.borrow().chip_title.as_deref());
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
