//! Data types both apps' parsers produce.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thumbnail {
    pub url: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

/// The smallest thumbnail at least `min_width` pixels wide, or the largest available.
pub fn pick_thumbnail(thumbs: &[Thumbnail], min_width: u32) -> Option<&Thumbnail> {
    let mut sorted: Vec<&Thumbnail> = thumbs.iter().collect();
    sorted.sort_by_key(|t| t.width);
    sorted.iter().find(|t| t.width >= min_width).or(sorted.last()).copied()
}

/// An image URL for a thumbnail list, sized for `width` pixels.
///
/// Google's image servers (`*.googleusercontent.com`, `*.ggpht.com`) scale on
/// request through the `=wN-hN` suffix, so we ask for exactly what we show and
/// both the download and the decoded texture stay small. Video thumbnails
/// (`i.ytimg.com`) come in fixed sizes: the smallest one that is big enough.
pub fn thumb_url(thumbs: &[Thumbnail], width: u32) -> Option<String> {
    let largest = thumbs.iter().max_by_key(|t| t.width)?;
    let url = &largest.url;
    if is_resizable(url)
        && let Some(pos) = url.rfind('=')
    {
        let (w, h) = (largest.width.max(1), largest.height.max(1));
        let height = ((width as u64 * h as u64) / w as u64).max(1);
        let crop = if url[pos..].contains("-p-") { "-p" } else { "" };
        return Some(format!("{}=w{width}-h{height}{crop}-l90-rj", &url[..pos]));
    }
    // Small video thumbnails: the 320×180 one, not the letterboxed 480×360.
    if width <= 320
        && url.contains("i.ytimg.com/vi/")
        && let Some(id) = url.split("/vi/").nth(1).and_then(|r| r.split('/').next())
    {
        return Some(format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"));
    }
    pick_thumbnail(thumbs, width).map(|t| t.url.clone())
}

/// Whether Google's image servers resize `url` on request.
pub fn is_resizable(url: &str) -> bool {
    url.contains("googleusercontent.com/") || url.contains("ggpht.com/")
}

/// The signed-in account, as the account menu shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub name: String,
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

/// The account's rating of a video, song, album or playlist.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LikeStatus {
    #[default]
    Indifferent,
    Like,
    Dislike,
}

impl LikeStatus {
    /// The endpoint that sets this rating.
    pub fn endpoint(self) -> &'static str {
        match self {
            LikeStatus::Like => "like/like",
            LikeStatus::Dislike => "like/dislike",
            LikeStatus::Indifferent => "like/removelike",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(url: &str, width: u32, height: u32) -> Thumbnail {
        Thumbnail { url: url.into(), width, height }
    }

    #[test]
    fn sizes() {
        let art = [t("https://lh3.googleusercontent.com/x=w60-h60-l90-rj", 60, 60), t("https://lh3.googleusercontent.com/x=w544-h544-l90-rj", 544, 544)];
        assert_eq!(thumb_url(&art, 200).as_deref(), Some("https://lh3.googleusercontent.com/x=w200-h200-l90-rj"));
        let videos = [t("https://i.ytimg.com/vi/x/default.jpg", 120, 90), t("https://i.ytimg.com/vi/x/hqdefault.jpg", 480, 360)];
        assert_eq!(thumb_url(&videos, 200).as_deref(), Some("https://i.ytimg.com/vi/x/mqdefault.jpg"));
        assert_eq!(thumb_url(&videos, 400).as_deref(), Some("https://i.ytimg.com/vi/x/hqdefault.jpg"));
        assert_eq!(pick_thumbnail(&videos, 1000).unwrap().width, 480);
    }
}
