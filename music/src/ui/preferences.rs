//! Preferences.

use adw::prelude::*;
use common::preferences::{account_row, add_search_history_rows, region_row, switch_row};

use crate::account;
use crate::api;
use crate::library::Library;
use crate::settings::{self, AudioQuality};

pub fn present(parent: &impl IsA<gtk::Widget>) {
    let s = settings::get();
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::new();

    let account_group = adw::PreferencesGroup::builder().title("Account").build();
    account_group.add(&account_row(&dialog, "Sign in for your recommendations, library, likes and history"));
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
    content.add(&region_row(s.region.as_deref(), |region| {
        settings::update(|s| s.region = region.clone());
        api::client().set_region(region);
        super::toast("Region changed; refresh Home to see it");
    }));
    page.add(&content);

    let privacy = adw::PreferencesGroup::builder().title("History").build();
    add_search_history_rows(
        &privacy,
        s.save_search_history,
        |v| {
            settings::update(|s| {
                s.save_search_history = v;
                if !v {
                    s.search_history.clear();
                }
            })
        },
        || {
            settings::update(|s| s.search_history.clear());
            super::toast("Search history cleared");
        },
    );
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
