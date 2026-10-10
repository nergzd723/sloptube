//! Preferences.

use adw::prelude::*;

use crate::api;
use crate::config;
use crate::library::Library;
use crate::settings::{self, AudioQuality, ColorScheme};

pub fn apply_color_scheme(scheme: ColorScheme) {
    let sm = adw::StyleManager::default();
    sm.set_color_scheme(match scheme {
        ColorScheme::Dark => adw::ColorScheme::ForceDark,
        ColorScheme::Light => adw::ColorScheme::ForceLight,
        ColorScheme::System if config::GTK_THEME_DARK.load(std::sync::atomic::Ordering::Relaxed) => {
            adw::ColorScheme::PreferDark
        }
        ColorScheme::System => adw::ColorScheme::Default,
    });
}

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

    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    let schemes = [ColorScheme::Dark, ColorScheme::System, ColorScheme::Light];
    let style = adw::ComboRow::builder()
        .title("Style")
        .model(&gtk::StringList::new(&["Dark", "Follow system", "Light"]))
        .selected(schemes.iter().position(|c| *c == s.color_scheme).unwrap_or(0) as u32)
        .build();
    style.connect_selected_notify(move |r| {
        let scheme = schemes[(r.selected() as usize).min(2)];
        settings::update(|s| s.color_scheme = scheme);
        apply_color_scheme(scheme);
    });
    appearance.add(&style);
    page.add(&appearance);

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
