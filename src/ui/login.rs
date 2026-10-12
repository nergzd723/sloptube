//! Signing in: an in-app Google login window (WebKitGTK) or importing a browser session.

use adw::prelude::*;
use gtk::glib;

use crate::account::Account;
use crate::auth::{self, Session, chromium};
use crate::runtime;

#[cfg(all(feature = "webkit", target_os = "linux"))]
const SIGN_IN_URL: &str = "https://accounts.google.com/ServiceLogin?service=youtube&uilel=3&passive=true&hl=en&continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue%26app%3Ddesktop%26hl%3Den%26next%3D%252F";

async fn finish(dialog: &adw::Dialog, session: Session) {
    match Account::get().sign_in(session).await {
        Ok(()) => {
            dialog.close();
            let name = Account::get().name();
            super::toast(dialog, &if name.is_empty() {
                "Signed in".to_string()
            } else {
                format!("Signed in as {name}")
            });
        }
        Err(e) => {
            let alert = adw::AlertDialog::new(Some("Sign-in failed"), Some(&e.to_string()));
            alert.add_response("ok", "OK");
            alert.present(Some(dialog));
        }
    }
}

#[cfg(all(feature = "webkit", target_os = "linux"))]
fn webkit_page(dialog: &adw::Dialog) -> adw::NavigationPage {
    use crate::auth::Cookie;
    use webkit::prelude::*;

    let network = webkit::NetworkSession::new_ephemeral();
    let view = webkit::WebView::builder().network_session(&network).build();
    if let Some(settings) = webkit::prelude::WebViewExt::settings(&view) {
        settings.set_enable_developer_extras(false);
        settings.set_javascript_can_open_windows_automatically(false);
    }
    view.set_vexpand(true);
    view.set_hexpand(true);

    let progress = gtk::ProgressBar::new();
    progress.add_css_class("osd");
    progress.set_valign(gtk::Align::Start);
    view.connect_estimated_load_progress_notify(glib::clone!(
        #[weak]
        progress,
        move |v| {
            let p = v.estimated_load_progress();
            progress.set_fraction(p);
            progress.set_visible(p < 1.0);
        }
    ));
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&view));
    overlay.add_overlay(&progress);

    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    view.connect_load_changed(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        network,
        move |view, event| {
            if event != webkit::LoadEvent::Finished || done.get() {
                return;
            }
            let Some(uri) = view.uri() else { return };
            let host = url::Url::parse(&uri).ok().and_then(|u| u.host_str().map(str::to_owned));
            if !matches!(host.as_deref(), Some("www.youtube.com" | "m.youtube.com" | "youtube.com")) {
                return;
            }
            let Some(cm) = network.cookie_manager() else { return };
            let done = done.clone();
            cm.all_cookies(gtk::gio::Cancellable::NONE, move |res| {
                let Ok(list) = res else { return };
                let cookies: Vec<Cookie> = list
                    .into_iter()
                    .filter_map(|mut c| {
                        Some(Cookie {
                            domain: c.domain()?.to_string(),
                            name: c.name()?.to_string(),
                            value: c.value()?.to_string(),
                            path: c.path().map(|p| p.to_string()).unwrap_or_else(|| "/".into()),
                            expires: c.expires().map(|d| d.to_unix()),
                            secure: c.is_secure(),
                            http_only: c.is_http_only(),
                        })
                    })
                    .collect();
                let session = Session::new(cookies, "webkit");
                if !session.is_signed_in() {
                    return;
                }
                done.set(true);
                glib::spawn_future_local(async move { finish(&dialog, session).await });
            });
        }
    ));
    view.load_uri(SIGN_IN_URL);

    let header = adw::HeaderBar::new();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&overlay));
    adw::NavigationPage::builder()
        .title("Sign in with Google")
        .child(&tv)
        .build()
}

pub fn present(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::Dialog::builder()
        .title("Sign in")
        .content_width(480)
        .content_height(680)
        .build();
    let nav = adw::NavigationView::new();

    let page = adw::PreferencesPage::new();
    let intro = adw::PreferencesGroup::new();
    let status = adw::StatusPage::builder()
        .icon_name(crate::config::APP_ID)
        .title("Use your YouTube account")
        .description("Your home feed, subscriptions, history, likes and Watch later come from your account. The session is kept in your system keyring and only sent to YouTube.")
        .build();
    status.add_css_class("compact");
    intro.add(&status);
    page.add(&intro);

    let google = adw::PreferencesGroup::new();
    #[cfg(all(feature = "webkit", target_os = "linux"))]
    {
        let button = gtk::Button::with_label("Sign in with Google");
        button.add_css_class("pill");
        button.add_css_class("suggested-action");
        button.set_halign(gtk::Align::Center);
        button.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            #[weak]
            nav,
            move |_| nav.push(&webkit_page(&dialog))
        ));
        google.add(&button);
    }
    #[cfg(not(all(feature = "webkit", target_os = "linux")))]
    {
        google.set_description(Some("This build has no built-in browser; import a session from your browser below."));
    }
    page.add(&google);

    let import = adw::PreferencesGroup::new();
    import.set_title("Import from a browser");
    import.set_description(Some(
        "Reuses the YouTube session of a browser profile you're signed in to. It may stop working when that browser rotates its cookies; signing in above gives SlopTube its own session.",
    ));
    let profiles = chromium::find_profiles();
    if profiles.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No supported browser found")
            .subtitle("Chromium, Chrome, Brave, Vivaldi and Edge profiles are supported")
            .build();
        import.add(&row);
    }
    for profile in profiles {
        let row = adw::ActionRow::builder()
            .title(profile.label())
            .subtitle(profile.cookies_db.display().to_string())
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("web-browser-symbolic"));
        let spinner = adw::Spinner::new();
        spinner.set_visible(false);
        row.add_suffix(&spinner);
        row.connect_activated(glib::clone!(
            #[weak]
            dialog,
            #[weak]
            spinner,
            move |row| {
                row.set_sensitive(false);
                spinner.set_visible(true);
                let profile = profile.clone();
                let row = row.downgrade();
                glib::spawn_future_local(async move {
                    let res = runtime::run(async move { chromium::import(&profile).await }).await;
                    if let Some(row) = row.upgrade() {
                        row.set_sensitive(true);
                    }
                    spinner.set_visible(false);
                    match res {
                        Ok(cookies) => finish(&dialog, Session::new(cookies, "chromium")).await,
                        Err(e) => {
                            let alert = adw::AlertDialog::new(Some("Import failed"), Some(&format!("{e:#}")));
                            alert.add_response("ok", "OK");
                            alert.present(Some(&dialog));
                        }
                    }
                });
            }
        ));
        import.add(&row);
    }
    page.add(&import);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&page));
    let root = adw::NavigationPage::builder().title("Sign in").child(&tv).build();
    nav.add(&root);
    dialog.set_child(Some(&nav));
    dialog.present(Some(parent));
}

/// Forget the session (with confirmation).
pub fn confirm_sign_out(parent: &impl IsA<gtk::Widget>) {
    let alert = adw::AlertDialog::new(
        Some("Sign out?"),
        Some("SlopTube will forget your YouTube session. Your account itself is not affected."),
    );
    alert.add_response("cancel", "Cancel");
    alert.add_response("sign-out", "Sign Out");
    alert.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
    alert.connect_response(None, |_, resp| {
        if resp == "sign-out" {
            Account::get().sign_out();
            let _ = auth::cookie_file_path();
        }
    });
    alert.present(Some(parent));
}
