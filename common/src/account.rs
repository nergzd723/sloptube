//! The Google account both apps use, as a GObject the UI binds to.
//!
//! There is one session for the process (`crate::session`), stored once in
//! the keyring (`crate::auth`). Signing in or out in either app does so for
//! both. The account is checked with SlopTube's client (`innertube::web()`).

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::auth::{self, Session};
use crate::innertube::{client, model::AccountInfo};
use crate::{runtime, session};

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
        pub loaded: Cell<bool>,
        pub on_changed: RefCell<Vec<Box<dyn Fn()>>>,
        pub on_expired: RefCell<Vec<Box<dyn Fn()>>>,
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

/// What YouTube says about a session.
enum Check {
    Valid(AccountInfo),
    SignedOut,
    /// No answer (network trouble): keep the session and try again later.
    Unknown,
}

impl Account {
    pub fn get() -> Account {
        ACCOUNT.with(|a| a.clone())
    }

    /// Called when the session changes: loaded, signed in or out, cookies rotated.
    pub fn connect_session_changed(&self, f: impl Fn() + 'static) {
        self.imp().on_changed.borrow_mut().push(Box::new(f));
    }

    /// Called when YouTube no longer accepts the stored session.
    pub fn connect_expired(&self, f: impl Fn() + 'static) {
        self.imp().on_expired.borrow_mut().push(Box::new(f));
    }

    fn changed(&self) {
        for f in self.imp().on_changed.borrow().iter() {
            f();
        }
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

    /// Restore the stored session at startup (once; later calls do nothing).
    ///
    /// Music stored its own session before the apps shared one: if SlopTube's
    /// isn't there or YouTube rejects it, Music's is tried, and whichever is
    /// kept is stored as the shared one. Music's is deleted only once a session
    /// has been accepted, never on network trouble.
    pub fn load_stored(&self) {
        if self.imp().loaded.replace(true) {
            return;
        }
        self.start_cookie_saver();
        if !auth::may_have_session() {
            return;
        }
        self.set_busy(true);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let (current, legacy) = runtime::run(auth::load_with_legacy()).await;
            let had_legacy = legacy.is_some();
            let candidates = [current, legacy].into_iter().flatten().filter(Session::is_signed_in);
            let (mut kept, mut accepted, mut rejected) = (false, false, false);
            for s in candidates {
                session::set(Some(s.clone()));
                this.apply_session(Some(&s));
                match this.check().await {
                    Check::Valid(info) => {
                        this.adopt(info);
                        (kept, accepted) = (true, true);
                        break;
                    }
                    Check::Unknown => {
                        kept = true;
                        break;
                    }
                    Check::SignedOut => {
                        log::warn!("YouTube reports a stored session as signed out");
                        rejected = true;
                    }
                }
            }
            if accepted && had_legacy {
                runtime::spawn(auth::clear_legacy());
            }
            if !kept {
                session::set(None);
                this.apply_session(None);
                if rejected {
                    runtime::spawn(auth::clear());
                    runtime::spawn(auth::clear_legacy());
                    for f in this.imp().on_expired.borrow().iter() {
                        f();
                    }
                }
            }
            auth::set_has_session(kept);
            this.set_busy(false);
            this.changed();
        });
    }

    /// Keep cookies Google rotates; otherwise the session expires sooner.
    fn start_cookie_saver(&self) {
        let this = self.downgrade();
        glib::timeout_add_seconds_local(60, move || {
            if let Some(s) = session::take_dirty() {
                runtime::spawn(async move {
                    if let Err(e) = auth::store(&s).await {
                        log::warn!("saving session failed: {e:#}");
                    }
                });
                if let Some(this) = this.upgrade() {
                    this.changed();
                }
            }
            glib::ControlFlow::Continue
        });
    }

    /// Adopt a freshly obtained session (from the sign-in window or a browser import).
    pub async fn sign_in(&self, new: Session) -> anyhow::Result<()> {
        if !new.is_signed_in() {
            anyhow::bail!("No signed-in YouTube session was found");
        }
        session::set(Some(new.clone()));
        self.apply_session(Some(&new));
        match self.check().await {
            Check::Valid(info) => self.adopt(info),
            Check::SignedOut => {
                session::set(None);
                self.apply_session(None);
                anyhow::bail!("YouTube didn't accept this session");
            }
            // Keep it; the name and picture come at the next start.
            Check::Unknown => self.save(),
        }
        auth::set_has_session(true);
        self.changed();
        Ok(())
    }

    async fn check(&self) -> Check {
        match runtime::run(async { client::web().account_info().await }).await {
            Ok(Some(info)) => Check::Valid(info),
            Ok(None) => Check::SignedOut,
            Err(e) => {
                log::warn!("account info failed: {e:#}");
                Check::Unknown
            }
        }
    }

    /// Note the account's name and picture and store the session.
    fn adopt(&self, info: AccountInfo) {
        session::set_account(info);
        self.apply_session(session::get().as_ref());
        self.save();
    }

    fn save(&self) {
        if let Some(s) = session::get() {
            runtime::spawn(async move {
                if let Err(e) = auth::store(&s).await {
                    log::warn!("saving session failed: {e:#}");
                }
            });
        }
    }

    /// Sign out (of both apps).
    pub fn sign_out(&self) {
        session::set(None);
        self.apply_session(None);
        auth::set_has_session(false);
        runtime::spawn(auth::clear());
        self.changed();
    }
}
