//! `sloptube-music-login`: Google sign-in for SlopTube Music.
//!
//! A separate process, so the player itself never loads WebKit: SlopTube
//! Music starts it when you choose "Sign in with Google", and it shows
//! Google's sign-in page in a throwaway in-memory browser session. Once the
//! page lands back on YouTube with a signed-in session, it prints the YouTube
//! and Google cookies as one JSON line on stdout and exits. It doesn't read
//! what you type. Closing the window exits without printing anything.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use webkit::prelude::*;

const SIGN_IN_URL: &str = "https://accounts.google.com/ServiceLogin?service=youtube&uilel=3&passive=true&hl=en\
    &continue=https%3A%2F%2Fwww.youtube.com%2Fsignin%3Faction_handle_signin%3Dtrue%26app%3Ddesktop%26hl%3Den\
    %26next%3Dhttps%253A%252F%252Fmusic.youtube.com%252F";

fn signed_in(cookies: &[serde_json::Value]) -> bool {
    let has = |name: &str| cookies.iter().any(|c| c["name"] == name);
    (has("SAPISID") || has("__Secure-3PAPISID")) && (has("SID") || has("__Secure-3PSID") || has("__Secure-1PSID"))
}

fn build(app: &adw::Application) {
    let network = webkit::NetworkSession::new_ephemeral();
    let view = webkit::WebView::builder().network_session(&network).build();
    if let Some(settings) = WebViewExt::settings(&view) {
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

    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&overlay));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Sign in to YouTube Music")
        .default_width(480)
        .default_height(720)
        .content(&toolbar)
        .build();

    let done = Rc::new(Cell::new(false));
    view.connect_load_changed(glib::clone!(
        #[weak]
        window,
        #[weak]
        network,
        move |view, event| {
            if event != webkit::LoadEvent::Finished || done.get() {
                return;
            }
            let Some(uri) = view.uri() else { return };
            let host = url::Url::parse(&uri).ok().and_then(|u| u.host_str().map(str::to_owned));
            if !matches!(
                host.as_deref(),
                Some("music.youtube.com" | "www.youtube.com" | "m.youtube.com" | "youtube.com")
            ) {
                return;
            }
            let Some(cm) = network.cookie_manager() else { return };
            let done = done.clone();
            cm.all_cookies(gio::Cancellable::NONE, move |res| {
                let Ok(list) = res else { return };
                let cookies: Vec<serde_json::Value> = list
                    .into_iter()
                    .filter_map(|mut c| {
                        let domain = c.domain()?.to_string();
                        let d = domain.trim_start_matches('.');
                        let wanted = d == "youtube.com"
                            || d.ends_with(".youtube.com")
                            || d == "google.com"
                            || d.ends_with(".google.com");
                        wanted.then(|| {
                            serde_json::json!({
                                "domain": domain,
                                "name": c.name().map(|s| s.to_string()).unwrap_or_default(),
                                "value": c.value().map(|s| s.to_string()).unwrap_or_default(),
                                "path": c.path().map(|p| p.to_string()).unwrap_or_else(|| "/".into()),
                                "expires": c.expires().map(|d| d.to_unix()),
                                "secure": c.is_secure(),
                                "http_only": c.is_http_only(),
                            })
                        })
                    })
                    .collect();
                if !signed_in(&cookies) {
                    return;
                }
                done.set(true);
                println!("{}", serde_json::json!({ "cookies": cookies }));
                window.close();
            });
        }
    ));
    view.load_uri(SIGN_IN_URL);
    window.present();
}

fn main() -> glib::ExitCode {
    // libadwaita ignores its stylesheet when GTK_THEME is set.
    if let Ok(theme) = std::env::var("GTK_THEME")
        && (theme == "Adwaita" || theme.starts_with("Adwaita:")) {
            // SAFETY: no other threads have been started yet.
            unsafe { std::env::remove_var("GTK_THEME") };
        }
    let app = adw::Application::builder()
        .application_id("io.github.nergzd.SlopTubeMusic.Login")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| {
        if std::env::args().any(|a| a == "--dark") {
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        }
    });
    app.connect_activate(build);
    app.run_with_args(&[] as &[&str])
}
