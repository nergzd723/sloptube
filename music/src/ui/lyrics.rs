//! Lyrics and related content of the current track, loaded only when shown.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::browse::shelves_to_rows;
use super::rows::{Row, RowList};
use crate::api;
use crate::playback::Playback;
use crate::runtime;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LyricsView {
        pub stack: gtk::Stack,
        pub text: gtk::Label,
        pub source: gtk::Label,
        pub status: adw::StatusPage,
        pub loaded_for: RefCell<String>,
        pub scrolled: gtk::ScrolledWindow,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LyricsView {
        const NAME: &'static str = "StmLyricsView";
        type Type = super::LyricsView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for LyricsView {}
    impl WidgetImpl for LyricsView {}
    impl BinImpl for LyricsView {}
}

glib::wrapper! {
    pub struct LyricsView(ObjectSubclass<imp::LyricsView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for LyricsView {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsView {
    pub fn new() -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.text.set_wrap(true);
        imp.text.set_xalign(0.0);
        imp.text.set_selectable(true);
        imp.text.add_css_class("lyrics");
        imp.source.set_xalign(0.0);
        imp.source.add_css_class("dimmed");
        imp.source.add_css_class("caption");
        let b = gtk::Box::new(gtk::Orientation::Vertical, 18);
        b.add_css_class("lyrics-box");
        b.append(&imp.text);
        b.append(&imp.source);
        let clamp = adw::Clamp::builder().maximum_size(720).child(&b).build();
        imp.scrolled.set_child(Some(&clamp));
        imp.scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        imp.scrolled.set_vexpand(true);
        imp.scrolled.set_propagate_natural_height(true);
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        imp.status.set_icon_name(Some("stm-lyrics-symbolic"));
        imp.status.add_css_class("compact");
        imp.stack.add_named(&spinner, Some("loading"));
        imp.stack.add_named(&imp.status, Some("status"));
        imp.stack.add_named(&imp.scrolled, Some("lyrics"));
        imp.stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        obj.set_child(Some(&imp.stack));
        let pb = Playback::get();
        pb.connect_video_id_notify(glib::clone!(
            #[weak]
            obj,
            move |_| {
                if obj.is_mapped() {
                    obj.refresh();
                }
            }
        ));
        obj.connect_map(|v| v.refresh());
        obj
    }

    fn show_status(&self, title: &str, description: &str) {
        let imp = self.imp();
        imp.status.set_title(title);
        imp.status.set_description(Some(description));
        imp.stack.set_visible_child_name("status");
    }

    pub fn refresh(&self) {
        let imp = self.imp();
        let pb = Playback::get();
        let id = pb.video_id();
        if id.is_empty() {
            self.show_status("Nothing playing", "");
            return;
        }
        if *imp.loaded_for.borrow() == id {
            return;
        }
        imp.loaded_for.replace(id.clone());
        imp.stack.set_visible_child_name("loading");
        let view = self.downgrade();
        glib::spawn_future_local(async move {
            let res: anyhow::Result<Option<api::Lyrics>> = async {
                let info = pb.track_info(&id).await?;
                match info.lyrics {
                    Some(l) => runtime::run(async move { api::client().lyrics(&l).await }).await,
                    None => Ok(None),
                }
            }
            .await;
            let Some(view) = view.upgrade() else { return };
            let imp = view.imp();
            if *imp.loaded_for.borrow() != id {
                return;
            }
            match res {
                Ok(Some(l)) => {
                    imp.text.set_text(&l.text);
                    imp.source.set_text(l.source.as_deref().unwrap_or(""));
                    imp.source.set_visible(l.source.is_some());
                    imp.scrolled.vadjustment().set_value(0.0);
                    imp.stack.set_visible_child_name("lyrics");
                }
                Ok(None) => view.show_status("Lyrics not available", "This song has no lyrics on YouTube Music."),
                Err(e) => {
                    imp.loaded_for.replace(String::new());
                    view.show_status("Couldn't load the lyrics", &format!("{e}"));
                }
            }
        });
    }
}

/// Songs, playlists and artists related to the current track.
pub fn related_view() -> RowList {
    let list = RowList::new();
    list.set_propagate_natural_height(true);
    let loaded_for: Rc<RefCell<String>> = Rc::default();
    list.set_empty_state("view-grid-symbolic", "Nothing related", "");
    list.set_loader(Rc::new(move |_| {
        Box::pin(async move {
            let pb = Playback::get();
            let id = pb.video_id();
            if id.is_empty() {
                return Ok((Vec::new(), None));
            }
            let info = pb.track_info(&id).await?;
            let Some(related) = info.related else {
                return Ok((Vec::new(), None));
            };
            let page = runtime::run(async move { api::client().browse_page(&related, None).await }).await?;
            let rows: Vec<Row> = shelves_to_rows(page.shelves);
            Ok((rows, None))
        })
    }));
    let refresh = {
        let loaded_for = loaded_for.clone();
        move |list: &RowList| {
            let id = Playback::get().video_id();
            if *loaded_for.borrow() != id {
                loaded_for.replace(id);
                list.reload();
            }
        }
    };
    let refresh = Rc::new(refresh);
    {
        let refresh = refresh.clone();
        list.connect_map(move |l| refresh(l));
    }
    Playback::get().connect_video_id_notify(glib::clone!(
        #[weak]
        list,
        move |_| {
            if list.is_mapped() {
                refresh(&list);
            }
        }
    ));
    list
}
