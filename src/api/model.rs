//! Plain data types produced by the InnerTube parsers.

use serde::{Deserialize, Serialize};

pub use common::innertube::model::{LikeStatus, Thumbnail, pick_thumbnail};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VideoItem {
    pub id: String,
    pub title: String,
    pub channel_name: Option<String>,
    pub channel_id: Option<String>,
    pub channel_avatar: Option<String>,
    pub thumbnails: Vec<Thumbnail>,
    /// Display duration, e.g. "13:16".
    pub duration: Option<String>,
    /// Display view count, e.g. "20K views".
    pub views: Option<String>,
    /// Display publish date, e.g. "2 years ago".
    pub published: Option<String>,
    pub live: bool,
    pub upcoming: bool,
    /// 0.0..=1.0 if the signed-in user has partially watched this video.
    pub watched_fraction: Option<f64>,
    /// Set when the item comes from a playlist context.
    pub playlist_id: Option<String>,
    pub index: Option<u32>,
}

impl VideoItem {
    pub fn thumbnail_url(&self, min_width: u32) -> String {
        pick_thumbnail(&self.thumbnails, min_width)
            .map(|t| t.url.clone())
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", self.id))
    }

    pub fn subtitle(&self, with_channel: bool) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if with_channel {
            if let Some(c) = &self.channel_name {
                parts.push(c);
            }
        }
        if let Some(v) = &self.views {
            parts.push(v);
        }
        if let Some(p) = &self.published {
            parts.push(p);
        }
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelItem {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub handle: Option<String>,
    pub subscribers: Option<String>,
    pub video_count: Option<String>,
    pub has_new_content: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub id: String,
    pub title: String,
    pub thumbnails: Vec<Thumbnail>,
    pub video_count: Option<String>,
    pub channel_name: Option<String>,
    pub channel_id: Option<String>,
    pub first_video_id: Option<String>,
    pub is_mix: bool,
}

#[derive(Debug, Clone)]
pub enum Item {
    Video(VideoItem),
    Channel(ChannelItem),
    Playlist(PlaylistItem),
}

impl Item {
    pub fn key(&self) -> &str {
        match self {
            Item::Video(v) => &v.id,
            Item::Channel(c) => &c.id,
            Item::Playlist(p) => &p.id,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Chip {
    pub title: String,
    pub token: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Shelf {
    pub title: String,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default)]
pub struct FeedPage {
    pub items: Vec<Item>,
    pub continuation: Option<String>,
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    /// Section titles keyed by the index of their first item (history "Today", "Yesterday", ...).
    pub sections: Vec<(usize, String)>,
    /// Message YouTube shows instead of content (e.g. "Try searching to get started").
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    Url(String),
    Seek(u64),
    Video { id: String, start: Option<u64> },
    Channel(String),
    Search(String),
}

#[derive(Debug, Clone)]
pub struct TextLink {
    /// Byte offsets into the UTF-8 text.
    pub start: usize,
    pub end: usize,
    pub target: LinkTarget,
}

#[derive(Debug, Clone, Default)]
pub struct RichText {
    pub text: String,
    pub links: Vec<TextLink>,
}

#[derive(Debug, Clone, Default)]
pub struct PlaylistPanel {
    pub id: String,
    pub title: String,
    pub items: Vec<VideoItem>,
    pub current_index: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct VideoDetails {
    pub id: String,
    pub title: String,
    pub channel: ChannelItem,
    pub channel_verified: bool,
    pub subscribed: Option<bool>,
    pub views: Option<String>,
    pub views_short: Option<String>,
    pub date: Option<String>,
    pub relative_date: Option<String>,
    pub likes: Option<String>,
    pub like_status: LikeStatus,
    pub description: RichText,
    pub related: Vec<Item>,
    pub related_continuation: Option<String>,
    pub comments_token: Option<String>,
    pub comment_count: Option<String>,
    pub playlist: Option<PlaylistPanel>,
    pub is_live: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub author_avatar: Option<String>,
    pub author_channel_id: Option<String>,
    pub text: RichText,
    pub published: String,
    pub likes: Option<String>,
    pub reply_count: Option<String>,
    pub replies_token: Option<String>,
    pub pinned: bool,
    pub hearted: bool,
    pub is_creator: bool,
    pub verified: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CommentsPage {
    pub comments: Vec<Comment>,
    pub continuation: Option<String>,
    pub count: Option<String>,
    /// (title, continuation token, selected)
    pub sort_options: Vec<(String, String, bool)>,
}

#[derive(Debug, Clone, Default)]
pub struct ChannelTab {
    pub title: String,
    pub params: String,
}

#[derive(Debug, Clone, Default)]
pub struct ChannelPage {
    pub id: String,
    pub name: String,
    pub handle: Option<String>,
    pub avatar: Option<String>,
    pub banner: Option<String>,
    pub subscribers: Option<String>,
    pub video_count: Option<String>,
    pub description: Option<String>,
    pub subscribed: Option<bool>,
    pub verified: bool,
    pub tabs: Vec<ChannelTab>,
    /// Content of the tab that was loaded with the page.
    pub content: FeedPage,
}

#[derive(Debug, Clone, Default)]
pub struct PlaylistPage {
    pub id: String,
    pub title: String,
    pub owner: Option<String>,
    pub owner_id: Option<String>,
    pub stats: Vec<String>,
    pub description: Option<String>,
    pub thumbnails: Vec<Thumbnail>,
    pub content: FeedPage,
}
