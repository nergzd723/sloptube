//! Rows both apps' Preferences dialogs are made of. The settings themselves
//! are each app's, so the rows take what to do as callbacks.

use adw::prelude::*;
use gtk::glib;

use crate::account::Account;

pub fn switch_row(title: &str, subtitle: Option<&str>, active: bool, f: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(active).build();
    if let Some(s) = subtitle.filter(|s| !s.is_empty()) {
        row.set_subtitle(s);
    }
    row.connect_active_notify(move |r| f(r.is_active()));
    row
}

pub fn combo_row(title: &str, items: &[&str], selected: usize, f: impl Fn(usize) + 'static) -> adw::ComboRow {
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(items))
        .selected(selected as u32)
        .build();
    row.connect_selected_notify(move |r| f(r.selected() as usize));
    row
}

/// Who is signed in (picture, name, handle) with a Sign In / Sign Out button
/// (the window's `win.sign-in` and `win.sign-out`), kept up to date while
/// `dialog` is open. `signed_out_hint` says what signing in gives.
pub fn account_row(dialog: &adw::PreferencesDialog, signed_out_hint: &str) -> adw::ActionRow {
    let who = adw::ActionRow::new();
    let avatar = adw::Avatar::new(40, None, true);
    who.add_prefix(&avatar);
    let button = gtk::Button::new();
    button.set_valign(gtk::Align::Center);
    who.add_suffix(&button);
    let hint = signed_out_hint.to_owned();
    let update = glib::clone!(
        #[weak]
        who,
        #[weak]
        avatar,
        #[weak]
        button,
        move |acc: &Account| {
            if acc.signed_in() {
                who.set_title(&glib::markup_escape_text(&acc.name()));
                who.set_subtitle(&glib::markup_escape_text(&acc.handle()));
                avatar.set_text(Some(&acc.name()));
                let url = acc.avatar();
                crate::image::set_avatar(&avatar, (!url.is_empty()).then_some(url.as_str()));
                button.set_label("Sign Out");
                button.set_action_name(Some("win.sign-out"));
                button.remove_css_class("suggested-action");
            } else {
                who.set_title("Not signed in");
                who.set_subtitle(&hint);
                avatar.set_text(None);
                crate::image::set_avatar(&avatar, None);
                button.set_label("Sign In…");
                button.set_action_name(Some("win.sign-in"));
                button.add_css_class("suggested-action");
            }
        }
    );
    let acc = Account::get();
    update(&acc);
    let ids: Vec<glib::SignalHandlerId> = ["signed-in", "name", "avatar"]
        .into_iter()
        .map(|p| {
            let update = update.clone();
            acc.connect_notify_local(Some(p), move |a, _| update(a))
        })
        .collect();
    let ids = std::cell::RefCell::new(ids);
    dialog.connect_closed(move |_| {
        for id in ids.borrow_mut().drain(..) {
            Account::get().disconnect(id);
        }
    });
    who
}

/// The content region: a two-letter code, or empty for automatic. `apply`
/// gets the new region once it's valid.
pub fn region_row(current: Option<&str>, apply: impl Fn(Option<String>) + 'static) -> adw::EntryRow {
    let region = adw::EntryRow::builder()
        .title("Region (two-letter code such as US, DE or UA; empty for automatic)")
        .text(current.unwrap_or_default())
        .show_apply_button(true)
        .build();
    region.connect_apply(move |r| {
        let code = r.text().trim().to_uppercase();
        let region = (code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic())).then_some(code);
        if region.is_none() && !r.text().trim().is_empty() {
            r.add_css_class("error");
            return;
        }
        r.remove_css_class("error");
        apply(region);
    });
    region
}

/// "Save search history" and "Clear search history". `set_saving(false)`
/// should also forget what was saved.
pub fn add_search_history_rows(
    group: &adw::PreferencesGroup,
    saving: bool,
    set_saving: impl Fn(bool) + 'static,
    clear: impl Fn() + 'static,
) {
    group.add(&switch_row("Save search history", None, saving, set_saving));
    let clear_row = adw::ButtonRow::builder().title("Clear search history").build();
    clear_row.connect_activated(move |_| clear());
    group.add(&clear_row);
}
