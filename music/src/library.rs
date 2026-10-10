//! The local library: liked songs, saved albums/playlists/artists and history.
//!
//! Kept in `~/.local/share/sloptube-music/library.json` so the Library tab is
//! useful without a Google account.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use gtk::glib;
use gtk::glib::subclass::Signal;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use serde::{Deserialize, Serialize};

use crate::api::{Card, Track};
use crate::config;

const HISTORY_MAX: usize = 300;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    /// Newest first.
    pub liked: Vec<Track>,
    /// Albums, playlists and artists, newest first.
    pub saved: Vec<Card>,
    /// Recently played, newest first.
    pub history: Vec<Track>,
}

fn path() -> std::path::PathBuf {
    config::data_dir().join("library.json")
}

/// Keep stored tracks small: one artwork size is enough.
fn slim(mut t: Track) -> Track {
    if t.thumbnails.len() > 1 {
        let best = t.thumbnails.iter().max_by_key(|x| x.width).cloned();
        t.thumbnails = best.into_iter().collect();
    }
    t.playlist_id = None;
    t.set_video_id = None;
    t
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Library {
        pub data: RefCell<Data>,
        pub save_pending: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Library {
        const NAME: &'static str = "StmLibrary";
        type Type = super::Library;
    }

    impl ObjectImpl for Library {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("changed").param_types([String::static_type()]).build()])
        }
    }
}

glib::wrapper! {
    pub struct Library(ObjectSubclass<imp::Library>);
}

thread_local! {
    static LIBRARY: Library = {
        let lib: Library = glib::Object::new();
        if let Some(d) = std::fs::read(path()).ok().and_then(|d| serde_json::from_slice::<Data>(&d).ok()) {
            lib.imp().data.replace(d);
        }
        lib
    };
}

impl Library {
    pub fn get() -> Library {
        LIBRARY.with(|l| l.clone())
    }

    fn changed(&self, what: &str) {
        self.emit_by_name::<()>("changed", &[&what]);
        if self.imp().save_pending.replace(true) {
            return;
        }
        glib::timeout_add_seconds_local_once(
            1,
            glib::clone!(
                #[weak(rename_to = lib)]
                self,
                move || lib.save_now()
            ),
        );
    }

    pub fn save_now(&self) {
        self.imp().save_pending.set(false);
        let data = serde_json::to_vec(&*self.imp().data.borrow());
        match data {
            Ok(d) => {
                if let Err(e) = config::write_atomic(&path(), &d) {
                    log::warn!("saving the library failed: {e}");
                }
            }
            Err(e) => log::warn!("serializing the library failed: {e}"),
        }
    }

    /// `what` is "liked", "saved" or "history".
    pub fn connect_changed(&self, f: impl Fn(&Self, &str) + 'static) -> glib::SignalHandlerId {
        self.connect_local("changed", false, move |args| {
            let lib = args[0].get::<Self>().unwrap();
            let what = args[1].get::<String>().unwrap_or_default();
            f(&lib, &what);
            None
        })
    }

    pub fn is_liked(&self, video_id: &str) -> bool {
        self.imp().data.borrow().liked.iter().any(|t| t.video_id == video_id)
    }

    pub fn set_liked(&self, track: &Track, liked: bool) {
        {
            let mut d = self.imp().data.borrow_mut();
            d.liked.retain(|t| t.video_id != track.video_id);
            if liked {
                d.liked.insert(0, slim(track.clone()));
            }
        }
        self.changed("liked");
    }

    pub fn toggle_liked(&self, track: &Track) -> bool {
        let liked = !self.is_liked(&track.video_id);
        self.set_liked(track, liked);
        liked
    }

    pub fn liked(&self) -> Vec<Track> {
        self.imp().data.borrow().liked.clone()
    }

    pub fn is_saved(&self, browse_id: &str) -> bool {
        self.imp().data.borrow().saved.iter().any(|c| c.target.id == browse_id)
    }

    pub fn set_saved(&self, card: &Card, saved: bool) {
        {
            let mut d = self.imp().data.borrow_mut();
            d.saved.retain(|c| c.target.id != card.target.id);
            if saved {
                let mut c = card.clone();
                if c.thumbnails.len() > 1 {
                    c.thumbnails = c.thumbnails.iter().max_by_key(|t| t.width).cloned().into_iter().collect();
                }
                d.saved.insert(0, c);
            }
        }
        self.changed("saved");
    }

    pub fn saved(&self) -> Vec<Card> {
        self.imp().data.borrow().saved.clone()
    }

    pub fn add_history(&self, track: &Track) {
        {
            let mut d = self.imp().data.borrow_mut();
            d.history.retain(|t| t.video_id != track.video_id);
            d.history.insert(0, slim(track.clone()));
            d.history.truncate(HISTORY_MAX);
        }
        self.changed("history");
    }

    pub fn history(&self) -> Vec<Track> {
        self.imp().data.borrow().history.clone()
    }

    pub fn clear_history(&self) {
        self.imp().data.borrow_mut().history.clear();
        self.changed("history");
    }
}
