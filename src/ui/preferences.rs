//! Preferences dialog.

use adw::prelude::*;
use gtk::{gio, glib};

use crate::account::Account;
use crate::api;
use crate::player::decoders;
use crate::settings::{self, CodecPref, ColorScheme, DownloadQuality, HEIGHTS};

fn switch_row(title: &str, subtitle: &str, active: bool, f: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(active).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.connect_active_notify(move |r| f(r.is_active()));
    row
}

fn combo_row(title: &str, items: &[String], selected: usize, f: impl Fn(usize) + 'static) -> adw::ComboRow {
    let strs: Vec<&str> = items.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&strs))
        .selected(selected as u32)
        .build();
    row.connect_selected_notify(move |r| f(r.selected() as usize));
    row
}

pub fn apply_color_scheme(scheme: ColorScheme) {
    let sm = adw::StyleManager::default();
    let env_dark = crate::config::GTK_THEME_DARK.load(std::sync::atomic::Ordering::Relaxed);
    sm.set_color_scheme(match scheme {
        ColorScheme::System if env_dark => adw::ColorScheme::PreferDark,
        ColorScheme::System => adw::ColorScheme::Default,
        ColorScheme::Light => adw::ColorScheme::ForceLight,
        ColorScheme::Dark => adw::ColorScheme::ForceDark,
    });
}

pub fn present(parent: &impl IsA<gtk::Widget>) {
    let s = settings::get();
    let dialog = adw::PreferencesDialog::new();
    dialog.set_search_enabled(true);

    // ---- Playback ----------------------------------------------------------
    let playback = adw::PreferencesPage::builder()
        .title("Playback")
        .icon_name("media-playback-start-symbolic")
        .build();
    let video = adw::PreferencesGroup::builder().title("Video").build();
    let codecs: Vec<String> = CodecPref::ALL.iter().map(|c| c.label().to_owned()).collect();
    video.add(&combo_row(
        "Preferred codec",
        &codecs,
        CodecPref::ALL.iter().position(|c| *c == s.codec).unwrap_or(0),
        |i| settings::update(|s| s.codec = CodecPref::ALL[i]),
    ));
    let heights: Vec<String> = HEIGHTS.iter().map(|h| format!("{h}p")).collect();
    video.add(&combo_row(
        "Maximum quality",
        &heights,
        HEIGHTS.iter().position(|h| *h == s.max_height).unwrap_or(2),
        |i| settings::update(|s| s.max_height = HEIGHTS[i]),
    ));
    video.add(&switch_row("Allow 60 fps", "", s.allow_60fps, |v| {
        settings::update(|s| s.allow_60fps = v)
    }));
    let support = decoders::probe();
    let hw = switch_row(
        "Hardware decoding",
        &format!("VA-API and V4L2 decoders. Found {}", support.summary()),
        s.hw_decode,
        |v| {
            settings::update(|s| s.hw_decode = v);
            if let Some(win) = super::main_window() {
                win.playback().refresh_decoders();
            }
        },
    );
    hw.set_subtitle_lines(4);
    video.add(&hw);
    playback.add(&video);

    let behaviour = adw::PreferencesGroup::builder().title("Behaviour").build();
    behaviour.add(&switch_row("Autoplay", "Play a related video when one ends", s.autoplay, |v| {
        settings::update(|s| s.autoplay = v)
    }));
    behaviour.add(&switch_row(
        "Resume playback",
        "Continue videos where you left off",
        s.resume_playback,
        |v| settings::update(|s| s.resume_playback = v),
    ));
    behaviour.add(&switch_row(
        "Keep playing in the background",
        "Closing the window while something plays keeps the audio going; use media keys or reopen SlopTube",
        s.background_playback,
        |v| settings::update(|s| s.background_playback = v),
    ));
    playback.add(&behaviour);
    dialog.add(&playback);

    // ---- Account -----------------------------------------------------------
    let account_page = adw::PreferencesPage::builder()
        .title("Account")
        .icon_name("avatar-default-symbolic")
        .build();
    let acc = adw::PreferencesGroup::new();
    let account = Account::get();
    let status = adw::ActionRow::builder()
        .title(if account.signed_in() {
            format!("Signed in as {}", account.name())
        } else {
            "Not signed in".to_string()
        })
        .subtitle(account.handle())
        .build();
    let button = gtk::Button::with_label(if account.signed_in() { "Sign Out" } else { "Sign In" });
    button.set_valign(gtk::Align::Center);
    if account.signed_in() {
        button.add_css_class("destructive-action");
    } else {
        button.add_css_class("suggested-action");
    }
    button.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            if Account::get().signed_in() {
                super::login::confirm_sign_out(&dialog);
            } else {
                dialog.close();
                if let Some(win) = super::main_window() {
                    super::login::present(&win);
                }
            }
        }
    ));
    status.add_suffix(&button);
    acc.add(&status);
    acc.add(&switch_row(
        "Use account for video streams",
        "Needed for age-restricted and members-only videos and Premium quality",
        s.use_account_for_streams,
        |v| settings::update(|s| s.use_account_for_streams = v),
    ));
    acc.add(&switch_row(
        "Add watched videos to YouTube history",
        "Also keeps your resume positions in sync with other devices",
        s.send_watch_history,
        |v| settings::update(|s| s.send_watch_history = v),
    ));
    account_page.add(&acc);
    dialog.add(&account_page);

    // ---- General -----------------------------------------------------------
    let general = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("emblem-system-symbolic")
        .build();
    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    let schemes = ["Follow system", "Light", "Dark"].map(String::from);
    let scheme_list = [ColorScheme::System, ColorScheme::Light, ColorScheme::Dark];
    appearance.add(&combo_row(
        "Style",
        &schemes,
        scheme_list.iter().position(|c| *c == s.color_scheme).unwrap_or(0),
        move |i| {
            settings::update(|s| s.color_scheme = scheme_list[i]);
            apply_color_scheme(scheme_list[i]);
        },
    ));
    general.add(&appearance);

    let content = adw::PreferencesGroup::builder().title("Content").build();
    let region = adw::EntryRow::builder()
        .title("Region code (e.g. US, DE, UA — empty for automatic)")
        .text(s.region.clone().unwrap_or_default())
        .show_apply_button(true)
        .build();
    region.connect_apply(|r| {
        let v = r.text().trim().to_ascii_uppercase();
        let v = (!v.is_empty()).then_some(v);
        settings::update(|s| s.region = v.clone());
        api::client().set_region(v);
    });
    content.add(&region);
    content.add(&switch_row("Remember searches", "", s.save_search_history, |v| {
        settings::update(|s| {
            s.save_search_history = v;
            if !v {
                s.search_history.clear();
            }
        })
    }));
    general.add(&content);

    let dl = adw::PreferencesGroup::builder().title("Downloads").build();
    let folder = adw::ActionRow::builder()
        .title("Folder")
        .subtitle(s.download_dir().display().to_string())
        .build();
    let choose = gtk::Button::from_icon_name("folder-open-symbolic");
    choose.set_valign(gtk::Align::Center);
    choose.add_css_class("flat");
    choose.connect_clicked(glib::clone!(
        #[weak]
        folder,
        move |b| {
            let fd = gtk::FileDialog::builder().title("Download folder").build();
            fd.set_initial_folder(Some(&gio::File::for_path(settings::get().download_dir())));
            let win = b.root().and_downcast::<gtk::Window>();
            fd.select_folder(win.as_ref(), gio::Cancellable::NONE, move |res| {
                if let Ok(f) = res {
                    if let Some(p) = f.path() {
                        folder.set_subtitle(&p.display().to_string());
                        settings::update(|s| s.download_dir = Some(p));
                    }
                }
            });
        }
    ));
    folder.add_suffix(&choose);
    dl.add(&folder);
    let qualities: Vec<String> = DownloadQuality::ALL.iter().map(|q| q.label().to_owned()).collect();
    dl.add(&combo_row(
        "Quality",
        &qualities,
        DownloadQuality::ALL
            .iter()
            .position(|q| *q == s.download_quality)
            .unwrap_or(1),
        |i| settings::update(|s| s.download_quality = DownloadQuality::ALL[i]),
    ));
    general.add(&dl);
    dialog.add(&general);

    dialog.present(Some(parent));
}
