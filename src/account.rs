//! Signed-in state shared across the UI.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::api;
use crate::auth::{self, Session};
use crate::runtime;

mod imp {
    use super::*;

    #[derive(glib::Properties, Default)]
    #[properties(wrapper_type = super::Account)]
    pub struct Account {
        #[property(get)]
        pub signed_in: Cell<bool>,
        /// The stored session is being loaded/validated.
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
        const NAME: &'static str = "SlopAccount";
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
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let session = runtime::run(auth::load()).await;
            if let Some(s) = session.filter(Session::is_signed_in) {
                api::client().set_session(Some(s.clone()));
                this.apply_session(Some(&s));
                this.refresh_info(false).await;
            }
            this.set_busy(false);
        });
        // Persist cookie rotations every so often.
        glib::timeout_add_seconds_local(60, || {
            if let Some(s) = api::client().take_dirty_session() {
                runtime::spawn(async move {
                    if let Err(e) = auth::store(&s).await {
                        log::warn!("saving session failed: {e:#}");
                    }
                });
            }
            glib::ControlFlow::Continue
        });
    }

    /// Adopt a freshly obtained session (from the login window or a browser import).
    pub async fn sign_in(&self, session: Session) -> anyhow::Result<()> {
        if !session.is_signed_in() {
            anyhow::bail!("No signed-in YouTube session was found");
        }
        api::client().set_session(Some(session.clone()));
        self.apply_session(Some(&session));
        if !self.refresh_info(true).await {
            api::client().set_session(None);
            self.apply_session(None);
            anyhow::bail!("YouTube didn't accept this session");
        }
        Ok(())
    }

    /// Fetch name/avatar; returns false if YouTube says we're logged out.
    async fn refresh_info(&self, fresh: bool) -> bool {
        let res = runtime::run(async move { api::client().account_info().await }).await;
        match res {
            Ok(Some(info)) => {
                let mut session = api::client().session().unwrap_or_default();
                session.account = Some(info);
                api::client().set_session(Some(session.clone()));
                self.apply_session(Some(&session));
                let s = session.clone();
                runtime::spawn(async move {
                    if let Err(e) = auth::store(&s).await {
                        log::warn!("saving session failed: {e:#}");
                    }
                });
                true
            }
            Ok(None) => {
                log::warn!("YouTube reports this session as signed out");
                if !fresh {
                    crate::ui::main_window().map(|w| w.toast("Your YouTube session expired. Please sign in again."));
                    self.sign_out();
                }
                false
            }
            Err(e) => {
                // Network trouble: keep the session, try again later.
                log::warn!("account info failed: {e:#}");
                fresh
            }
        }
    }

    pub fn sign_out(&self) {
        api::client().set_session(None);
        self.apply_session(None);
        runtime::spawn(async move { auth::clear().await });
    }
}
