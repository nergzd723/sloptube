//! The frame both apps' windows are built in: tabs (each with its own
//! navigation stack), a sidebar on wide windows and a bottom tab bar on
//! narrow ones, toasts, and a few page helpers. Each window composes a
//! [`Shell`] and lays out its player around `split` and `switcher_bar`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

/// The window button layout without the app icon: the logo already shows it,
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

/// A header bar for the apps' pages.
pub fn header_bar() -> adw::HeaderBar {
    let header = adw::HeaderBar::new();
    if let Some(layout) = decoration_layout() {
        header.set_decoration_layout(Some(&layout));
    }
    header
}

/// The app's logo: its icon and name.
pub fn logo(icon: &str, name: &str, size: i32) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    b.add_css_class("app-logo");
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(size);
    let label = gtk::Label::new(Some(name));
    label.add_css_class("logo-text");
    b.append(&image);
    b.append(&label);
    b
}

/// The initial window size: `default`, or `WxH` from the environment variable
/// `var` (handy for trying the phone layout).
pub fn window_size(var: &str, default: (i32, i32)) -> (i32, i32) {
    std::env::var(var)
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or(default)
}

/// On every press and release GTK sets `:active` on each widget from the one
/// pressed up to the window, and that restyles every widget in the window whose
/// style depends on an ancestor's state. CSS only skips hidden widgets, not
/// pages that stacks merely stop drawing, so each touch restyled every page in
/// the other tabs and the back stack (15-25 ms per press and release). Hide a
/// page's content while the page is off screen to keep it out of that.
pub fn hide_content_while_unmapped(page: &adw::NavigationPage) {
    // SAFETY: the key is only ever used with `()`.
    if unsafe { page.data::<()>("sloptube-hides-content") }.is_some() {
        return;
    }
    unsafe { page.set_data("sloptube-hides-content", ()) };
    let sync = |page: &adw::NavigationPage| {
        if let Some(child) = page.child() {
            child.set_visible(page.is_mapped());
        }
    };
    page.connect_map(sync);
    page.connect_unmap(sync);
    sync(page);
}

struct Inner {
    window: glib::WeakRef<adw::ApplicationWindow>,
    toast_overlay: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    stack: adw::ViewStack,
    switcher_bar: adw::ViewSwitcherBar,
    sidebar: gtk::Box,
    sidebar_nav: gtk::ListBox,
    navs: RefCell<Vec<(String, adw::NavigationView)>>,
    narrow: Cell<bool>,
    narrow_only: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    wide_only: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    syncing: Cell<bool>,
    on_layout: RefCell<Vec<Box<dyn Fn(bool)>>>,
    on_tab_activated: RefCell<Vec<Box<dyn Fn()>>>,
}

/// A window's tabs, sidebar, bottom tab bar and layout.
#[derive(Clone)]
pub struct Shell(Rc<Inner>);

impl Shell {
    /// `narrow_width` (sp) is where the sidebar gives way to bottom tabs; below
    /// 520 sp the window also gets the `phone` class. `logo` heads the sidebar.
    pub fn new(window: &adw::ApplicationWindow, logo: &impl IsA<gtk::Widget>, sidebar_width: (f64, f64), narrow_width: u32) -> Self {
        let shell = Shell(Rc::new(Inner {
            window: window.downgrade(),
            toast_overlay: adw::ToastOverlay::new(),
            split: adw::OverlaySplitView::new(),
            stack: adw::ViewStack::new(),
            switcher_bar: adw::ViewSwitcherBar::new(),
            sidebar: gtk::Box::new(gtk::Orientation::Vertical, 6),
            sidebar_nav: gtk::ListBox::new(),
            navs: RefCell::default(),
            narrow: Cell::new(false),
            narrow_only: RefCell::default(),
            wide_only: RefCell::default(),
            syncing: Cell::new(false),
            on_layout: RefCell::default(),
            on_tab_activated: RefCell::default(),
        }));
        let i = &shell.0;
        i.stack.set_vexpand(true);
        i.switcher_bar.set_stack(Some(&i.stack));

        i.sidebar_nav.add_css_class("navigation-sidebar");
        // The selection only mirrors the visible tab. GTK also selects rows when focus
        // moves into the sidebar, as it does when the player collapses and hides the
        // focused button, so tabs switch on activation (a tap or Enter) instead.
        let weak = Rc::downgrade(&shell.0);
        i.sidebar_nav.connect_row_selected(move |_, _| {
            if let Some(i) = weak.upgrade()
                && !i.syncing.get()
            {
                Shell(i).sync_sidebar();
            }
        });
        let weak = Rc::downgrade(&shell.0);
        i.sidebar_nav.connect_row_activated(move |_, row| {
            let Some(i) = weak.upgrade() else { return };
            let shell = Shell(i);
            let name = row.widget_name();
            if shell.0.stack.visible_child_name().as_deref() == Some(name.as_str()) {
                // Tapping the tab that is showing returns to its root.
                shell.current_nav().pop_to_tag(&name);
            }
            shell.0.stack.set_visible_child_name(&name);
            for f in shell.0.on_tab_activated.borrow().iter() {
                f();
            }
        });
        let weak = Rc::downgrade(&shell.0);
        i.stack.connect_visible_child_name_notify(move |_| {
            if let Some(i) = weak.upgrade() {
                Shell(i).sync_sidebar();
            }
        });
        i.sidebar.append(&i.sidebar_nav);
        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .child(&i.sidebar)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let header = header_bar();
        header.set_show_title(false);
        header.pack_start(logo);
        let sidebar_tv = adw::ToolbarView::new();
        sidebar_tv.add_top_bar(&header);
        sidebar_tv.set_content(Some(&sidebar_scroll));
        sidebar_tv.add_css_class("sidebar");
        i.split.set_sidebar(Some(&sidebar_tv));
        i.split.set_min_sidebar_width(sidebar_width.0);
        i.split.set_max_sidebar_width(sidebar_width.1);

        shell.setup_breakpoints(window, narrow_width);
        shell
    }

    pub fn split(&self) -> &adw::OverlaySplitView {
        &self.0.split
    }

    pub fn stack(&self) -> &adw::ViewStack {
        &self.0.stack
    }

    pub fn switcher_bar(&self) -> &adw::ViewSwitcherBar {
        &self.0.switcher_bar
    }

    /// Wraps the window's content; toasts appear over it.
    pub fn toast_overlay(&self) -> &adw::ToastOverlay {
        &self.0.toast_overlay
    }

    /// The sidebar's column, for sections below the tabs.
    pub fn sidebar(&self) -> &gtk::Box {
        &self.0.sidebar
    }

    pub fn toast(&self, message: &str) {
        let t = adw::Toast::new(message);
        t.set_timeout(3);
        self.0.toast_overlay.add_toast(t);
    }

    /// Add a tab with `root` as its first page.
    pub fn add_tab(&self, name: &str, title: &str, icon: &str, root: &adw::NavigationPage) {
        let i = &self.0;
        let nav = adw::NavigationView::new();
        nav.connect_visible_page_notify(|nav| {
            if let Some(page) = nav.visible_page() {
                hide_content_while_unmapped(&page);
            }
        });
        hide_content_while_unmapped(root);
        nav.add(root);
        let p = i.stack.add_titled_with_icon(&nav, Some(name), title, icon);
        p.set_use_underline(false);
        i.navs.borrow_mut().push((name.into(), nav));

        let row = gtk::ListBoxRow::new();
        row.set_widget_name(name);
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.append(&gtk::Image::from_icon_name(icon));
        b.append(&gtk::Label::new(Some(title)));
        row.set_child(Some(&b));
        i.sidebar_nav.append(&row);
        self.sync_sidebar();
    }

    /// Called when a tab is picked in the sidebar (e.g. to collapse the player).
    pub fn connect_tab_activated(&self, f: impl Fn() + 'static) {
        self.0.on_tab_activated.borrow_mut().push(Box::new(f));
    }

    /// Called with `narrow` whenever the layout switches.
    pub fn connect_layout(&self, f: impl Fn(bool) + 'static) {
        self.0.on_layout.borrow_mut().push(Box::new(f));
    }

    pub fn narrow(&self) -> bool {
        self.0.narrow.get()
    }

    /// Show `w` only in the narrow layout.
    pub fn register_narrow_only(&self, w: &impl IsA<gtk::Widget>) {
        w.set_visible(self.0.narrow.get());
        self.0.narrow_only.borrow_mut().push(w.upcast_ref::<gtk::Widget>().downgrade());
    }

    /// Show `w` only in the wide layout.
    pub fn register_wide_only(&self, w: &impl IsA<gtk::Widget>) {
        w.set_visible(!self.0.narrow.get());
        self.0.wide_only.borrow_mut().push(w.upcast_ref::<gtk::Widget>().downgrade());
    }

    pub fn visible_tab(&self) -> String {
        self.0.stack.visible_child_name().unwrap_or_default().to_string()
    }

    pub fn current_nav(&self) -> adw::NavigationView {
        let name = self.visible_tab();
        let navs = self.0.navs.borrow();
        navs.iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| navs[0].1.clone())
    }

    /// Select the visible tab's row in the sidebar.
    fn sync_sidebar(&self) {
        let i = &self.0;
        let name = self.visible_tab();
        i.syncing.set(true);
        let mut child = i.sidebar_nav.first_child();
        while let Some(c) = child {
            if let Some(row) = c.downcast_ref::<gtk::ListBoxRow>()
                && row.widget_name() == name
            {
                i.sidebar_nav.select_row(Some(row));
            }
            child = c.next_sibling();
        }
        i.syncing.set(false);
    }

    fn setup_breakpoints(&self, window: &adw::ApplicationWindow, narrow_width: u32) {
        // Only one breakpoint applies at a time (the last matching one), so both
        // switch to the narrow layout; the phone one also tightens the styling.
        for (width, phone) in [(narrow_width, false), (520, true)] {
            let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse(&format!("max-width: {width}sp")).unwrap());
            let weak = Rc::downgrade(&self.0);
            bp.connect_apply(move |_| {
                let Some(i) = weak.upgrade() else { return };
                let shell = Shell(i);
                if let Some(win) = shell.0.window.upgrade() {
                    if phone {
                        win.add_css_class("phone");
                    } else {
                        win.remove_css_class("phone");
                    }
                }
                shell.set_narrow(true);
            });
            let weak = Rc::downgrade(&self.0);
            bp.connect_unapply(move |_| {
                let Some(i) = weak.upgrade() else { return };
                let Some(win) = i.window.upgrade() else { return };
                if phone {
                    win.remove_css_class("phone");
                }
                // Switching between the two breakpoints unapplies one before
                // applying the other: only go wide if none applies after all.
                let weak = Rc::downgrade(&i);
                glib::idle_add_local_once(move || {
                    if let Some(i) = weak.upgrade()
                        && i.window.upgrade().is_some_and(|w| w.current_breakpoint().is_none())
                    {
                        Shell(i).set_narrow(false);
                    }
                });
            });
            window.add_breakpoint(bp);
        }
        self.apply_layout(false);
    }

    fn set_narrow(&self, narrow: bool) {
        if self.0.narrow.get() != narrow {
            self.apply_layout(narrow);
        }
    }

    fn apply_layout(&self, narrow: bool) {
        let i = &self.0;
        i.narrow.set(narrow);
        i.split.set_collapsed(narrow);
        i.split.set_show_sidebar(!narrow);
        i.switcher_bar.set_reveal(narrow);
        if let Some(win) = i.window.upgrade() {
            log::debug!("layout: narrow={narrow} (window {}x{})", win.width(), win.height());
            if narrow {
                win.add_css_class("narrow");
            } else {
                win.remove_css_class("narrow");
            }
        }
        i.narrow_only.borrow_mut().retain(|w| w.upgrade().is_some());
        i.wide_only.borrow_mut().retain(|w| w.upgrade().is_some());
        for w in i.narrow_only.borrow().iter().filter_map(|w| w.upgrade()) {
            w.set_visible(narrow);
        }
        for w in i.wide_only.borrow().iter().filter_map(|w| w.upgrade()) {
            w.set_visible(!narrow);
        }
        for f in i.on_layout.borrow().iter() {
            f(narrow);
        }
    }

    /// Keep keyboard focus inside `sheet` while `expanded()` says it covers the
    /// window, giving it back with `refocus`. Crossing a breakpoint (fullscreen,
    /// a resize) hides the content for a frame; GTK moves focus away and
    /// libadwaita replays that as Tab presses on the next frame, which would
    /// end up on a sidebar row or header button behind the sheet (where Space
    /// then activates it). Dialogs and menus may take focus.
    pub fn keep_focus_in(&self, sheet: &impl IsA<gtk::Widget>, expanded: impl Fn() -> bool + 'static, refocus: impl Fn() + 'static) {
        let Some(window) = self.0.window.upgrade() else { return };
        let sheet = sheet.upcast_ref::<gtk::Widget>().downgrade();
        let queued = Rc::new(Cell::new(false));
        let check = Rc::new(move |win: &adw::ApplicationWindow| {
            let Some(sheet) = sheet.upgrade() else { return };
            if !expanded() {
                return;
            }
            let lost = GtkWindowExt::focus(win).is_none_or(|f| {
                !f.is_ancestor(&sheet)
                    && f.ancestor(adw::Dialog::static_type()).is_none()
                    && f.ancestor(gtk::Popover::static_type()).is_none()
            });
            if lost {
                refocus();
            }
        });
        window.connect_focus_widget_notify(move |win| {
            if queued.replace(true) {
                return;
            }
            let queued = queued.clone();
            let check = check.clone();
            // Check once that replay has run.
            win.add_tick_callback(move |win, _| {
                let win = win.downgrade();
                let queued = queued.clone();
                let check = check.clone();
                glib::idle_add_local_once(move || {
                    queued.set(false);
                    if let Some(win) = win.upgrade() {
                        check(&win);
                    }
                });
                glib::ControlFlow::Break
            });
        });
    }

    /// Ask for a link: prefilled from the clipboard when it holds one that
    /// `recognise` accepts; `open` gets what was entered.
    pub fn open_link_dialog(&self, title: &str, body: Option<&str>, placeholder: &str, recognise: fn(&str) -> bool, open: impl Fn(String) + 'static) {
        let Some(window) = self.0.window.upgrade() else { return };
        let dialog = adw::AlertDialog::new(Some(title), body);
        let entry = gtk::Entry::builder()
            .placeholder_text(placeholder)
            .activates_default(true)
            .build();
        if let Some(display) = gdk::Display::default() {
            let entry = entry.downgrade();
            display.clipboard().read_text_async(gio::Cancellable::NONE, move |res| {
                if let (Ok(Some(text)), Some(entry)) = (res, entry.upgrade())
                    && recognise(&text)
                    && entry.text().is_empty()
                {
                    entry.set_text(text.trim());
                    entry.select_region(0, -1);
                }
            });
        }
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("open", "Open");
        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("open"));
        dialog.connect_response(None, move |_, resp| {
            if resp == "open" {
                open(entry.text().to_string());
            }
        });
        dialog.present(Some(&window));
    }
}
