//! Up next: the playback queue.

use adw::prelude::*;
use gtk::glib;

use super::cards::{ListRow, RowStyle};
use crate::api::Track;
use crate::playback::Playback;

pub struct QueueView {
    pub root: gtk::Box,
    pub list: gtk::ListView,
}

pub fn queue_view() -> QueueView {
    let pb = Playback::get();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("queue-view");

    let header = gtk::Box::new(gtk::Orientation::Vertical, 2);
    header.add_css_class("queue-header");
    let from = gtk::Label::new(Some("PLAYING FROM"));
    from.set_xalign(0.0);
    from.add_css_class("caption");
    from.add_css_class("dimmed");
    let title = gtk::Label::new(None);
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("heading");
    header.append(&from);
    header.append(&title);
    let update_title = glib::clone!(
        #[weak]
        title,
        #[weak]
        header,
        move |pb: &Playback| {
            let t = pb.queue_title();
            title.set_text(&t);
            header.set_visible(!t.is_empty());
        }
    );
    update_title(&pb);
    pb.connect_queue_title_notify(update_title);
    root.append(&header);

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        li.set_child(Some(&ListRow::new()));
    });
    factory.connect_bind(|_, obj| {
        let li = obj.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(row), Some(item)) = (
            li.child().and_downcast::<ListRow>(),
            li.item().and_downcast::<glib::BoxedAnyObject>(),
        ) {
            row.bind_track(&item.borrow::<Track>(), RowStyle::Duration);
            row.set_queue_index(li.position() as i32);
            let current = Playback::get().current() == li.position() as i32;
            if current {
                row.add_css_class("current");
            } else {
                row.remove_css_class("current");
            }
        }
    });
    let list = gtk::ListView::new(Some(gtk::NoSelection::new(Some(pb.queue()))), Some(factory));
    list.set_single_click_activate(true);
    list.add_css_class("row-list");
    list.add_css_class("queue-list");
    list.connect_activate(|_, pos| Playback::get().play_index(pos));
    let sw = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .vexpand(true)
        // Lets the phone's bottom sheet open tall.
        .propagate_natural_height(true)
        .build();
    root.append(&sw);
    QueueView { root, list }
}

/// Bring the current track into view.
pub fn scroll_to_current(list: &gtk::ListView) {
    let cur = Playback::get().current();
    if cur >= 0 && (cur as u32) < list.model().map(|m| m.n_items()).unwrap_or(0) {
        list.scroll_to(cur as u32, gtk::ListScrollFlags::NONE, None);
    }
}
