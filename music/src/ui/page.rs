//! Shared page chrome: header bars, the logo, the main menu.

use adw::prelude::*;
use gtk::gio;

use crate::config;

pub fn primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let main = gio::Menu::new();
    main.append(Some("Open Link…"), Some("win.open-link"));
    main.append(Some("Open SlopTube"), Some("app.show-videos"));
    main.append(Some("Preferences"), Some("app.preferences"));
    main.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    main.append(Some("About SlopTube Music"), Some("app.about"));
    menu.append_section(None, &main);
    let quit = gio::Menu::new();
    quit.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &quit);
    menu
}

/// The window button layout without the app icon: our logo already shows it,
/// and layouts like "icon:minimize,maximize,close" would put the two side by side.
fn decoration_layout() -> Option<String> {
    let layout = gtk::Settings::default()?.gtk_decoration_layout()?;
    let strip = |side: &str| {
        side.split(',')
            .map(str::trim)
            .filter(|b| !b.is_empty() && *b != "icon")
            .collect::<Vec<_>>()
            .join(",")
    };
    Some(match layout.split_once(':') {
        Some((start, end)) => format!("{}:{}", strip(start), strip(end)),
        None => strip(&layout),
    })
}

/// A header bar for the app's pages.
pub fn header_bar() -> adw::HeaderBar {
    let header = adw::HeaderBar::new();
    if let Some(layout) = decoration_layout() {
        header.set_decoration_layout(Some(&layout));
    }
    header
}

/// The "▶ Music" logo.
pub fn logo() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    b.add_css_class("app-logo");
    let icon = gtk::Image::from_icon_name(config::APP_ID);
    icon.set_pixel_size(26);
    let label = gtk::Label::new(Some("Music"));
    label.add_css_class("logo-text");
    b.append(&icon);
    b.append(&label);
    b
}

pub struct Chrome {
    pub page: adw::NavigationPage,
    pub header: adw::HeaderBar,
    pub toolbar: adw::ToolbarView,
    /// The logo shown on root pages when the sidebar is hidden.
    pub logo: Option<gtk::Box>,
}

/// A navigation page with the app's header bar around `content`.
pub fn page(title: &str, tag: Option<&str>, content: &impl IsA<gtk::Widget>, root: bool) -> Chrome {
    let header = header_bar();
    let mut logo_widget = None;
    if root {
        let logo = logo();
        header.pack_start(&logo);
        header.set_show_title(false);
        logo_widget = Some(logo);
    }
    if root {
        header.pack_end(&super::login::account_button());
    }
    let menu = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&primary_menu())
        .tooltip_text("Main Menu")
        .primary(root)
        .build();
    header.pack_end(&menu);
    let search = gtk::Button::from_icon_name("system-search-symbolic");
    search.set_action_name(Some("win.search"));
    search.set_tooltip_text(Some("Search"));
    header.pack_end(&search);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    let page = adw::NavigationPage::builder().title(title).child(&toolbar).build();
    if let Some(t) = tag {
        page.set_tag(Some(t));
    }
    Chrome {
        page,
        header,
        toolbar,
        logo: logo_widget,
    }
}
