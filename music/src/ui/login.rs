//! Signing in: Google's sign-in page in the separate `sloptube-music-login`
//! process, or importing the session of a Firefox or Chromium profile.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::account::Account;
use crate::auth::{Cookie, Session, chromium, firefox};
use crate::image;
use crate::runtime;

/// The sign-in helper: next to this executable, or on `PATH`.
fn login_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SLOPTUBE_MUSIC_LOGIN") {
        return Some(p.into());
    }
    let sibling = std::env::current_exe()
        .ok()
        .map(|e| e.with_file_name("sloptube-music-login"))
        .filter(|p| p.is_file());
    sibling.or_else(|| glib::find_program_in_path("sloptube-music-login"))
}

fn alert(parent: &impl IsA<gtk::Widget>, title: &str, body: &str) {
    let a = adw::AlertDialog::new(Some(title), Some(body));
    a.add_response("ok", "OK");
    a.present(Some(parent));
}

async fn finish(dialog: &adw::Dialog, session: Session) {
    match Account::get().sign_in(session).await {
        Ok(()) => {
            dialog.close();
            let name = Account::get().name();
            super::toast(&if name.is_empty() {
                "Signed in".to_string()
            } else {
                format!("Signed in as {name}")
            });
        }
        Err(e) => alert(dialog, "Sign-in failed", &format!("{e:#}")),
    }
}

/// Run the sign-in window and wait for its cookies.
async fn run_helper(helper: PathBuf, process: &std::cell::RefCell<Option<gio::Subprocess>>) -> anyhow::Result<Option<Vec<Cookie>>> {
    let mut argv: Vec<std::ffi::OsString> = vec![helper.into()];
    if adw::StyleManager::default().is_dark() {
        argv.push("--dark".into());
    }
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(|a| a.as_os_str()).collect();
    let p = gio::Subprocess::newv(&args, gio::SubprocessFlags::STDOUT_PIPE)?;
    process.replace(Some(p.clone()));
    let (stdout, _) = p.communicate_utf8_future(None).await?;
    process.replace(None);
    let Some(line) = stdout.as_deref().and_then(|s| s.lines().find(|l| l.contains("\"cookies\""))) else {
        return Ok(None);
    };
    #[derive(serde::Deserialize)]
    struct Output {
        cookies: Vec<Cookie>,
    }
    Ok(Some(serde_json::from_str::<Output>(line)?.cookies))
}

fn import_row(dialog: &adw::Dialog, title: &str, subtitle: &str, source: &'static str, import: impl Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Vec<Cookie>>>>> + 'static) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("web-browser-symbolic"));
    let spinner = adw::Spinner::new();
    spinner.set_visible(false);
    row.add_suffix(&spinner);
    let import = std::rc::Rc::new(import);
    row.connect_activated(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        spinner,
        move |row| {
            row.set_sensitive(false);
            spinner.set_visible(true);
            let fut = import();
            let row = row.downgrade();
            glib::spawn_future_local(async move {
                let res = fut.await;
                if let Some(row) = row.upgrade() {
                    row.set_sensitive(true);
                }
                spinner.set_visible(false);
                match res {
                    Ok(cookies) => finish(&dialog, Session::new(cookies, source)).await,
                    Err(e) => alert(&dialog, "Import failed", &format!("{e:#}")),
                }
            });
        }
    ));
    row
}

pub fn present(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::Dialog::builder()
        .title("Sign in")
        .content_width(480)
        .content_height(640)
        .build();
    let page = adw::PreferencesPage::new();

    let intro = adw::PreferencesGroup::new();
    let status = adw::StatusPage::builder()
        .icon_name(crate::config::APP_ID)
        .title("Use your YouTube Music account")
        .description(
            "Your recommendations, library, likes and history come from your account. \
             The session is kept in your system keyring and only sent to YouTube.",
        )
        .build();
    status.add_css_class("compact");
    intro.add(&status);
    page.add(&intro);

    let google = adw::PreferencesGroup::new();
    match login_helper() {
        Some(helper) => {
            let button = gtk::Button::with_label("Sign in with Google");
            button.add_css_class("pill");
            button.add_css_class("suggested-action");
            button.set_halign(gtk::Align::Center);
            let waiting = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            waiting.set_halign(gtk::Align::Center);
            waiting.append(&adw::Spinner::new());
            waiting.append(&gtk::Label::new(Some("Finish signing in in the new window")));
            let cancel = gtk::Button::with_label("Cancel");
            cancel.add_css_class("flat");
            waiting.append(&cancel);
            waiting.set_visible(false);
            let process: std::rc::Rc<std::cell::RefCell<Option<gio::Subprocess>>> = Default::default();
            cancel.connect_clicked(glib::clone!(
                #[strong]
                process,
                move |_| {
                    if let Some(p) = process.borrow().as_ref() {
                        p.force_exit();
                    }
                }
            ));
            button.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                waiting,
                #[strong]
                process,
                move |b| {
                    b.set_visible(false);
                    waiting.set_visible(true);
                    let helper = helper.clone();
                    let process = process.clone();
                    let b = b.downgrade();
                    glib::spawn_future_local(async move {
                        let res = run_helper(helper, &process).await;
                        waiting.set_visible(false);
                        if let Some(b) = b.upgrade() {
                            b.set_visible(true);
                        }
                        match res {
                            Ok(Some(cookies)) => finish(&dialog, Session::new(cookies, "webkit")).await,
                            Ok(None) => {}
                            Err(e) => alert(&dialog, "Sign-in failed", &format!("{e:#}")),
                        }
                    });
                }
            ));
            google.add(&button);
            google.add(&waiting);
        }
        None => google.set_description(Some(
            "The sign-in window (sloptube-music-login) isn't installed; import a session from your browser below.",
        )),
    }
    page.add(&google);

    let import = adw::PreferencesGroup::new();
    import.set_title("Import from a browser");
    import.set_description(Some(
        "Reuses the YouTube session of a browser profile you're signed in to. It may stop working \
         when that browser rotates its cookies; signing in with Google gives SlopTube Music its own session.",
    ));
    let mut any = false;
    for profile in firefox::find_profiles() {
        any = true;
        let p = profile.clone();
        import.add(&import_row(
            &dialog,
            &profile.label(),
            &profile.cookies_db.display().to_string(),
            "firefox",
            move || {
                let p = p.clone();
                Box::pin(runtime::run(async move { firefox::import(&p).await }))
            },
        ));
    }
    for profile in chromium::find_profiles() {
        any = true;
        let p = profile.clone();
        import.add(&import_row(
            &dialog,
            &profile.label(),
            &profile.cookies_db.display().to_string(),
            "chromium",
            move || {
                let p = p.clone();
                Box::pin(runtime::run(async move { chromium::import(&p).await }))
            },
        ));
    }
    if !any {
        import.add(
            &adw::ActionRow::builder()
                .title("No supported browser found")
                .subtitle("Firefox, LibreWolf, Zen, Chromium, Chrome, Brave, Vivaldi and Edge profiles are supported")
                .build(),
        );
    }
    page.add(&import);

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&page));
    dialog.set_child(Some(&tv));
    dialog.present(Some(parent));
}

/// Forget the session, after asking.
pub fn confirm_sign_out(parent: &impl IsA<gtk::Widget>) {
    let alert = adw::AlertDialog::new(
        Some("Sign out?"),
        Some("SlopTube Music will forget your YouTube session. Your account itself is not affected."),
    );
    alert.add_response("cancel", "Cancel");
    alert.add_response("sign-out", "Sign Out");
    alert.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
    alert.connect_response(None, |_, resp| {
        if resp == "sign-out" {
            Account::get().sign_out();
            super::toast("Signed out");
        }
    });
    alert.present(Some(parent));
}

/// The avatar button in the header: sign in, or the account's menu.
pub fn account_button() -> gtk::MenuButton {
    let avatar = adw::Avatar::new(26, None, false);
    let button = gtk::MenuButton::builder().child(&avatar).tooltip_text("Account").build();
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("account-button");
    let update = glib::clone!(
        #[weak]
        button,
        #[weak]
        avatar,
        move |acc: &Account| {
            let menu = gio::Menu::new();
            if acc.signed_in() {
                avatar.set_show_initials(true);
                avatar.set_text(Some(&acc.name()));
                let url = acc.avatar();
                image::set_url(&avatar, (!url.is_empty()).then_some(url.as_str()), 96);
                let who = gio::Menu::new();
                let label = if acc.handle().is_empty() {
                    acc.name()
                } else {
                    format!("{} ({})", acc.name(), acc.handle())
                };
                who.append(Some(&label), Some("win.show-account"));
                menu.append_section(None, &who);
                menu.append(Some("Sign Out"), Some("win.sign-out"));
            } else {
                avatar.set_show_initials(false);
                avatar.set_text(None);
                image::set_url(&avatar, None, 0);
                menu.append(Some("Sign In…"), Some("win.sign-in"));
            }
            button.set_menu_model(Some(&menu));
        }
    );
    let acc = Account::get();
    update(&acc);
    for prop in ["signed-in", "name", "avatar"] {
        let update = update.clone();
        let id = acc.connect_notify_local(Some(prop), move |a, _| update(a));
        // Disconnect when the button goes away (pages come and go).
        let id = std::cell::Cell::new(Some(id));
        button.connect_destroy(move |_| {
            if let Some(id) = id.take() {
                Account::get().disconnect(id);
            }
        });
    }
    button
}
