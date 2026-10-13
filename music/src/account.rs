//! The signed-in state, as a GObject the UI binds to.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::api;
use crate::auth::{self, Session};
use crate::extractor::extractor;
use crate::runtime;
use crate::settings;

mod imp {
    use super::*;

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::Account)]
    pub struct Account {
        #[property(get)]
        pub signed_in: Cell<bool>,
        /// The stored session is being loaded or checked.
        #[property(get)]
        pub busy: Cell<bool>,
        #[property(get)]
        pub name: RefCell<String>,
        #[property(get)]
        pub handle: RefCell<String>,
        #[property(get)]
        pub avatar: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Account {
        const NAME: &'static str = "StmAccount";
        type Type = super::Account;
    }

    #[glib::derived_properties]
    impl ObjectImpl for Account {}
}

glib::wrapper! {
    pub struct Account(ObjectSubclass<imp::Account>);
}

thread_local! {
    static ACCOUNT: Account = glib::Object::new();
}

/// Hand the account's cookies to yt-dlp (or stop doing so).
pub fn update_stream_cookies() {
    let session = api::client().session().filter(Session::is_signed_in);
    let path = match session {
        Some(s) if settings::get().use_account_for_streams => match auth::write_cookie_file(&s) {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn!("cannot write the cookie file for yt-dlp: {e:#}");
                None
            }
        },
        _ => {
            let _ = std::fs::remove_file(auth::cookie_file_path());
            None
        }
    };
    extractor().set_cookie_file(path);
}

impl Account {
    pub fn get() -> Account {
        ACCOUNT.with(|a| a.clone())
    }

    fn set_busy(&self, b: bool) {
        self.imp().busy.set(b);
        self.notify_busy();
    }

    fn apply_session(&self, session: Option<&Session>) {
        let imp = self.imp();
        let signed_in = session.is_some_and(Session::is_signed_in);
        let info = session.and_then(|s| s.account.clone()).unwrap_or_default();
        imp.name.replace(info.name);
        imp.handle.replace(info.handle.unwrap_or_default());
        imp.avatar.replace(info.avatar.unwrap_or_default());
        self.notify_name();
        self.notify_handle();
        self.notify_avatar();
        if imp.signed_in.get() != signed_in {
            imp.signed_in.set(signed_in);
            self.notify_signed_in();
        }
    }

    /// Restore the stored session at startup.
    pub fn load_stored(&self) {
        // Never signed in: don't wake the keyring (it may ask to be unlocked).
        let has_session = settings::get().has_session || auth::fallback_exists();
        if !has_session {
            self.start_cookie_saver();
            return;
        }
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let session = runtime::run(auth::load()).await;
            if let Some(s) = session.filter(Session::is_signed_in) {
                api::client().set_session(Some(s.clone()));
                update_stream_cookies();
                this.apply_session(Some(&s));
                this.refresh_info(false).await;
            }
            this.set_busy(false);
        });
        self.start_cookie_saver();
    }

    /// Keep cookies Google rotates; otherwise the session expires sooner.
    fn start_cookie_saver(&self) {
        glib::timeout_add_seconds_local(60, || {
            if let Some(s) = api::client().take_dirty_session() {
                update_stream_cookies();
                runtime::spawn(async move {
                    if let Err(e) = auth::store(&s).await {
                        log::warn!("saving the session failed: {e:#}");
                    }
                });
            }
            glib::ControlFlow::Continue
        });
    }

    /// Adopt a new session (from the sign-in window or a browser import).
    pub async fn sign_in(&self, session: Session) -> anyhow::Result<()> {
        if !session.is_signed_in() {
            anyhow::bail!("No signed-in YouTube session was found");
        }
        api::client().set_session(Some(session.clone()));
        self.apply_session(Some(&session));
        if !self.refresh_info(true).await {
            api::client().set_session(None);
            self.apply_session(None);
            update_stream_cookies();
            anyhow::bail!("YouTube didn't accept this session");
        }
        update_stream_cookies();
        settings::update(|s| s.has_session = true);
        Ok(())
    }

    /// Fetch the name and picture; false if YouTube says the session is signed out.
    async fn refresh_info(&self, fresh: bool) -> bool {
        let res = runtime::run(async move { api::client().account_info().await }).await;
        match res {
            Ok(Some(info)) => {
                let mut session = api::client().session().unwrap_or_default();
                session.account = Some(info);
                api::client().set_session(Some(session.clone()));
                self.apply_session(Some(&session));
                runtime::spawn(async move {
                    if let Err(e) = auth::store(&session).await {
                        log::warn!("saving the session failed: {e:#}");
                    }
                });
                true
            }
            Ok(None) => {
                log::warn!("YouTube reports this session as signed out");
                if !fresh {
                    crate::ui::toast("Your YouTube session expired. Please sign in again.");
                    self.sign_out();
                }
                false
            }
            Err(e) => {
                // Network trouble: keep the session and try again next time.
                log::warn!("account info failed: {e:#}");
                fresh
            }
        }
    }

    pub fn sign_out(&self) {
        settings::update(|s| s.has_session = false);
        api::client().set_session(None);
        self.apply_session(None);
        update_stream_cookies();
        runtime::spawn(async move { auth::clear().await });
    }
}
