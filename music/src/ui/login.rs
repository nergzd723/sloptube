//! Signing in and out: the shared dialog (`common::login`) with Music's icon
//! and toasts, and the account button in the header.

use adw::prelude::*;
use gtk::{gio, glib};

use crate::account::Account;
use crate::config;
use crate::image;

pub fn present(parent: &impl IsA<gtk::Widget>) {
    common::login::present(parent, config::APP_ID, |msg| super::toast(msg));
}

/// Forget the session, after asking.
pub fn confirm_sign_out(parent: &impl IsA<gtk::Widget>) {
    common::login::confirm_sign_out(parent, |msg| super::toast(msg));
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
