//! YouTube and YouTube Music links, parsed once for both apps; each app maps
//! a [`Link`] to its own pages.

/// What a YouTube link points at.
#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// A video or song (`watch`, `youtu.be`, `/shorts/`, `/live/`), maybe in a
    /// playlist and from a time. A `watch` link may name only a playlist.
    Watch {
        video: Option<String>,
        playlist: Option<String>,
        start: Option<f64>,
    },
    Playlist(String),
    /// A channel id (`UC…`), a handle (`@name`), or an older `c/name` or `user/name`.
    Channel(String),
    /// Any other browse id (`music.youtube.com/browse/…`).
    Browse(String),
    Search(String),
}

/// Parse a youtube.com, m.youtube.com, music.youtube.com or youtu.be link.
pub fn parse(url: &str) -> Option<Link> {
    let u = url::Url::parse(url.trim()).ok()?;
    let host = u.host_str()?.trim_start_matches("www.").trim_start_matches("m.");
    let query = |k: &str| u.query_pairs().find(|(kk, _)| kk == k).map(|(_, v)| v.into_owned());
    let start = query("t").and_then(|t| t.trim_end_matches('s').parse::<f64>().ok());
    let path = u.path();
    let first = |p: &str| p.split('/').next().filter(|s| !s.is_empty()).map(str::to_owned);
    match host {
        "youtu.be" => Some(Link::Watch {
            video: Some(first(path.trim_start_matches('/'))?),
            playlist: query("list"),
            start,
        }),
        "youtube.com" | "music.youtube.com" => {
            if path == "/watch" {
                let (video, playlist) = (query("v"), query("list"));
                (video.is_some() || playlist.is_some()).then_some(Link::Watch { video, playlist, start })
            } else if let Some(id) = path.strip_prefix("/shorts/").or_else(|| path.strip_prefix("/live/")) {
                Some(Link::Watch {
                    video: Some(first(id)?),
                    playlist: None,
                    start,
                })
            } else if path == "/playlist" {
                Some(Link::Playlist(query("list")?))
            } else if let Some(id) = path.strip_prefix("/channel/") {
                Some(Link::Channel(first(id)?))
            } else if let Some(handle) = path.strip_prefix("/@") {
                Some(Link::Channel(format!("@{}", first(handle)?)))
            } else if path.starts_with("/c/") || path.starts_with("/user/") {
                // Older custom and user URLs; SlopTube resolves them like handles.
                let mut parts = path.trim_start_matches('/').split('/');
                let (kind, name) = (parts.next()?, parts.next().filter(|n| !n.is_empty())?);
                Some(Link::Channel(format!("{kind}/{name}")))
            } else if let Some(id) = path.strip_prefix("/browse/") {
                Some(Link::Browse(first(id)?))
            } else if path == "/results" || path == "/search" {
                Some(Link::Search(query("search_query").or_else(|| query("q"))?))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether `url` is a music.youtube.com link.
pub fn is_music(url: &str) -> bool {
    url::Url::parse(url.trim()).is_ok_and(|u| u.host_str() == Some("music.youtube.com"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch(video: Option<&str>, playlist: Option<&str>, start: Option<f64>) -> Option<Link> {
        Some(Link::Watch {
            video: video.map(Into::into),
            playlist: playlist.map(Into::into),
            start,
        })
    }

    #[test]
    fn videos() {
        assert_eq!(parse("https://youtu.be/abc?t=90s"), watch(Some("abc"), None, Some(90.0)));
        assert_eq!(
            parse("https://www.youtube.com/watch?v=abc&list=PL1&t=5"),
            watch(Some("abc"), Some("PL1"), Some(5.0))
        );
        assert_eq!(parse("https://m.youtube.com/shorts/xyz/"), watch(Some("xyz"), None, None));
        assert_eq!(parse("https://music.youtube.com/watch?list=RDAMPL1"), watch(None, Some("RDAMPL1"), None));
        assert_eq!(parse("https://www.youtube.com/watch"), None);
    }

    #[test]
    fn pages() {
        assert_eq!(parse("https://youtube.com/playlist?list=PL1"), Some(Link::Playlist("PL1".into())));
        assert_eq!(parse("https://www.youtube.com/channel/UC1/videos"), Some(Link::Channel("UC1".into())));
        assert_eq!(parse("https://www.youtube.com/@blender/videos"), Some(Link::Channel("@blender".into())));
        assert_eq!(parse("https://www.youtube.com/c/Blender"), Some(Link::Channel("c/Blender".into())));
        assert_eq!(parse("https://music.youtube.com/browse/MPREb_1"), Some(Link::Browse("MPREb_1".into())));
        assert_eq!(parse("https://music.youtube.com/search?q=lofi"), Some(Link::Search("lofi".into())));
        assert_eq!(parse("https://www.youtube.com/results?search_query=a+b"), Some(Link::Search("a b".into())));
        assert_eq!(parse("https://example.com/watch?v=abc"), None);
        assert!(is_music("https://music.youtube.com/watch?v=abc") && !is_music("https://youtube.com/"));
    }
}
