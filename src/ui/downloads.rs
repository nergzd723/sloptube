//! The Downloads page.

use std::collections::HashMap;
use std::rc::Rc;
use std::cell::RefCell;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::downloads::{Downloads, Entry, Status};
use crate::settings;

struct Row {
    row: gtk::ListBoxRow,
    progress: gtk::ProgressBar,
    status: gtk::Label,
    action: gtk::Button,
}

fn status_text(e: &Entry) -> String {
    match &e.status {
        Status::Queued => "Waiting…".into(),
        Status::Running => {
            let mut s = format!("{:.0}%", e.progress * 100.0);
            if let Some(sp) = &e.speed {
                s.push_str(&format!(" · {sp}"));
            }
            if let Some(eta) = &e.eta {
                s.push_str(&format!(" · {eta} left"));
            }
            s
        }
        Status::Done => {
            let size = e
                .path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| glib::format_size(m.len()).to_string())
                .unwrap_or_default();
            let kind = if e.audio_only { "Audio" } else { "Video" };
            [Some(kind.to_owned()), e.channel.clone(), (!size.is_empty()).then_some(size)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ")
        }
        Status::Failed(msg) => format!("Failed: {msg}"),
        Status::Cancelled => "Cancelled".into(),
    }
}

fn build_row(e: &Entry) -> Row {
    let row = gtk::ListBoxRow::new();
    row.set_widget_name(&e.id);
    row.set_activatable(e.status == Status::Done);
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.add_css_class("download-row");
    let pic = gtk::Picture::new();
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_can_shrink(true);
    if let Some(t) = &e.thumbnail {
        pic.set_filename(Some(t));
    }
    let frame = super::aspect::AspectBin::new(&pic, 16.0 / 9.0);
    frame.set_size_request(120, -1);
    frame.set_valign(gtk::Align::Center);
    frame.add_css_class("thumb-frame");
    frame.set_overflow(gtk::Overflow::Hidden);
    b.append(&frame);
    let info = gtk::Box::new(gtk::Orientation::Vertical, 4);
    info.set_hexpand(true);
    info.set_valign(gtk::Align::Center);
    let title = gtk::Label::new(Some(&e.title));
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_lines(2);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.add_css_class("heading");
    let status = gtk::Label::new(Some(&status_text(e)));
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.add_css_class("dim-label");
    status.add_css_class("caption");
    let progress = gtk::ProgressBar::new();
    progress.set_fraction(e.progress);
    progress.set_visible(matches!(e.status, Status::Running | Status::Queued));
    info.append(&title);
    info.append(&status);
    info.append(&progress);
    b.append(&info);
    let action = gtk::Button::new();
    action.add_css_class("flat");
    action.add_css_class("circular");
    action.set_valign(gtk::Align::Center);
    b.append(&action);
    row.set_child(Some(&b));
    let r = Row {
        row,
        progress,
        status,
        action,
    };
    update_row(&r, e);
    r
}

fn update_row(r: &Row, e: &Entry) {
    r.status.set_text(&status_text(e));
    r.progress.set_fraction(e.progress);
    r.progress.set_visible(matches!(e.status, Status::Running | Status::Queued));
    r.row.set_activatable(e.status == Status::Done);
    let running = matches!(e.status, Status::Running | Status::Queued);
    r.action
        .set_icon_name(if running { "process-stop-symbolic" } else { "user-trash-symbolic" });
    r.action
        .set_tooltip_text(Some(if running { "Cancel" } else { "Remove" }));
}

pub fn downloads_page() -> adw::NavigationPage {
    let downloads = Downloads::get();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(gtk::Align::Start);
    let rows: Rc<RefCell<HashMap<String, Row>>> = Rc::default();

    let empty = adw::StatusPage::builder()
        .icon_name("folder-download-symbolic")
        .title("No downloads")
        .description("Use “Download” on a video to keep it for offline viewing.")
        .vexpand(true)
        .build();
    let stack = gtk::Stack::new();
    let clamp = adw::Clamp::builder().maximum_size(900).child(&list).build();
    clamp.set_margin_top(12);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&clamp)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    stack.add_named(&scroll, Some("list"));
    stack.add_named(&empty, Some("empty"));

    let rebuild: Rc<dyn Fn()> = {
        let list = list.downgrade();
        let rows = rows.clone();
        let stack = stack.downgrade();
        Rc::new(move || {
            let (Some(list), Some(stack)) = (list.upgrade(), stack.upgrade()) else { return };
            list.remove_all();
            rows.borrow_mut().clear();
            let entries = Downloads::get().entries();
            for e in &entries {
                let r = build_row(e);
                let id = e.id.clone();
                r.action.connect_clicked(move |_| {
                    let d = Downloads::get();
                    match d.entry(&id).map(|e| e.status) {
                        Some(Status::Running | Status::Queued) => d.cancel(&id),
                        _ => d.remove(&id, true),
                    }
                });
                list.append(&r.row);
                rows.borrow_mut().insert(e.id.clone(), r);
            }
            stack.set_visible_child_name(if entries.is_empty() { "empty" } else { "list" });
        })
    };
    rebuild();
    {
        let rebuild = rebuild.clone();
        downloads.connect_list_changed(move |_| rebuild());
    }
    {
        let rows = rows.clone();
        downloads.connect_changed(move |d, id| {
            if let (Some(r), Some(e)) = (rows.borrow().get(id), d.entry(id)) {
                update_row(r, &e);
            }
        });
    }
    list.connect_row_activated(|list, row| {
        let id = row.widget_name().to_string();
        let Some(e) = Downloads::get().entry(&id) else { return };
        let (Some(path), Some(win)) = (e.path.clone(), list.root().and_downcast::<super::window::Window>()) else {
            return;
        };
        win.playback().open_file(e.video_item(), &path);
        win.expand_player();
    });

    let header = common::shell::header_bar();
    let open = gtk::Button::from_icon_name("folder-open-symbolic");
    open.set_tooltip_text(Some("Open downloads folder"));
    open.connect_clicked(|b| {
        let dir = settings::get().download_dir();
        let _ = std::fs::create_dir_all(&dir);
        let file = gio::File::for_path(&dir);
        gtk::FileLauncher::new(Some(&file)).launch(
            b.root().and_downcast_ref::<gtk::Window>(),
            gio::Cancellable::NONE,
            |_| {},
        );
    });
    header.pack_end(&open);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&stack));
    adw::NavigationPage::builder()
        .title("Downloads")
        .tag("downloads")
        .child(&tv)
        .build()
}
