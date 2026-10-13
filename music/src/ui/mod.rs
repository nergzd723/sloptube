//! User interface.

pub mod actions;
pub mod art;
pub mod artist;
pub mod browse;
pub mod cards;
pub mod collection;
pub mod library;
pub mod login;
pub mod lyrics;
pub mod now_playing;
pub mod page;
pub mod player_bar;
pub mod preferences;
pub mod queue;
pub mod rows;
pub mod search;
pub mod shelf;
pub mod window;

use adw::prelude::*;
use common::links::{self, Link};
use gtk::gio;

use crate::api::{BrowseTarget, PageKind, WatchTarget};

/// Lists kept on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalList {
    Liked,
    History,
}

/// Where a click should take the user.
#[derive(Debug, Clone)]
pub enum Nav {
    /// An album, playlist, artist or any other browse page (with a title to show while loading).
    Page(BrowseTarget, String),
    Search(String),
    Local(LocalList),
    /// Start playing (links to songs).
    Play(WatchTarget),
    /// A web link.
    Url(String),
}

/// Route a navigation request to the window that owns `widget`.
pub fn navigate(widget: &impl IsA<gtk::Widget>, nav: Nav) {
    if let Nav::Url(url) = &nav {
        if let Some(n) = nav_for_url(url) {
            return navigate(widget, n);
        }
        gtk::UriLauncher::new(url).launch(
            widget.root().and_downcast_ref::<gtk::Window>(),
            gio::Cancellable::NONE,
            |res| {
                if let Err(e) = res {
                    log::warn!("cannot open link: {e}");
                }
            },
        );
        return;
    }
    if let Some(win) = widget.root().and_downcast::<window::Window>().or_else(main_window) {
        win.navigate(nav);
    }
}

pub fn main_window() -> Option<window::Window> {
    gio::Application::default()
        .and_downcast::<gtk::Application>()?
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<window::Window>().ok())
}

pub fn toast(message: &str) {
    if let Some(win) = main_window() {
        win.toast(message);
    }
}

pub fn copy_text(widget: &impl IsA<gtk::Widget>, text: &str, what: &str) {
    widget.clipboard().set_text(text);
    toast(what);
}

pub fn format_time(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Call `f(narrow)` now and whenever the window switches between the phone
/// and desktop layouts, for as long as `widget` exists.
pub fn on_layout(widget: &impl IsA<gtk::Widget>, f: impl Fn(bool) + 'static) {
    let Some(win) = main_window() else {
        f(false);
        return;
    };
    f(win.narrow());
    let weak = widget.upcast_ref::<gtk::Widget>().downgrade();
    let id = win.connect_narrow_notify(move |w| {
        if weak.upgrade().is_some() {
            f(w.narrow());
        }
    });
    let id = std::cell::Cell::new(Some(id));
    let win = win.downgrade();
    widget.connect_destroy(move |_| {
        if let (Some(w), Some(id)) = (win.upgrade(), id.take()) {
            w.disconnect(id);
        }
    });
}

/// Remove all children of a container, through the container's own API
/// (list boxes keep their own record of rows; unparenting them corrupts it).
pub fn clear(container: &impl IsA<gtk::Widget>) {
    let w = container.upcast_ref::<gtk::Widget>();
    if let Some(list) = w.downcast_ref::<gtk::ListBox>() {
        list.remove_all();
    } else if let Some(flow) = w.downcast_ref::<gtk::FlowBox>() {
        flow.remove_all();
    } else if let Some(b) = w.downcast_ref::<gtk::Box>() {
        while let Some(c) = b.first_child() {
            b.remove(&c);
        }
    } else {
        while let Some(c) = w.first_child() {
            c.unparent();
        }
    }
}

/// Map music.youtube.com / youtube.com / youtu.be links to in-app navigation.
pub fn nav_for_url(url: &str) -> Option<Nav> {
    Some(match links::parse(url)? {
        Link::Watch { video, playlist, .. } => Nav::Play(WatchTarget {
            video_id: video,
            playlist_id: playlist,
            params: None,
        }),
        Link::Playlist(list) => Nav::Page(BrowseTarget::new(format!("VL{list}")), String::new()),
        // Handles and custom URLs only resolve on youtube.com.
        Link::Channel(id) if !id.starts_with("UC") => return None,
        Link::Channel(id) | Link::Browse(id) => Nav::Page(BrowseTarget::new(id), String::new()),
        Link::Search(q) => Nav::Search(q),
    })
}

/// A shareable link for a browse target.
pub fn browse_url(t: &BrowseTarget) -> String {
    match t.kind {
        PageKind::Playlist => format!(
            "https://music.youtube.com/playlist?list={}",
            t.id.strip_prefix("VL").unwrap_or(&t.id)
        ),
        PageKind::Artist | PageKind::Profile => format!("https://music.youtube.com/channel/{}", t.id),
        _ => format!("https://music.youtube.com/browse/{}", t.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        match nav_for_url("https://music.youtube.com/watch?v=abc&list=RDAMVMabc") {
            Some(Nav::Play(t)) => {
                assert_eq!(t.video_id.as_deref(), Some("abc"));
                assert_eq!(t.playlist_id.as_deref(), Some("RDAMVMabc"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            nav_for_url("https://music.youtube.com/playlist?list=PL1"),
            Some(Nav::Page(t, _)) if t.id == "VLPL1" && t.kind == PageKind::Playlist
        ));
        assert!(matches!(
            nav_for_url("https://music.youtube.com/browse/MPREb_x"),
            Some(Nav::Page(t, _)) if t.kind == PageKind::Album
        ));
        assert!(matches!(nav_for_url("https://youtu.be/xyz"), Some(Nav::Play(_))));
        assert!(nav_for_url("https://example.com/watch?v=abc").is_none());
    }

    #[test]
    fn times() {
        assert_eq!(format_time(59.9), "0:59");
        assert_eq!(format_time(3723.0), "1:02:03");
    }
}
