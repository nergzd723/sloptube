//! User interface.

pub mod aspect;
pub mod card;
pub mod channel;
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
pub mod sheet;
pub mod watch;
pub mod window;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::api::{LinkTarget, RichText, VideoItem};

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
    let u = url::Url::parse(url.trim()).ok()?;
    let host = u.host_str()?.trim_start_matches("www.").trim_start_matches("m.");
    let query = |k: &str| u.query_pairs().find(|(kk, _)| kk == k).map(|(_, v)| v.into_owned());
    let start = query("t").and_then(|t| t.trim_end_matches('s').parse::<f64>().ok());
    match host {
        "youtu.be" => {
            let id = u.path().trim_start_matches('/');
            (!id.is_empty()).then(|| Nav::Video {
                item: VideoItem {
                    id: id.to_owned(),
                    ..Default::default()
                },
                playlist: query("list"),
                start,
            })
        }
        "youtube.com" | "music.youtube.com" => {
            let path = u.path();
            if path == "/watch" {
                let id = query("v")?;
                Some(Nav::Video {
                    item: VideoItem {
                        id,
                        ..Default::default()
                    },
                    playlist: query("list"),
                    start,
                })
            } else if let Some(id) = path.strip_prefix("/shorts/").or_else(|| path.strip_prefix("/live/")) {
                Some(Nav::video_id(id.trim_end_matches('/')))
            } else if path == "/playlist" {
                Some(Nav::Playlist(query("list")?))
            } else if let Some(id) = path.strip_prefix("/channel/") {
                Some(Nav::Channel(id.split('/').next()?.to_owned()))
            } else if path.starts_with("/@") {
                Some(Nav::Channel(path.trim_start_matches('/').split('/').next()?.to_owned()))
            } else if path == "/results" {
                Some(Nav::Search(query("search_query")?))
            } else {
                None
            }
        }
        _ => None,
    }
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
    label
}

pub fn copy_to_clipboard(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget.clipboard().set_text(text);
    toast(widget, "Link copied");
}

pub fn spawn<F: std::future::Future<Output = ()> + 'static>(f: F) {
    glib::spawn_future_local(f);
}

const LAST_MOVE_KEY: &str = "sloptube-last-move";
const PRESS_STOPPED_KEY: &str = "sloptube-press-stopped";

/// Note whether each press on `scrolled` lands while it is still moving (gliding
/// after a fling), for [`press_stopped_scrolling`].
pub fn track_scrolling(scrolled: &gtk::ScrolledWindow) {
    for adj in [scrolled.hadjustment(), scrolled.vadjustment()] {
        let scrolled = scrolled.downgrade();
        adj.connect_value_changed(move |_| {
            if let Some(s) = scrolled.upgrade() {
                // SAFETY: the key is only ever used with `Instant`.
                unsafe { s.set_data(LAST_MOVE_KEY, std::time::Instant::now()) };
            }
        });
    }
    let legacy = gtk::EventControllerLegacy::new();
    legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
    legacy.connect_event(|c, ev| {
        use gtk::gdk::EventType;
        if matches!(ev.event_type(), EventType::ButtonPress | EventType::TouchBegin) {
            if let Some(s) = c.widget() {
                // SAFETY: the keys are only ever used with `Instant` and `bool`.
                unsafe {
                    let moving = s
                        .data::<std::time::Instant>(LAST_MOVE_KEY)
                        .is_some_and(|t| t.as_ref().elapsed() < std::time::Duration::from_millis(100));
                    s.set_data(PRESS_STOPPED_KEY, moving);
                }
            }
        }
        glib::Propagation::Proceed
    });
    scrolled.add_controller(legacy);
}

/// Whether the current press landed on a list (`widget` or one around it) that
/// was still moving. Such a tap only stops the list (as on phones); it shouldn't
/// also open whatever slid under the finger.
pub fn press_stopped_scrolling(widget: &impl IsA<gtk::Widget>) -> bool {
    let mut w = Some(widget.as_ref().clone());
    while let Some(x) = w {
        // SAFETY: see `track_scrolling`.
        if unsafe { x.data::<bool>(PRESS_STOPPED_KEY) }.is_some_and(|b| unsafe { *b.as_ref() }) {
            return true;
        }
        w = x.parent();
    }
    false
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
        assert!(nav_for_youtube_url("https://example.com/watch?v=abc").is_none());
    }

    #[test]
    fn times() {
        assert_eq!(format_time(59.9), "0:59");
        assert_eq!(format_time(3723.0), "1:02:03");
    }
}
