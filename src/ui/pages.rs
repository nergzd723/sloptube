//! Top-level tab pages (Home, Subscriptions, Library) and shared page chrome.

use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use super::card::CardStyle;
use super::feed::{FeedView, Loader};
use super::playlist::PlaylistPage;
use super::{Nav, navigate};
use crate::account::Account;
use crate::api::{self, FeedPage};
use crate::config;
use crate::image;
use crate::runtime;

/// A loader for a browse feed (`browse_id` + optional params) with continuations.
pub fn browse_loader(browse_id: &'static str) -> Loader {
    Rc::new(move |token: Option<String>| {
        Box::pin(async move {
            runtime::run(async move {
                let c = api::client();
                match token {
                    None => c.feed(browse_id, None).await,
                    Some(t) => c.feed_more(&t).await,
                }
            })
            .await
        })
    })
}

fn logo() -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    b.add_css_class("app-logo");
    let icon = gtk::Image::from_icon_name(config::APP_ID);
    icon.set_pixel_size(24);
    let label = gtk::Label::new(Some(config::APP_NAME));
    label.add_css_class("logo-text");
    b.append(&icon);
    b.append(&label);
    b.upcast()
}

fn account_header(account: &Account) -> gtk::Box {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.add_css_class("account-header");
    let big = adw::Avatar::new(40, Some(&account.name()), true);
    image::set_avatar(&big, Some(&account.avatar()));
    let names = gtk::Box::new(gtk::Orientation::Vertical, 2);
    names.set_valign(gtk::Align::Center);
    let name = gtk::Label::new(Some(&account.name()));
    name.set_xalign(0.0);
    name.add_css_class("heading");
    let handle = gtk::Label::new(Some(&account.handle()));
    handle.set_xalign(0.0);
    handle.add_css_class("dim-label");
    handle.add_css_class("caption");
    names.append(&name);
    names.append(&handle);
    header.append(&big);
    header.append(&names);
    header
}

/// The account avatar with its menu.
pub fn account_button() -> gtk::MenuButton {
    let account = Account::get();
    let avatar = adw::Avatar::new(26, None, true);
    let icon = gtk::Image::from_icon_name("avatar-default-symbolic");
    let face = gtk::Stack::new();
    face.add_named(&icon, Some("icon"));
    face.add_named(&avatar, Some("avatar"));
    let button = gtk::MenuButton::builder()
        .child(&face)
        .tooltip_text("Account")
        .build();
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("account-button");

    let update = {
        let button = button.downgrade();
        let avatar = avatar.downgrade();
        move |account: &Account| {
            let (Some(button), Some(avatar)) = (button.upgrade(), avatar.upgrade()) else {
                return;
            };
            let menu = gio::Menu::new();
            let top = gio::Menu::new();
            if account.signed_in() {
                let item = gio::MenuItem::new(None, None);
                item.set_attribute_value("custom", Some(&"account-header".to_variant()));
                top.append_item(&item);
                avatar.set_text(Some(&account.name()));
                image::set_avatar(&avatar, Some(&account.avatar()));
                if let Some(face) = avatar.parent().and_downcast::<gtk::Stack>() {
                    face.set_visible_child_name("avatar");
                }
            } else {
                image::set_avatar(&avatar, None);
                if let Some(face) = avatar.parent().and_downcast::<gtk::Stack>() {
                    face.set_visible_child_name("icon");
                }
                top.append(Some("Sign in…"), Some("win.sign-in"));
            }
            menu.append_section(None, &top);
            let mid = gio::Menu::new();
            mid.append(Some("Downloads"), Some("win.show-downloads"));
            mid.append(Some("Open YouTube Link…"), Some("win.open-link"));
            menu.append_section(None, &mid);
            let bottom = gio::Menu::new();
            bottom.append(Some("Preferences"), Some("app.preferences"));
            bottom.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
            bottom.append(Some("About SlopTube"), Some("app.about"));
            if account.signed_in() {
                bottom.append(Some("Sign Out"), Some("win.sign-out"));
            }
            menu.append_section(None, &bottom);
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            if account.signed_in() {
                popover.add_child(&account_header(account), "account-header");
            }
            button.set_popover(Some(&popover));
        }
    };
    update(&account);
    for prop in ["signed-in", "name", "avatar"] {
        let update = update.clone();
        account.connect_notify_local(Some(prop), move |a, _| update(a));
    }
    button
}

/// Header bar used on the root page of every tab.
pub fn tab_header(win: &super::window::Window) -> adw::HeaderBar {
    let header = adw::HeaderBar::new();
    header.add_css_class("tab-header");
    let logo = logo();
    header.pack_start(&logo);
    win.register_narrow_only(&logo);

    let search_pill = gtk::Button::new();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.append(&gtk::Image::from_icon_name("system-search-symbolic"));
    let l = gtk::Label::new(Some("Search YouTube"));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    content.append(&l);
    search_pill.set_child(Some(&content));
    search_pill.add_css_class("search-pill");
    search_pill.set_action_name(Some("win.search"));
    let pill_clamp = adw::Clamp::builder()
        .maximum_size(560)
        .tightening_threshold(360)
        .child(&search_pill)
        .hexpand(true)
        .build();
    header.set_title_widget(Some(&pill_clamp));
    win.register_wide_only(&pill_clamp);

    header.pack_end(&account_button());
    let search = gtk::Button::from_icon_name("system-search-symbolic");
    search.set_tooltip_text(Some("Search (Ctrl+F)"));
    search.set_action_name(Some("win.search"));
    header.pack_end(&search);
    win.register_narrow_only(&search);
    header
}

fn toolbar_page(title: &str, tag: &str, header: &adw::HeaderBar, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(header);
    tv.set_content(Some(content));
    adw::NavigationPage::builder().title(title).tag(tag).child(&tv).build()
}

/// A pushed page with a back button and a feed.
pub fn feed_page(title: &str, loader: Loader, style: CardStyle) -> (adw::NavigationPage, FeedView) {
    let feed = FeedView::new(style);
    feed.set_loader(loader);
    let header = adw::HeaderBar::new();
    let page = toolbar_page(title, "", &header, &feed);
    let f = feed.clone();
    page.connect_showing(move |_| f.ensure_loaded());
    (page, feed)
}

fn signed_out_status(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let sign_in = gtk::Button::with_label("Sign in");
    sign_in.add_css_class("pill");
    sign_in.add_css_class("suggested-action");
    sign_in.set_action_name(Some("win.sign-in"));
    let explore = gtk::Button::with_label("Explore instead");
    explore.add_css_class("pill");
    explore.set_action_name(Some("win.show-explore"));
    let buttons = gtk::Box::new(gtk::Orientation::Vertical, 12);
    buttons.set_halign(gtk::Align::Center);
    buttons.append(&sign_in);
    buttons.append(&explore);
    adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(description)
        .child(&buttons)
        .vexpand(true)
        .build()
}

fn busy_page() -> gtk::Widget {
    let s = adw::Spinner::new();
    s.set_size_request(32, 32);
    s.set_halign(gtk::Align::Center);
    s.set_valign(gtk::Align::Center);
    s.upcast()
}

/// Swap between account content and a signed-out prompt as the account state changes.
fn account_gated(content: &impl IsA<gtk::Widget>, signed_out: &impl IsA<gtk::Widget>, on_signed_in: impl Fn() + 'static) -> gtk::Stack {
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.add_named(content, Some("content"));
    stack.add_named(signed_out, Some("signed-out"));
    stack.add_named(&busy_page(), Some("busy"));
    let account = Account::get();
    let on_signed_in = Rc::new(on_signed_in);
    let update = {
        let stack = stack.downgrade();
        let on_signed_in = on_signed_in.clone();
        move |a: &Account| {
            let Some(stack) = stack.upgrade() else { return };
            if a.busy() && !a.signed_in() {
                stack.set_visible_child_name("busy");
            } else if a.signed_in() {
                stack.set_visible_child_name("content");
                on_signed_in();
            } else {
                stack.set_visible_child_name("signed-out");
            }
        }
    };
    update(&account);
    let u = update.clone();
    account.connect_signed_in_notify(move |a| u(a));
    account.connect_busy_notify(move |a| update(a));
    stack
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

pub fn home_page(win: &super::window::Window) -> (adw::NavigationPage, FeedView) {
    let feed = FeedView::new(CardStyle::GRID);
    feed.set_loader(browse_loader("FEwhat_to_watch"));
    feed.set_empty_state("view-grid-symbolic", "No recommendations yet");
    let signed_out = signed_out_status(
        config::APP_ID,
        "Your recommendations live in your account",
        "YouTube no longer shows a home feed without signing in. Sign in to get your personalised home, subscriptions, history and Watch later — or browse Explore and search without an account.",
    );
    let f = feed.clone();
    let stack = account_gated(&feed, &signed_out, move || f.reload());
    let page = toolbar_page("Home", "home", &tab_header(win), &stack);
    (page, feed)
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

fn channel_strip() -> (gtk::ScrolledWindow, Rc<dyn Fn()>) {
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    strip.add_css_class("channel-strip");
    let scroll = gtk::ScrolledWindow::builder()
        .child(&strip)
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let strip_weak = strip.downgrade();
    let reload: Rc<dyn Fn()> = Rc::new(move || {
        let strip = strip_weak.clone();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().guide_subscriptions().await }).await;
            let Some(strip) = strip.upgrade() else { return };
            while let Some(c) = strip.first_child() {
                strip.remove(&c);
            }
            let Ok(channels) = res else { return };
            let scale = strip.scale_factor().max(1) as u32;
            for ch in channels {
                let b = gtk::Button::new();
                b.add_css_class("flat");
                b.add_css_class("channel-chip");
                let v = gtk::Box::new(gtk::Orientation::Vertical, 4);
                let overlay = gtk::Overlay::new();
                let avatar = adw::Avatar::new(52, Some(&ch.name), true);
                image::set_avatar(
                    &avatar,
                    ch.avatar.as_deref().map(|u| image::sized_avatar_url(u, 52 * scale)).as_deref(),
                );
                overlay.set_child(Some(&avatar));
                if ch.has_new_content {
                    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    dot.add_css_class("new-dot");
                    dot.set_halign(gtk::Align::End);
                    dot.set_valign(gtk::Align::End);
                    overlay.add_overlay(&dot);
                }
                let l = gtk::Label::new(Some(&ch.name));
                l.set_max_width_chars(9);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.add_css_class("caption");
                v.append(&overlay);
                v.append(&l);
                b.set_child(Some(&v));
                b.set_tooltip_text(Some(&ch.name));
                let id = ch.id.clone();
                b.connect_clicked(move |b| navigate(b, Nav::Channel(id.clone())));
                strip.append(&b);
            }
            let all = gtk::Button::with_label("All");
            all.add_css_class("flat");
            all.set_valign(gtk::Align::Center);
            all.set_action_name(Some("win.show-channels"));
            strip.append(&all);
        });
    });
    (scroll, reload)
}

pub fn subscriptions_page(win: &super::window::Window) -> (adw::NavigationPage, FeedView) {
    let feed = FeedView::new(CardStyle::GRID);
    feed.set_loader(browse_loader("FEsubscriptions"));
    feed.set_empty_state("view-grid-symbolic", "No new videos from your subscriptions");
    let (strip, reload_strip) = channel_strip();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&strip);
    content.append(&feed);
    let signed_out = signed_out_status(
        "sloptube-subscriptions-symbolic",
        "Sign in to see your subscriptions",
        "New videos from the channels you follow on YouTube show up here.",
    );
    let f = feed.clone();
    let stack = account_gated(&content, &signed_out, move || {
        reload_strip();
        f.reload();
    });
    let page = toolbar_page("Subscriptions", "subscriptions", &tab_header(win), &stack);
    (page, feed)
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

pub fn library_page(win: &super::window::Window) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    let account_group = adw::PreferencesGroup::new();
    account_group.set_title("Your YouTube");
    let rows: [(&str, &str, &str); 5] = [
        ("History", "document-open-recent-symbolic", "history"),
        ("Watch later", "sloptube-watch-later-symbolic", "watch-later"),
        ("Liked videos", "sloptube-thumbs-up-symbolic", "liked"),
        ("Playlists", "view-list-symbolic", "playlists"),
        ("Channels", "sloptube-subscriptions-symbolic", "channels"),
    ];
    let mut account_rows = Vec::new();
    for (title, icon, key) in rows {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let key = key.to_owned();
        row.connect_activated(move |row| open_library_entry(row, &key));
        account_group.add(&row);
        account_rows.push(row);
    }
    page.add(&account_group);

    let local_group = adw::PreferencesGroup::new();
    local_group.set_title("On this device");
    let downloads = adw::ActionRow::builder()
        .title("Downloads")
        .activatable(true)
        .build();
    downloads.add_prefix(&gtk::Image::from_icon_name("folder-download-symbolic"));
    downloads.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    downloads.set_action_name(Some("win.show-downloads"));
    local_group.add(&downloads);
    page.add(&local_group);

    let account = Account::get();
    let update = move |a: &Account| {
        for row in &account_rows {
            row.set_sensitive(a.signed_in());
            row.set_subtitle(if a.signed_in() { "" } else { "Sign in to see this" });
        }
    };
    update(&account);
    account.connect_signed_in_notify(move |a| update(a));
    toolbar_page("Library", "library", &tab_header(win), &page)
}

fn open_library_entry(widget: &impl IsA<gtk::Widget>, key: &str) {
    let Some(nav) = widget
        .ancestor(adw::NavigationView::static_type())
        .and_downcast::<adw::NavigationView>()
    else {
        return;
    };
    match key {
        "history" => {
            let (page, _) = feed_page("History", browse_loader("FEhistory"), CardStyle::COMPACT);
            nav.push(&page);
        }
        "watch-later" => nav.push(&PlaylistPage::new("WL")),
        "liked" => nav.push(&PlaylistPage::new("LL")),
        "playlists" => {
            let (page, _) = feed_page("Playlists", browse_loader("FEplaylist_aggregation"), CardStyle::GRID);
            nav.push(&page);
        }
        "channels" => {
            let (page, _) = feed_page("Channels", browse_loader("FEchannels"), CardStyle::GRID);
            nav.push(&page);
        }
        _ => {}
    }
}

/// A one-off page showing items already in memory.
pub fn static_page(title: &str, page_data: FeedPage) -> adw::NavigationPage {
    let feed = FeedView::new(CardStyle::GRID);
    feed.set_items(page_data.items);
    let header = adw::HeaderBar::new();
    toolbar_page(title, "", &header, &feed)
}
