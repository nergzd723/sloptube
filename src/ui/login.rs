//! Signing in and out: the shared dialog (`common::login`), with SlopTube's
//! icon and toasts.

use adw::prelude::*;

use crate::config;

pub fn present(parent: &impl IsA<gtk::Widget>) {
    let widget = parent.as_ref().clone();
    common::login::present(parent, config::APP_ID, move |msg| super::toast(&widget, msg));
}

/// Forget the session (with confirmation).
pub fn confirm_sign_out(parent: &impl IsA<gtk::Widget>) {
    let widget = parent.as_ref().clone();
    common::login::confirm_sign_out(parent, move |msg| super::toast(&widget, msg));
}
