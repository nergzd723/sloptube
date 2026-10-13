//! The session in memory, shared by every InnerTube client in the process.
//!
//! Google rotates some cookies (`*PSIDTS`, ...) through `Set-Cookie` on API
//! responses. Two clients with their own copies would each keep only their
//! own rotations and overwrite each other's when saving, so there is one jar:
//! both clients sign their requests from it and put what Google sends back
//! into it, and the account saves it now and then (`take_dirty`).

use std::sync::RwLock;

use reqwest::header::{self, HeaderMap, HeaderValue};

use crate::auth::Session;

struct Store {
    session: Option<Session>,
    /// Cookies changed since the session was last saved.
    dirty: bool,
    /// Bumped whenever the identity changes (sign-in, sign-out).
    generation: u64,
}

static STORE: RwLock<Store> = RwLock::new(Store {
    session: None,
    dirty: false,
    generation: 0,
});

/// Replace the session (a new identity).
pub fn set(session: Option<Session>) {
    let mut st = STORE.write().unwrap();
    st.session = session;
    st.dirty = false;
    st.generation += 1;
}

/// Update the session's account details without changing the identity.
pub fn set_account(info: crate::innertube::model::AccountInfo) {
    if let Some(s) = STORE.write().unwrap().session.as_mut() {
        s.account = Some(info);
    }
}

pub fn get() -> Option<Session> {
    STORE.read().unwrap().session.clone()
}

/// A number that changes whenever the identity does.
pub fn generation() -> u64 {
    STORE.read().unwrap().generation
}

pub fn is_signed_in() -> bool {
    STORE.read().unwrap().session.as_ref().is_some_and(Session::is_signed_in)
}

/// The session, if its cookies changed since the last call (for saving).
pub fn take_dirty() -> Option<Session> {
    let mut st = STORE.write().unwrap();
    if st.dirty {
        st.dirty = false;
        st.session.clone()
    } else {
        None
    }
}

/// Sign a request to `host` sent from `origin`: cookies and `SAPISIDHASH`.
pub fn sign(h: &mut HeaderMap, host: &str, origin: &'static str) {
    let st = STORE.read().unwrap();
    let Some(s) = st.session.as_ref().filter(|s| s.is_signed_in()) else {
        return;
    };
    if let Ok(v) = HeaderValue::from_str(&s.cookie_header(host)) {
        h.insert(header::COOKIE, v);
    }
    if let Some(a) = s.authorization(origin).and_then(|a| HeaderValue::from_str(&a).ok()) {
        h.insert(header::AUTHORIZATION, a);
    }
    h.insert("X-Goog-AuthUser", HeaderValue::from_static("0"));
    h.insert("X-Origin", HeaderValue::from_static(origin));
    h.insert("X-Youtube-Bootstrap-Logged-In", HeaderValue::from_static("true"));
}

/// Keep cookies Google refreshed in a response from `host`.
pub fn absorb(headers: &HeaderMap, host: &str) {
    let set: Vec<&str> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if set.is_empty() {
        return;
    }
    let mut st = STORE.write().unwrap();
    if let Some(session) = st.session.as_mut() {
        let mut changed = false;
        for c in set {
            changed |= session.apply_set_cookie(c, host);
        }
        st.dirty |= changed;
    }
}
