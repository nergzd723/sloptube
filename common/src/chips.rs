//! A sideways-scrolling row of filter chips (feed topics, search filters,
//! moods): at most one is on at a time.

use gtk::prelude::*;

#[derive(Clone)]
pub struct ChipRow {
    scroller: gtk::ScrolledWindow,
    row: gtk::Box,
}

impl Default for ChipRow {
    fn default() -> Self {
        Self::new()
    }
}

impl ChipRow {
    pub fn new() -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.add_css_class("chips");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&row)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();
        crate::touch::track_scrolling(&scroller);
        Self { scroller, row }
    }

    /// The scrolled row, to put into a page.
    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.scroller
    }

    pub fn clear(&self) {
        while let Some(c) = self.row.first_child() {
            self.row.remove(&c);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.row.first_child().is_none()
    }

    /// Add a chip, `on` or not. Clicking a chip turns the others off and calls
    /// `select` with whether it is on now: clicking the chip that is on turns it
    /// off when `can_clear`, and leaves it on (calling `select(true)` again)
    /// otherwise.
    pub fn add(&self, label: &str, on: bool, can_clear: bool, select: impl Fn(bool) + 'static) -> gtk::ToggleButton {
        let b = gtk::ToggleButton::with_label(label);
        b.add_css_class("chip");
        b.set_active(on);
        let row = self.row.downgrade();
        b.connect_clicked(move |btn| {
            // GtkToggleButton has already toggled itself. A tap that only stopped
            // the row gliding doesn't count.
            if crate::touch::press_stopped_scrolling(btn) {
                btn.set_active(!btn.is_active());
                return;
            }
            if !can_clear && !btn.is_active() {
                btn.set_active(true);
            }
            if let Some(row) = row.upgrade() {
                let mut child = row.first_child();
                while let Some(c) = child {
                    if let Some(t) = c.downcast_ref::<gtk::ToggleButton>()
                        && t != btn
                    {
                        t.set_active(false);
                    }
                    child = c.next_sibling();
                }
            }
            select(btn.is_active());
        });
        self.row.append(&b);
        b
    }
}
