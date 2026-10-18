//! User interface.

pub mod aspect;
pub mod card;
pub mod channel;
pub mod collapse;
pub mod comments;
pub mod downloads;
pub mod explore;
pub mod feed;
pub mod login;
pub mod mini_player;
pub mod pages;
pub mod player_view;
pub mod playlist;
pub mod preferences;
pub mod search;
pub mod watch;
pub mod window;

use adw::prelude::*;
use common::links::{self, Link};
pub use common::touch::{press_stopped_scrolling, route_label_touch, track_scrolling};
use gtk::{gio, glib};

use crate::api::{LinkTarget, RichText, TextLink, VideoItem};

/// Where a click should take the user.
#[derive(Debug, Clone)]
pub enum Nav {
    Video {
        item: VideoItem,
        playlist: Option<String>,
        start: Option<f64>,
    },
    Channel(String),
    Playlist(String),
    Search(String),
    Url(String),
}

impl Nav {
    pub fn video(item: VideoItem) -> Self {
        let playlist = item.playlist_id.clone();
        Nav::Video {
            item,
            playlist,
            start: None,
        }
    }

    pub fn video_id(id: &str) -> Self {
        Nav::Video {
            item: VideoItem {
                id: id.to_owned(),
                ..Default::default()
            },
            playlist: None,
            start: None,
        }
    }
}

/// Route a navigation request to the window that owns `widget`.
pub fn navigate(widget: &impl IsA<gtk::Widget>, nav: Nav) {
    if let Nav::Url(url) = &nav {
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
    if let Some(win) = widget.root().and_downcast::<window::Window>() {
        win.navigate(nav);
    } else if let Some(win) = main_window() {
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

pub fn toast(widget: &impl IsA<gtk::Widget>, message: &str) {
    if let Some(win) = widget
        .root()
        .and_downcast::<window::Window>()
        .or_else(main_window)
    {
        win.toast(message);
    }
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

fn link_uri(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Url(u) => u.clone(),
        LinkTarget::Seek(t) => format!("sloptube:seek/{t}"),
        LinkTarget::Video { id, start } => match start {
            Some(t) => format!("sloptube:video/{id}/{t}"),
            None => format!("sloptube:video/{id}"),
        },
        LinkTarget::Channel(id) => format!("sloptube:channel/{id}"),
        LinkTarget::Search(q) => format!("sloptube:search/{}", glib::Uri::escape_string(q, None, false)),
    }
}

/// Pango markup for text with links (descriptions, comments).
pub fn rich_markup(rt: &RichText) -> String {
    let mut out = String::with_capacity(rt.text.len() + 64);
    let mut pos = 0;
    for link in &rt.links {
        if link.start < pos || link.end > rt.text.len() {
            continue;
        }
        out.push_str(&glib::markup_escape_text(&rt.text[pos..link.start]));
        out.push_str(&format!(
            "<a href=\"{}\">{}</a>",
            glib::markup_escape_text(&link_uri(&link.target)),
            glib::markup_escape_text(&rt.text[link.start..link.end])
        ));
        pos = link.end;
    }
    out.push_str(&glib::markup_escape_text(&rt.text[pos..]));
    out
}

/// Handle a link activated in a label with `rich_markup` content.
pub fn activate_link(widget: &impl IsA<gtk::Widget>, uri: &str) -> glib::Propagation {
    let Some(rest) = uri.strip_prefix("sloptube:") else {
        // Plain web links: open YouTube links inside the app, others in the browser.
        if let Some(nav) = nav_for_youtube_url(uri) {
            navigate(widget, nav);
        } else {
            navigate(widget, Nav::Url(uri.to_owned()));
        }
        return glib::Propagation::Stop;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        ["seek", t] => {
            if let (Ok(t), Some(win)) = (t.parse::<f64>(), main_window()) {
                win.playback().seek(t);
            }
        }
        ["video", id] => navigate(widget, Nav::video_id(id)),
        ["video", id, t] => navigate(
            widget,
            Nav::Video {
                item: VideoItem {
                    id: (*id).to_owned(),
                    ..Default::default()
                },
                playlist: None,
                start: t.parse().ok(),
            },
        ),
        ["channel", id] => navigate(widget, Nav::Channel((*id).to_owned())),
        ["search", q] => navigate(
            widget,
            Nav::Search(glib::Uri::unescape_string(q, None::<&str>).map(|s| s.to_string()).unwrap_or_default()),
        ),
        _ => {}
    }
    glib::Propagation::Stop
}

/// Map youtube.com / youtu.be links to in-app navigation.
pub fn nav_for_youtube_url(url: &str) -> Option<Nav> {
    Some(match links::parse(url)? {
        Link::Watch {
            video: Some(id),
            playlist,
            start,
        } => Nav::Video {
            item: VideoItem {
                id,
                ..Default::default()
            },
            playlist,
            start,
        },
        Link::Watch {
            video: None,
            playlist: Some(list),
            ..
        }
        | Link::Playlist(list) => Nav::Playlist(list),
        Link::Watch { .. } => return None,
        Link::Channel(id) => Nav::Channel(id),
        Link::Browse(id) if id.starts_with("UC") => Nav::Channel(id),
        Link::Browse(id) => Nav::Playlist(id.strip_prefix("VL")?.to_owned()),
        Link::Search(q) => Nav::Search(q),
    })
}


/// The link of `rt` at (`x`, `y`) in `label`, which shows `rt` as its text.
pub fn link_at<'a>(label: &gtk::Label, rt: &'a RichText, x: f64, y: f64) -> Option<&'a TextLink> {
    let (ox, oy) = label.layout_offsets();
    let scale = gtk::pango::SCALE as f64;
    let (inside, index, _) = label
        .layout()
        .xy_to_index(((x - ox as f64) * scale) as i32, ((y - oy as f64) * scale) as i32);
    // The label's text is `rt.text` (markup only adds the links), so byte indices match.
    let index = index as usize;
    inside.then(|| rt.links.iter().find(|l| l.start <= index && index < l.end)).flatten()
}

/// A label with rich text that routes link clicks into the app.
pub fn rich_label(rt: &RichText) -> gtk::Label {
    let label = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .use_markup(true)
        .build();
    label.set_markup(&rich_markup(rt));
    label.connect_activate_link(|label, uri| activate_link(label, uri));
    let rt = rt.clone();
    route_label_touch(&label, move |label, x, y| {
        if let Some(link) = link_at(label, &rt, x, y) {
            activate_link(label, &link_uri(&link.target));
        }
    });
    label
}

pub fn copy_to_clipboard(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.clipboard().set_text(text);
    toast(widget, "Link copied");
}

pub fn spawn<F: std::future::Future<Output = ()> + 'static>(f: F) {
    glib::spawn_future_local(f);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_urls() {
        match nav_for_youtube_url("https://www.youtube.com/watch?v=abc&t=42s&list=PL1") {
            Some(Nav::Video { item, playlist, start }) => {
                assert_eq!(item.id, "abc");
                assert_eq!(playlist.as_deref(), Some("PL1"));
                assert_eq!(start, Some(42.0));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(nav_for_youtube_url("https://youtu.be/xyz"), Some(Nav::Video { .. })));
        assert!(matches!(nav_for_youtube_url("https://www.youtube.com/@Blender"), Some(Nav::Channel(c)) if c == "@Blender"));
        assert!(matches!(nav_for_youtube_url("https://www.youtube.com/c/LinusTechTips/videos"), Some(Nav::Channel(c)) if c == "c/LinusTechTips"));
        assert!(matches!(nav_for_youtube_url("https://www.youtube.com/user/LinusTechTips"), Some(Nav::Channel(c)) if c == "user/LinusTechTips"));
        assert!(nav_for_youtube_url("https://www.youtube.com/c/").is_none());
        assert!(nav_for_youtube_url("https://example.com/watch?v=abc").is_none());
    }

    #[test]
    fn times() {
        assert_eq!(format_time(59.9), "0:59");
        assert_eq!(format_time(3723.0), "1:02:03");
    }
}
