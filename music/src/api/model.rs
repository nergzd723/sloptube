//! Plain data types for YouTube Music content.

use serde::{Deserialize, Serialize};

pub use common::innertube::model::{LikeStatus, Thumbnail, thumb_url};

/// What a browse id leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PageKind {
    Album,
    Playlist,
    Artist,
    /// A user's channel (same page layout as an artist).
    Profile,
    Podcast,
    Episode,
    #[default]
    Other,
}

impl PageKind {
    pub fn from_page_type(t: &str) -> Self {
        match t {
            "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK" => PageKind::Album,
            "MUSIC_PAGE_TYPE_PLAYLIST" => PageKind::Playlist,
            "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_LIBRARY_ARTIST" => PageKind::Artist,
            "MUSIC_PAGE_TYPE_USER_CHANNEL" => PageKind::Profile,
            "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE" => PageKind::Podcast,
            "MUSIC_PAGE_TYPE_NON_MUSIC_AUDIO_TRACK_PAGE" => PageKind::Episode,
            _ => PageKind::Other,
        }
    }

    /// Guess from the id alone (links, saved items).
    pub fn from_browse_id(id: &str) -> Self {
        if id.starts_with("MPRE") {
            PageKind::Album
        } else if id.starts_with("VL") || id.starts_with("PL") || id.starts_with("OLAK") || id.starts_with("RDCLAK") {
            PageKind::Playlist
        } else if id.starts_with("UC") {
            PageKind::Artist
        } else if id.starts_with("MPSP") {
            PageKind::Podcast
        } else {
            PageKind::Other
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowseTarget {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    #[serde(default)]
    pub kind: PageKind,
}

impl BrowseTarget {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        let kind = PageKind::from_browse_id(&id);
        Self { id, params: None, kind }
    }

    pub fn with_params(id: impl Into<String>, params: Option<String>) -> Self {
        Self {
            params,
            ..Self::new(id)
        }
    }
}

/// Something to start playing: a song (optionally within a playlist) or a playlist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumRef {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<AlbumRef>,
    /// Seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u32>,
    #[serde(default)]
    pub thumbnails: Vec<Thumbnail>,
    /// A music video rather than an audio track (16:9 artwork).
    #[serde(default)]
    pub is_video: bool,
    #[serde(default)]
    pub explicit: bool,
    /// "1.9B plays", "880M views", ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
    /// Track number on an album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_video_id: Option<String>,
    #[serde(default)]
    pub unavailable: bool,
    /// The signed-in account's rating, when the response says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liked: Option<bool>,
}

impl Track {
    pub fn artist_names(&self) -> String {
        join_artists(&self.artists)
    }

    /// "Artist • Album" (or just the artists).
    pub fn byline(&self) -> String {
        let mut parts = Vec::new();
        let artists = self.artist_names();
        if !artists.is_empty() {
            parts.push(artists);
        }
        if let Some(a) = &self.album {
            parts.push(a.name.clone());
        }
        parts.join(" • ")
    }

    pub fn first_artist_id(&self) -> Option<String> {
        self.artists.iter().find_map(|a| a.id.clone())
    }

    pub fn url(&self) -> String {
        format!("https://music.youtube.com/watch?v={}", self.video_id)
    }
}

pub fn join_artists(artists: &[ArtistRef]) -> String {
    match artists.len() {
        0 => String::new(),
        1 => artists[0].name.clone(),
        n => {
            let head: Vec<&str> = artists[..n - 1].iter().map(|a| a.name.as_str()).collect();
            format!("{} & {}", head.join(", "), artists[n - 1].name)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CardKind {
    #[default]
    Album,
    Single,
    Ep,
    Playlist,
    Artist,
    Profile,
    /// A radio or mix that is played rather than opened.
    Mix,
    Podcast,
}

impl CardKind {
    pub fn round(self) -> bool {
        matches!(self, CardKind::Artist | CardKind::Profile)
    }

    pub fn label(self) -> &'static str {
        match self {
            CardKind::Album => "Album",
            CardKind::Single => "Single",
            CardKind::Ep => "EP",
            CardKind::Playlist => "Playlist",
            CardKind::Artist => "Artist",
            CardKind::Profile => "Profile",
            CardKind::Mix => "Mix",
            CardKind::Podcast => "Podcast",
        }
    }
}

/// An album, playlist, artist or mix shown as a tile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub kind: CardKind,
    pub target: BrowseTarget,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub thumbnails: Vec<Thumbnail>,
    /// What the play button on the tile starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play: Option<WatchTarget>,
}

/// A colourful "moods & genres" button.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoodTile {
    pub title: String,
    /// 0xAARRGGBB
    pub color: Option<u32>,
    pub target: BrowseTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Track(Track),
    Card(Card),
    Mood(MoodTile),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShelfKind {
    /// A horizontal row of tiles.
    #[default]
    Carousel,
    /// Songs in columns of four, scrolled sideways ("Quick picks").
    TrackColumns,
    /// A vertical list of songs or entities.
    List,
    /// Tiles that wrap.
    Grid,
    /// Mood and genre buttons.
    Moods,
    /// A block of text (artist description).
    Text,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shelf {
    pub title: String,
    pub strapline: Option<String>,
    /// "More" / "Show all".
    pub more: Option<BrowseTarget>,
    pub kind: ShelfKind,
    pub items: Vec<Item>,
    pub text: Option<String>,
    /// Further items of a list shelf (search results).
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chip {
    pub title: String,
    pub params: Option<String>,
    pub selected: bool,
}

/// A page of shelves: home, explore, moods, charts, search results, ...
#[derive(Debug, Clone, Default)]
pub struct BrowsePage {
    pub title: Option<String>,
    pub chips: Vec<Chip>,
    pub shelves: Vec<Shelf>,
    pub continuation: Option<String>,
}

/// An album or a playlist.
#[derive(Debug, Clone, Default)]
pub struct CollectionPage {
    pub id: String,
    pub kind: PageKind,
    pub title: String,
    pub subtitle: String,
    pub second_subtitle: String,
    pub description: Option<String>,
    pub artists: Vec<ArtistRef>,
    pub thumbnails: Vec<Thumbnail>,
    /// Playlist id for playing everything (`OLAK…` for albums, `PL…`/`RD…` for playlists).
    pub playlist_id: Option<String>,
    pub tracks: Vec<Track>,
    pub continuation: Option<String>,
    pub shelves: Vec<Shelf>,
}

impl CollectionPage {
    pub fn card(&self) -> Card {
        let kind = match self.kind {
            PageKind::Album => {
                if self.subtitle.starts_with("Single") {
                    CardKind::Single
                } else if self.subtitle.starts_with("EP") {
                    CardKind::Ep
                } else {
                    CardKind::Album
                }
            }
            _ => CardKind::Playlist,
        };
        Card {
            kind,
            target: BrowseTarget {
                id: self.id.clone(),
                params: None,
                kind: self.kind,
            },
            title: self.title.clone(),
            subtitle: self.subtitle.clone(),
            artists: self.artists.clone(),
            thumbnails: self.thumbnails.clone(),
            play: self.playlist_id.as_ref().map(|p| WatchTarget {
                playlist_id: Some(p.clone()),
                ..Default::default()
            }),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub thumbnails: Vec<Thumbnail>,
    pub subscribers: Option<String>,
    pub audience: Option<String>,
    pub shuffle: Option<WatchTarget>,
    pub radio: Option<WatchTarget>,
    pub shelves: Vec<Shelf>,
}

impl ArtistPage {
    pub fn card(&self) -> Card {
        Card {
            kind: CardKind::Artist,
            target: BrowseTarget {
                id: self.id.clone(),
                params: None,
                kind: PageKind::Artist,
            },
            title: self.name.clone(),
            subtitle: self.audience.clone().or_else(|| self.subscribers.clone()).unwrap_or_default(),
            artists: Vec::new(),
            thumbnails: self.thumbnails.clone(),
            play: None,
        }
    }
}

/// The queue from the `next` endpoint, plus where to find lyrics and related content.
#[derive(Debug, Clone, Default)]
pub struct WatchQueue {
    pub tracks: Vec<Track>,
    pub continuation: Option<String>,
    pub playlist_id: Option<String>,
    /// Index of the track the response marks as playing.
    pub selected: Option<usize>,
    pub lyrics: Option<String>,
    pub related: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lyrics {
    pub text: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Suggestions {
    pub queries: Vec<String>,
    pub items: Vec<Item>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_joining() {
        let a = |n: &str| ArtistRef {
            name: n.into(),
            id: None,
        };
        assert_eq!(join_artists(&[a("A")]), "A");
        assert_eq!(join_artists(&[a("A"), a("B"), a("C")]), "A, B & C");
    }
}
