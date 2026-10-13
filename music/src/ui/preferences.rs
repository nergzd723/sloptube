//! Preferences.

use adw::prelude::*;
use gtk::glib;

use crate::account::{self, Account};
use crate::api;
use crate::image;
use crate::library::Library;
use crate::settings::{self, AudioQuality};

fn switch_row(title: &str, subtitle: Option<&str>, active: bool, f: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(active).build();
    if let Some(s) = subtitle {
        row.set_subtitle(s);
    }
    row.connect_active_notify(move |r| f(r.is_active()));
    row
}

pub fn present(parent: &impl IsA<gtk::Widget>) {
    let s = settings::get();
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();

    let account_group = adw::PreferencesGroup::builder().title("Account").build();
    let who = adw::ActionRow::new();
    let avatar = adw::Avatar::new(40, None, true);
    who.add_prefix(&avatar);
    let button = gtk::Button::new();
    button.set_valign(gtk::Align::Center);
    who.add_suffix(&button);
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
                image::set_url(&avatar, (!url.is_empty()).then_some(url.as_str()), 96);
                button.set_label("Sign Out");
                button.set_action_name(Some("win.sign-out"));
                button.remove_css_class("suggested-action");
            } else {
                who.set_title("Not signed in");
                who.set_subtitle("Sign in for your recommendations, library, likes and history");
                avatar.set_text(None);
                image::set_url(&avatar, None, 0);
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
    account_group.add(&who);
    account_group.add(&switch_row(
        "Use the account for audio",
        Some("Premium audio quality and age-restricted songs"),
        s.use_account_for_streams,
        |v| {
            settings::update(|s| s.use_account_for_streams = v);
            account::update_stream_cookies();
        },
    ));
    account_group.add(&switch_row(
        "Add plays to YouTube history",
        Some("Songs played here appear in your history and shape recommendations"),
        s.send_history,
        |v| settings::update(|s| s.send_history = v),
    ));
    page.add(&account_group);

    let playback = adw::PreferencesGroup::builder().title("Playback").build();
    let quality = adw::ComboRow::builder()
        .title("Audio quality")
        .subtitle("Lower quality uses less data")
        .model(&gtk::StringList::new(
            &AudioQuality::ALL.iter().map(|q| q.label()).collect::<Vec<_>>(),
        ))
        .selected(AudioQuality::ALL.iter().position(|q| *q == s.quality).unwrap_or(2) as u32)
        .build();
    quality.connect_selected_notify(|r| {
        let q = AudioQuality::ALL[(r.selected() as usize).min(2)];
        settings::update(|s| s.quality = q);
    });
    playback.add(&quality);
    playback.add(&switch_row(
        "Autoplay",
        Some("Keep playing similar songs when the queue ends"),
        s.autoplay,
        |v| settings::update(|s| s.autoplay = v),
    ));
    playback.add(&switch_row(
        "Play in the background",
        Some("Keep playing after the window is closed"),
        s.background_playback,
        |v| settings::update(|s| s.background_playback = v),
    ));
    playback.add(&switch_row(
        "Restore the queue",
        Some("Bring back the last queue when the app starts"),
        s.restore_queue,
        |v| settings::update(|s| s.restore_queue = v),
    ));
    page.add(&playback);

    let content = adw::PreferencesGroup::builder().title("Content").build();
    let region = adw::EntryRow::builder()
        .title("Region (two-letter code, empty for automatic)")
        .text(s.region.clone().unwrap_or_default())
        .show_apply_button(true)
        .build();
    region.connect_apply(|r| {
        let code = r.text().trim().to_uppercase();
        let region = (code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic())).then_some(code);
        if region.is_none() && !r.text().trim().is_empty() {
            r.add_css_class("error");
            return;
        }
        r.remove_css_class("error");
        settings::update(|s| s.region = region.clone());
        api::client().set_region(region);
        super::toast("Region changed; refresh Home to see it");
    });
    content.add(&region);
    page.add(&content);

    let privacy = adw::PreferencesGroup::builder().title("History").build();
    privacy.add(&switch_row("Save search history", None, s.save_search_history, |v| {
        settings::update(|s| {
            s.save_search_history = v;
            if !v {
                s.search_history.clear();
            }
        })
    }));
    let clear_search = adw::ButtonRow::builder().title("Clear search history").build();
    clear_search.connect_activated(|_| {
        settings::update(|s| s.search_history.clear());
        super::toast("Search history cleared");
    });
    privacy.add(&clear_search);
    let clear_history = adw::ButtonRow::builder().title("Clear listening history").build();
    clear_history.add_css_class("destructive-action");
    clear_history.connect_activated(|_| {
        Library::get().clear_history();
        super::toast("Listening history cleared");
    });
    privacy.add(&clear_history);
    page.add(&privacy);

    dialog.add(&page);
    dialog.present(Some(parent));
}
