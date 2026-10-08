//! Tolerant parsers for InnerTube (YouTube WEB client) responses.
//!
//! YouTube reshuffles its JSON regularly, so instead of hard-coding long paths
//! the feed parsers walk the tree and pick up every renderer they understand.
//! Shorts and ads are dropped on the way.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::model::*;

// ---------------------------------------------------------------------------
// Generic helpers
// ---------------------------------------------------------------------------

/// Extract display text from `{simpleText}`, `{runs:[{text}]}`, `{content}` or a plain string.
pub fn text(v: &Value) -> Option<String> {
    fn raw(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(map) => {
                if let Some(s) = map.get("simpleText").and_then(Value::as_str) {
                    Some(s.to_owned())
                } else if let Some(runs) = map.get("runs").and_then(Value::as_array) {
                    Some(
                        runs.iter()
                            .filter_map(|r| r.get("text").and_then(Value::as_str))
                            .collect(),
                    )
                } else if let Some(s) = map.get("content").and_then(Value::as_str) {
                    Some(s.to_owned())
                } else {
                    map.get("text").and_then(raw)
                }
            }
            _ => None,
        }
    }
    raw(v).filter(|s| !s.trim().is_empty())
}

fn text_at(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr).and_then(text)
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

/// Depth-first search for the first value stored under `key`.
pub fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if let Some(found) = map.get(key) {
                return Some(found);
            }
            map.values().find_map(|child| find_key(child, key))
        }
        Value::Array(arr) => arr.iter().find_map(|child| find_key(child, key)),
        _ => None,
    }
}

/// Collect every value stored under `key` (does not descend into matches).
pub fn find_all<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    fn rec<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    if k == key {
                        out.push(child);
                    } else {
                        rec(child, key, out);
                    }
                }
            }
            Value::Array(arr) => arr.iter().for_each(|c| rec(c, key, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    rec(v, key, &mut out);
    out
}

fn fix_url(u: &str) -> String {
    if let Some(rest) = u.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        u.to_owned()
    }
}

/// Parse thumbnails from `{thumbnails:[..]}`, `{sources:[..]}`, `{image:{sources}}` or an array.
pub fn thumbnails(v: &Value) -> Vec<Thumbnail> {
    let arr = v
        .get("thumbnails")
        .or_else(|| v.get("sources"))
        .or_else(|| v.pointer("/image/sources"))
        .and_then(Value::as_array)
        .or_else(|| v.as_array());
    arr.map(|a| {
        a.iter()
            .filter_map(|t| {
                let url = t.get("url")?.as_str()?;
                Some(Thumbnail {
                    url: fix_url(url),
                    width: t.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                    height: t.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn best_url(v: &Value, min_width: u32) -> Option<String> {
    let thumbs = thumbnails(v);
    pick_thumbnail(&thumbs, min_width).map(|t| t.url.clone())
}

fn browse_id(v: &Value) -> Option<String> {
    find_key(v, "browseEndpoint")
        .and_then(|e| e.get("browseId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn channel_browse_id(v: &Value) -> Option<String> {
    find_all(v, "browseEndpoint")
        .into_iter()
        .filter_map(|e| e.get("browseId").and_then(Value::as_str))
        .find(|id| id.starts_with("UC"))
        .map(str::to_owned)
}

fn continuation_token(v: &Value) -> Option<String> {
    find_key(v, "continuationCommand")
        .and_then(|c| c.get("token"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn looks_like_duration(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

fn is_views_text(t: &str, a11y: Option<&str>) -> bool {
    let l = t.to_ascii_lowercase();
    l.contains(" view")
        || l.ends_with("views")
        || l.contains("watching")
        || l.contains("waiting")
        || a11y.is_some_and(|a| a.contains(" view"))
}

fn is_time_text(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    l.contains(" ago")
        || l.starts_with("streamed")
        || l.starts_with("premiere")
        || l.starts_with("scheduled")
        || l.starts_with("updated")
        || l.starts_with("started")
}

/// True if the response says the request was handled as logged out.
pub fn is_logged_out(resp: &Value) -> bool {
    resp.pointer("/responseContext/mainAppWebResponseContext/loggedOut")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

// ---------------------------------------------------------------------------
// Feed walker
// ---------------------------------------------------------------------------

const SKIP_KEYS: &[&str] = &[
    // Shorts
    "reelShelfRenderer",
    "reelItemRenderer",
    "shortsLockupViewModel",
    // Ads
    "adSlotRenderer",
    "promotedVideoRenderer",
    "searchPyvRenderer",
    "promotedSparklesWebRenderer",
    "statementBannerRenderer",
    "bannerPromoRenderer",
    // Noise
    "frameworkUpdates",
    "engagementPanels",
    "topbar",
    "playerOverlays",
    "menu",
    "feedFilterChipBarRenderer",
    "chipBarViewModel",
    "sortFilterSubMenuRenderer",
    "thumbnailOverlays",
    "header",
];

/// Walks arbitrary InnerTube JSON and collects feed items in document order.
#[derive(Default)]
pub struct Collector {
    pub items: Vec<Item>,
    pub continuation: Option<String>,
    pub sections: Vec<(usize, String)>,
    seen: HashSet<String>,
    skip_rich_sections: bool,
    skip_shelves: bool,
}

impl Collector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Home-style feeds: skip inline "rich sections" (Shorts rows, news shelves, ...).
    pub fn skipping_rich_sections(mut self) -> Self {
        self.skip_rich_sections = true;
        self
    }

    pub fn skipping_shelves(mut self) -> Self {
        self.skip_shelves = true;
        self
    }

    pub fn collect(v: &Value) -> Self {
        let mut c = Self::default();
        c.walk(v);
        c
    }

    fn push(&mut self, item: Item) {
        if self.seen.insert(item.key().to_owned()) {
            self.items.push(item);
        }
    }

    pub fn walk(&mut self, v: &Value) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    self.visit(k, child);
                }
            }
            Value::Array(arr) => arr.iter().for_each(|c| self.walk(c)),
            _ => {}
        }
    }

    fn visit(&mut self, key: &str, child: &Value) {
        if SKIP_KEYS.contains(&key) {
            return;
        }
        match key {
            "videoRenderer"
            | "compactVideoRenderer"
            | "gridVideoRenderer"
            | "playlistVideoRenderer"
            | "videoWithContextRenderer" => {
                if let Some(v) = parse_video_renderer(child) {
                    self.push(Item::Video(v));
                }
            }
            "lockupViewModel" => {
                if let Some(item) = parse_lockup(child) {
                    self.push(item);
                }
            }
            "channelRenderer" | "gridChannelRenderer" => {
                if let Some(c) = parse_channel_renderer(child) {
                    self.push(Item::Channel(c));
                }
            }
            "playlistRenderer" | "gridPlaylistRenderer" | "radioRenderer" | "compactRadioRenderer"
            | "compactPlaylistRenderer" => {
                if let Some(p) = parse_playlist_renderer(child) {
                    self.push(Item::Playlist(p));
                }
            }
            "continuationItemRenderer" => {
                if let Some(t) = continuation_token(child) {
                    self.continuation = Some(t);
                }
            }
            "richSectionRenderer" if self.skip_rich_sections => {}
            "richShelfRenderer" | "shelfRenderer" if self.skip_shelves => {}
            "richShelfRenderer" | "shelfRenderer" if is_shorts_shelf(child) => {}
            "itemSectionRenderer" => {
                if let Some(title) = child
                    .pointer("/header/itemSectionHeaderRenderer/title")
                    .and_then(text)
                {
                    self.sections.push((self.items.len(), title));
                }
                self.walk(child);
            }
            _ => self.walk(child),
        }
    }

    pub fn into_page(self) -> FeedPage {
        FeedPage {
            items: self.items,
            continuation: self.continuation,
            sections: self.sections,
            ..Default::default()
        }
    }
}

fn is_shorts_shelf(v: &Value) -> bool {
    let icon = str_at(v, "/icon/iconType").unwrap_or("");
    icon.contains("SHORTS")
        || v.get("title")
            .and_then(text)
            .is_some_and(|t| t.trim() == "Shorts")
}

fn is_short_endpoint(ne: &Value) -> bool {
    ne.get("reelWatchEndpoint").is_some()
        || str_at(ne, "/commandMetadata/webCommandMetadata/url")
            .is_some_and(|u| u.starts_with("/shorts/"))
}

pub fn parse_video_renderer(v: &Value) -> Option<VideoItem> {
    let id = v.get("videoId")?.as_str()?.to_owned();
    if v.get("navigationEndpoint").is_some_and(is_short_endpoint) {
        return None;
    }
    let title = v
        .get("title")
        .and_then(text)
        .or_else(|| v.get("headline").and_then(text))
        .unwrap_or_default();

    let byline = ["longBylineText", "ownerText", "shortBylineText"]
        .iter()
        .find_map(|k| v.get(*k).filter(|x| text(x).is_some()));
    let channel_name = byline.and_then(text);
    let channel_id = byline.and_then(channel_browse_id);
    let channel_avatar = v
        .pointer("/channelThumbnailSupportedRenderers/channelThumbnailWithLinkRenderer/thumbnail")
        .or_else(|| v.get("channelThumbnail"))
        .and_then(|t| best_url(t, 68));

    let mut duration = v.get("lengthText").and_then(text);
    let mut live = false;
    let mut watched = None;
    if let Some(ovs) = v.get("thumbnailOverlays").and_then(Value::as_array) {
        for o in ovs {
            if let Some(ts) = o.get("thumbnailOverlayTimeStatusRenderer") {
                match str_at(ts, "/style").unwrap_or("") {
                    "SHORTS" => return None,
                    "LIVE" => live = true,
                    "UPCOMING" => {}
                    _ => {
                        if duration.is_none() {
                            duration = ts.get("text").and_then(text);
                        }
                    }
                }
            }
            if let Some(p) = o.pointer("/thumbnailOverlayResumePlaybackRenderer/percentDurationWatched")
            {
                watched = p.as_f64().map(|p| (p / 100.0).clamp(0.0, 1.0));
            }
        }
    }
    if let Some(badges) = v.get("badges").and_then(Value::as_array) {
        live |= badges.iter().any(|b| {
            str_at(b, "/metadataBadgeRenderer/style") == Some("BADGE_STYLE_TYPE_LIVE_NOW")
        });
    }
    let upcoming = v.get("upcomingEventData").is_some();

    let mut views = v
        .get("shortViewCountText")
        .and_then(text)
        .or_else(|| v.get("viewCountText").and_then(text));
    let mut published = v.get("publishedTimeText").and_then(text);
    if views.is_none() && published.is_none() {
        if let Some(info) = v.get("videoInfo").and_then(text) {
            for part in info.split(" • ").map(str::trim) {
                if is_views_text(part, None) {
                    views = Some(part.to_owned());
                } else if is_time_text(part) {
                    published = Some(part.to_owned());
                }
            }
        }
    }
    if upcoming && published.is_none() {
        published = Some("Upcoming".into());
    }

    Some(VideoItem {
        id,
        title,
        channel_name,
        channel_id,
        channel_avatar,
        thumbnails: v.get("thumbnail").map(thumbnails).unwrap_or_default(),
        duration: if live { None } else { duration },
        views,
        published,
        live,
        upcoming,
        watched_fraction: watched,
        playlist_id: str_at(v, "/navigationEndpoint/watchEndpoint/playlistId").map(str::to_owned),
        index: v
            .get("index")
            .and_then(text)
            .and_then(|s| s.trim().parse().ok()),
    })
}

struct MetaPart {
    text: String,
    a11y: Option<String>,
    channel_id: Option<String>,
}

fn metadata_rows(meta: &Value) -> Vec<Vec<MetaPart>> {
    let rows = meta
        .pointer("/metadata/contentMetadataViewModel/metadataRows")
        .and_then(Value::as_array);
    rows.map(|rows| {
        rows.iter()
            .map(|row| {
                row.get("metadataParts")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| {
                                let t = p.get("text")?;
                                Some(MetaPart {
                                    text: text(t)?,
                                    a11y: p
                                        .get("accessibilityLabel")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned),
                                    channel_id: t.get("commandRuns").and_then(channel_browse_id),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect()
    })
    .unwrap_or_default()
}

pub fn parse_lockup(v: &Value) -> Option<Item> {
    let id = v.get("contentId")?.as_str()?.to_owned();
    let ctype = v.get("contentType").and_then(Value::as_str).unwrap_or("");
    let on_tap = v.pointer("/rendererContext/commandContext/onTap/innertubeCommand");
    if on_tap.is_some_and(is_short_endpoint) {
        return None;
    }
    let meta = v.pointer("/metadata/lockupMetadataViewModel")?;
    let title = meta.get("title").and_then(text).unwrap_or_default();
    let rows = metadata_rows(meta);
    let avatar_vm = meta.pointer("/image/decoratedAvatarViewModel");
    let avatar_url = avatar_vm
        .and_then(|a| a.pointer("/avatar/avatarViewModel/image"))
        .and_then(|i| best_url(i, 68));
    let avatar_channel = avatar_vm.and_then(channel_browse_id);
    let image = v.get("contentImage").unwrap_or(&Value::Null);
    let badges: Vec<(String, String)> = find_all(image, "thumbnailBadgeViewModel")
        .into_iter()
        .map(|b| {
            (
                b.get("text").and_then(Value::as_str).unwrap_or("").to_owned(),
                b.get("badgeStyle").and_then(Value::as_str).unwrap_or("").to_owned(),
            )
        })
        .collect();

    match ctype {
        "LOCKUP_CONTENT_TYPE_VIDEO" => {
            let mut item = VideoItem {
                id,
                title,
                thumbnails: image
                    .pointer("/thumbnailViewModel/image")
                    .map(thumbnails)
                    .unwrap_or_default(),
                ..Default::default()
            };
            for (t, style) in &badges {
                let upper = t.to_ascii_uppercase();
                if style.contains("LIVE") || upper == "LIVE" {
                    item.live = true;
                } else if upper == "UPCOMING" || style.contains("UPCOMING") {
                    item.upcoming = true;
                } else if upper == "SHORTS" {
                    return None;
                } else if looks_like_duration(t) {
                    item.duration = Some(t.clone());
                }
            }
            item.watched_fraction = find_key(image, "thumbnailOverlayProgressBarViewModel")
                .and_then(|p| p.get("startPercent"))
                .and_then(Value::as_f64)
                .map(|p| (p / 100.0).clamp(0.0, 1.0));

            let multi_row = rows.len() > 1;
            for (ri, row) in rows.iter().enumerate() {
                for part in row {
                    if is_views_text(&part.text, part.a11y.as_deref()) {
                        let mut t = part.text.clone();
                        if !t.to_ascii_lowercase().contains("view")
                            && !t.to_ascii_lowercase().contains("watching")
                        {
                            t.push_str(" views");
                        }
                        item.views = Some(t);
                    } else if is_time_text(&part.text) {
                        item.published = Some(part.text.clone());
                    } else if item.channel_name.is_none()
                        && (part.channel_id.is_some() || (ri == 0 && multi_row))
                    {
                        item.channel_name = Some(part.text.clone());
                        item.channel_id = part.channel_id.clone();
                    }
                }
            }
            if item.channel_id.is_none() {
                item.channel_id = avatar_channel;
            }
            item.channel_avatar = avatar_url;
            if item.upcoming && item.published.is_none() {
                item.published = Some("Upcoming".into());
            }
            Some(Item::Video(item))
        }
        "LOCKUP_CONTENT_TYPE_PLAYLIST"
        | "LOCKUP_CONTENT_TYPE_ALBUM"
        | "LOCKUP_CONTENT_TYPE_PODCAST"
        | "LOCKUP_CONTENT_TYPE_STATION"
        | "LOCKUP_CONTENT_TYPE_COURSE" => {
            let thumbs = find_key(image, "thumbnailViewModel")
                .and_then(|t| t.get("image"))
                .map(thumbnails)
                .unwrap_or_default();
            let first_video_id = v
                .pointer("/itemPlayback/inlinePlayerData/onSelect/innertubeCommand/watchEndpoint/videoId")
                .or_else(|| on_tap.and_then(|t| t.pointer("/watchEndpoint/videoId")))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let (mut channel_name, mut channel_id) = (None, None);
            for part in rows.iter().flatten() {
                if part.channel_id.is_some() {
                    channel_name = Some(part.text.clone());
                    channel_id = part.channel_id.clone();
                    break;
                }
            }
            Some(Item::Playlist(PlaylistItem {
                is_mix: ctype == "LOCKUP_CONTENT_TYPE_STATION" || id.starts_with("RD"),
                id,
                title,
                thumbnails: thumbs,
                video_count: badges.first().map(|b| b.0.clone()).filter(|s| !s.is_empty()),
                channel_name,
                channel_id,
                first_video_id,
            }))
        }
        "LOCKUP_CONTENT_TYPE_CHANNEL" => Some(Item::Channel(ChannelItem {
            id,
            name: title,
            avatar: avatar_url,
            ..Default::default()
        })),
        _ => None,
    }
}

pub fn parse_channel_renderer(v: &Value) -> Option<ChannelItem> {
    let id = v.get("channelId")?.as_str()?.to_owned();
    let name = v.get("title").and_then(text).unwrap_or_default();
    let handle = str_at(v, "/navigationEndpoint/browseEndpoint/canonicalBaseUrl")
        .and_then(|u| u.strip_prefix('/'))
        .filter(|u| u.starts_with('@'))
        .map(str::to_owned);
    // YouTube swapped these fields around: the "subscriber" text may live in either.
    let texts: Vec<String> = ["subscriberCountText", "videoCountText"]
        .iter()
        .filter_map(|k| v.get(*k).and_then(text))
        .collect();
    let subscribers = texts.iter().find(|t| t.contains("subscriber")).cloned();
    let video_count = texts.iter().find(|t| t.contains("video")).cloned();
    Some(ChannelItem {
        id,
        name,
        avatar: v.get("thumbnail").and_then(|t| best_url(t, 88)),
        handle,
        subscribers,
        video_count,
        has_new_content: false,
    })
}

pub fn parse_playlist_renderer(v: &Value) -> Option<PlaylistItem> {
    let id = v.get("playlistId")?.as_str()?.to_owned();
    let title = v.get("title").and_then(text).unwrap_or_default();
    let thumbs = v
        .get("thumbnails")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .map(thumbnails)
        .or_else(|| v.get("thumbnail").map(thumbnails))
        .unwrap_or_default();
    let byline = v
        .get("longBylineText")
        .or_else(|| v.get("shortBylineText"));
    Some(PlaylistItem {
        is_mix: id.starts_with("RD"),
        id,
        title,
        thumbnails: thumbs,
        video_count: v
            .get("videoCountText")
            .and_then(text)
            .or_else(|| v.get("videoCount").and_then(Value::as_str).map(|c| format!("{c} videos"))),
        channel_name: byline.and_then(text),
        channel_id: byline.and_then(channel_browse_id),
        first_video_id: str_at(v, "/navigationEndpoint/watchEndpoint/videoId").map(str::to_owned),
    })
}

// ---------------------------------------------------------------------------
// Feeds (browse / search)
// ---------------------------------------------------------------------------

fn selected_tab(resp: &Value) -> Option<&Value> {
    let tabs = resp
        .pointer("/contents/twoColumnBrowseResultsRenderer/tabs")?
        .as_array()?;
    let mut first = None;
    for t in tabs {
        let Some(tr) = t.get("tabRenderer").or_else(|| t.get("expandableTabRenderer")) else {
            continue;
        };
        if tr.get("content").is_none() {
            continue;
        }
        if tr.get("selected").and_then(Value::as_bool) == Some(true) {
            return Some(tr);
        }
        first.get_or_insert(tr);
    }
    first
}

fn selected_tab_content(resp: &Value) -> Option<&Value> {
    selected_tab(resp).and_then(|t| t.get("content"))
}

pub fn parse_chips(v: &Value) -> Vec<Chip> {
    let mut chips = Vec::new();
    for c in find_all(v, "chipCloudChipRenderer") {
        let Some(title) = c.get("text").and_then(text) else {
            continue;
        };
        chips.push(Chip {
            title,
            token: c.get("navigationEndpoint").and_then(continuation_token),
            selected: c.get("isSelected").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    for c in find_all(v, "chipViewModel") {
        let Some(title) = c.get("text").and_then(text) else {
            continue;
        };
        chips.push(Chip {
            title,
            token: c.get("tapCommand").and_then(continuation_token),
            selected: c.get("selected").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    chips
}

fn feed_message(v: &Value) -> Option<String> {
    for key in ["feedNudgeRenderer", "backgroundPromoRenderer", "messageRenderer"] {
        if let Some(r) = find_key(v, key) {
            let title = r.get("title").and_then(text);
            let sub = r
                .get("subtitle")
                .and_then(text)
                .or_else(|| r.get("text").and_then(text))
                .or_else(|| r.get("bodyText").and_then(text));
            let msg = [title, sub].into_iter().flatten().collect::<Vec<_>>().join("\n");
            if !msg.is_empty() {
                return Some(msg);
            }
        }
    }
    None
}

fn walk_continuations(c: &mut Collector, resp: &Value) {
    for key in [
        "onResponseReceivedActions",
        "onResponseReceivedEndpoints",
        "onResponseReceivedCommands",
    ] {
        if let Some(arr) = resp.get(key) {
            c.walk(arr);
        }
    }
    // Old-style continuation responses
    if let Some(cc) = resp.get("continuationContents") {
        c.walk(cc);
    }
}

/// Browse response (home, subscriptions, channel tabs, playlists, history...) or its continuation.
pub fn parse_browse_feed(resp: &Value, skip_rich_sections: bool) -> FeedPage {
    let mut c = Collector::new();
    c.skip_rich_sections = skip_rich_sections;
    let content = selected_tab_content(resp);
    if let Some(content) = content {
        c.walk(content);
    }
    walk_continuations(&mut c, resp);
    let mut page = c.into_page();
    // Chip bars live in the grid header (or in a reload command when switching chips).
    let chip_root = content
        .and_then(|c| find_key(c, "feedFilterChipBarRenderer").or_else(|| find_key(c, "chipBarViewModel")))
        .or_else(|| find_key(resp, "feedFilterChipBarRenderer"))
        .or_else(|| find_key(resp, "chipBarViewModel"));
    if let Some(root) = chip_root {
        page.chips = parse_chips(root);
    }
    if page.items.is_empty() {
        page.message = feed_message(resp);
    }
    page
}

pub fn parse_search(resp: &Value) -> FeedPage {
    let mut c = Collector::new();
    if let Some(primary) = resp.pointer("/contents/twoColumnSearchResultsRenderer/primaryContents") {
        c.walk(primary);
    }
    walk_continuations(&mut c, resp);
    let mut page = c.into_page();
    if page.items.is_empty() {
        page.message = feed_message(resp);
    }
    page
}

/// Pages made of shelves (Explore destinations, channel home tabs).
pub fn parse_shelves(resp: &Value) -> FeedPage {
    let content = selected_tab_content(resp).unwrap_or(resp);
    let mut shelves = Vec::new();
    for key in ["richShelfRenderer", "shelfRenderer"] {
        for shelf in find_all(content, key) {
            if is_shorts_shelf(shelf) {
                continue;
            }
            let title = shelf.get("title").and_then(text).unwrap_or_default();
            let items = Collector::collect(shelf).items;
            if !items.is_empty() {
                shelves.push(Shelf { title, items });
            }
        }
    }
    let mut loose = Collector::new().skipping_shelves();
    loose.walk(content);
    walk_continuations(&mut loose, resp);
    let mut page = loose.into_page();
    page.shelves = shelves;
    page
}

// ---------------------------------------------------------------------------
// Rich text (descriptions, comments)
// ---------------------------------------------------------------------------

fn redirect_target(url: &str) -> String {
    if let Ok(u) = url::Url::parse(&fix_url(url)) {
        if u.path() == "/redirect" {
            if let Some((_, q)) = u.query_pairs().find(|(k, _)| k == "q") {
                return q.into_owned();
            }
        }
    }
    if url.starts_with('/') {
        return format!("https://www.youtube.com{url}");
    }
    url.to_owned()
}

fn command_target(cmd: &Value, video_id: &str) -> Option<LinkTarget> {
    if let Some(we) = cmd.get("watchEndpoint") {
        let id = we.get("videoId").and_then(Value::as_str)?.to_owned();
        let start = we.get("startTimeSeconds").and_then(Value::as_u64);
        return Some(match start {
            Some(t) if id == video_id => LinkTarget::Seek(t),
            _ => LinkTarget::Video { id, start },
        });
    }
    if let Some(be) = cmd.get("browseEndpoint") {
        let id = be.get("browseId").and_then(Value::as_str).unwrap_or("");
        if id.starts_with("UC") {
            return Some(LinkTarget::Channel(id.to_owned()));
        }
        if let Some(tag) = be
            .get("canonicalBaseUrl")
            .and_then(Value::as_str)
            .and_then(|u| u.strip_prefix("/hashtag/"))
        {
            return Some(LinkTarget::Search(format!("#{tag}")));
        }
        return None;
    }
    if let Some(u) = cmd.pointer("/urlEndpoint/url").and_then(Value::as_str) {
        return Some(LinkTarget::Url(redirect_target(u)));
    }
    if let Some(u) = str_at(cmd, "/commandMetadata/webCommandMetadata/url") {
        return Some(LinkTarget::Url(redirect_target(u)));
    }
    None
}

/// Parse an attributed string (`{content, commandRuns}`) into text + links.
pub fn parse_attributed(v: &Value, video_id: &str) -> RichText {
    let content = v
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // InnerTube offsets are UTF-16 code units; map them to byte offsets.
    let mut utf16_to_byte = Vec::with_capacity(content.len() + 1);
    for (byte_idx, ch) in content.char_indices() {
        for _ in 0..ch.len_utf16() {
            utf16_to_byte.push(byte_idx);
        }
    }
    utf16_to_byte.push(content.len());

    let mut links = Vec::new();
    if let Some(runs) = v.get("commandRuns").and_then(Value::as_array) {
        for run in runs {
            let (Some(start), Some(len)) = (
                run.get("startIndex").and_then(Value::as_u64),
                run.get("length").and_then(Value::as_u64),
            ) else {
                continue;
            };
            let Some(cmd) = run.pointer("/onTap/innertubeCommand") else {
                continue;
            };
            let Some(target) = command_target(cmd, video_id) else {
                continue;
            };
            let s = start as usize;
            let e = (start + len) as usize;
            if e >= utf16_to_byte.len() || s >= e {
                continue;
            }
            links.push(TextLink {
                start: utf16_to_byte[s],
                end: utf16_to_byte[e],
                target,
            });
        }
    }
    links.sort_by_key(|l| l.start);
    let mut text = RichText { text: content, links };
    if text.links.is_empty() {
        add_timestamp_links(&mut text);
    }
    text
}

/// Turn bare "12:34" style timestamps into seek links (used for comments without command runs).
pub fn add_timestamp_links(rt: &mut RichText) {
    let bytes = rt.text.as_bytes();
    let mut i = 0;
    let mut found = Vec::new();
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let start = i;
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b':') {
                j += 1;
            }
            let candidate = &rt.text[start..j];
            let candidate = candidate.trim_end_matches(':');
            let end = start + candidate.len();
            let boundary_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
            if boundary_ok && looks_like_duration(candidate) {
                if let Some(secs) = parse_duration(candidate) {
                    found.push(TextLink {
                        start,
                        end,
                        target: LinkTarget::Seek(secs),
                    });
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    if !found.is_empty() {
        rt.links.extend(found);
        rt.links.sort_by_key(|l| l.start);
    }
}

pub fn parse_duration(s: &str) -> Option<u64> {
    let mut total = 0u64;
    for part in s.split(':') {
        total = total * 60 + part.trim().parse::<u64>().ok()?;
    }
    Some(total)
}

// ---------------------------------------------------------------------------
// Watch page (/next)
// ---------------------------------------------------------------------------

pub fn parse_next(resp: &Value, video_id: &str) -> VideoDetails {
    let mut d = VideoDetails {
        id: video_id.to_owned(),
        ..Default::default()
    };
    let results = resp
        .pointer("/contents/twoColumnWatchNextResults/results/results/contents")
        .unwrap_or(&Value::Null);
    let primary = find_key(results, "videoPrimaryInfoRenderer").unwrap_or(&Value::Null);
    let secondary = find_key(results, "videoSecondaryInfoRenderer").unwrap_or(&Value::Null);

    d.title = primary.get("title").and_then(text).unwrap_or_default();
    if let Some(vc) = primary.pointer("/viewCount/videoViewCountRenderer") {
        d.views = vc.get("viewCount").and_then(text);
        d.views_short = vc.get("shortViewCount").and_then(text);
        d.is_live = vc.get("isLive").and_then(Value::as_bool).unwrap_or(false);
    }
    d.date = primary.get("dateText").and_then(text);
    d.relative_date = primary.get("relativeDateText").and_then(text);

    if let Some(actions) = primary.get("videoActions") {
        if let Some(like_vm) = find_key(actions, "likeButtonViewModel") {
            d.likes = find_key(like_vm, "buttonViewModel")
                .and_then(|b| b.get("title"))
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty() && *t != "Like")
                .map(str::to_owned);
            d.like_status = match find_key(like_vm, "likeStatusEntity")
                .and_then(|e| e.get("likeStatus"))
                .and_then(Value::as_str)
            {
                Some("LIKE") => LikeStatus::Like,
                Some("DISLIKE") => LikeStatus::Dislike,
                _ => LikeStatus::Indifferent,
            };
        }
    }

    if let Some(owner) = secondary.pointer("/owner/videoOwnerRenderer") {
        d.channel = ChannelItem {
            id: owner
                .get("navigationEndpoint")
                .and_then(browse_id)
                .unwrap_or_default(),
            name: owner.get("title").and_then(text).unwrap_or_default(),
            avatar: owner.get("thumbnail").and_then(|t| best_url(t, 88)),
            subscribers: owner.get("subscriberCountText").and_then(text),
            ..Default::default()
        };
        d.channel_verified = owner
            .get("badges")
            .and_then(Value::as_array)
            .is_some_and(|b| {
                b.iter().any(|b| {
                    str_at(b, "/metadataBadgeRenderer/style")
                        .is_some_and(|s| s.contains("VERIFIED"))
                })
            });
    }
    d.subscribed = subscribed_state(secondary).or_else(|| subscribed_state(resp));
    d.description = match secondary.get("attributedDescription") {
        Some(ad) => parse_attributed(ad, video_id),
        None => {
            let mut rt = RichText {
                text: secondary.get("description").and_then(text).unwrap_or_default(),
                links: Vec::new(),
            };
            add_timestamp_links(&mut rt);
            rt
        }
    };

    if let Some(sec) = resp.pointer("/contents/twoColumnWatchNextResults/secondaryResults") {
        let c = Collector::collect(sec);
        d.related = c.items;
        d.related_continuation = c.continuation;
    }

    for section in find_all(results, "itemSectionRenderer") {
        if str_at(section, "/sectionIdentifier") == Some("comment-item-section") {
            d.comments_token = continuation_token(section);
        }
    }
    if let Some(panels) = resp.get("engagementPanels").and_then(Value::as_array) {
        for p in panels {
            let Some(e) = p.get("engagementPanelSectionListRenderer") else {
                continue;
            };
            if str_at(e, "/targetId") == Some("engagement-panel-comments-section") {
                d.comment_count = e
                    .pointer("/header/engagementPanelTitleHeaderRenderer/contextualInfo")
                    .and_then(text);
                if d.comments_token.is_none() {
                    d.comments_token = e.get("content").and_then(continuation_token);
                }
            }
        }
    }

    if let Some(pl) = resp.pointer("/contents/twoColumnWatchNextResults/playlist/playlist") {
        let mut items = Vec::new();
        let mut current = None;
        if let Some(contents) = pl.get("contents").and_then(Value::as_array) {
            for c in contents {
                let Some(r) = c.get("playlistPanelVideoRenderer") else {
                    continue;
                };
                let Some(id) = r.get("videoId").and_then(Value::as_str) else {
                    continue;
                };
                if r.get("selected").and_then(Value::as_bool) == Some(true) {
                    current = Some(items.len());
                }
                let byline = r.get("shortBylineText").or_else(|| r.get("longBylineText"));
                items.push(VideoItem {
                    id: id.to_owned(),
                    title: r.get("title").and_then(text).unwrap_or_default(),
                    channel_name: byline.and_then(text),
                    channel_id: byline.and_then(channel_browse_id),
                    thumbnails: r.get("thumbnail").map(thumbnails).unwrap_or_default(),
                    duration: r.get("lengthText").and_then(text),
                    playlist_id: str_at(pl, "/playlistId").map(str::to_owned),
                    index: r
                        .get("indexText")
                        .and_then(text)
                        .and_then(|s| s.parse().ok()),
                    ..Default::default()
                });
            }
        }
        d.playlist = Some(PlaylistPanel {
            id: str_at(pl, "/playlistId").unwrap_or_default().to_owned(),
            title: pl.get("title").and_then(text).unwrap_or_default(),
            items,
            current_index: current,
        });
    }
    d
}

fn subscribed_state(v: &Value) -> Option<bool> {
    if let Some(b) = find_key(v, "subscribeButtonRenderer") {
        if let Some(s) = b.get("subscribed").and_then(Value::as_bool) {
            return Some(s);
        }
    }
    find_key(v, "subscriptionStateEntity")
        .and_then(|e| e.get("subscribed"))
        .and_then(Value::as_bool)
}

/// Continuation of the related-videos column.
pub fn parse_next_continuation(resp: &Value) -> FeedPage {
    let mut c = Collector::new();
    walk_continuations(&mut c, resp);
    c.into_page()
}

// ---------------------------------------------------------------------------
// Comments
// ---------------------------------------------------------------------------

pub fn parse_comments(resp: &Value, video_id: &str) -> CommentsPage {
    let mut entities: HashMap<&str, &Value> = HashMap::new();
    let mut toolbar_states: HashMap<&str, &Value> = HashMap::new();
    if let Some(muts) = resp
        .pointer("/frameworkUpdates/entityBatchUpdate/mutations")
        .and_then(Value::as_array)
    {
        for m in muts {
            let Some(key) = m.get("entityKey").and_then(Value::as_str) else {
                continue;
            };
            if let Some(p) = m.pointer("/payload/commentEntityPayload") {
                entities.insert(key, p);
            } else if let Some(p) = m.pointer("/payload/engagementToolbarStateEntityPayload") {
                toolbar_states.insert(key, p);
            }
        }
    }

    let mut page = CommentsPage::default();
    let mut items: Vec<&Value> = Vec::new();
    for key in ["onResponseReceivedEndpoints", "onResponseReceivedActions"] {
        if let Some(arr) = resp.get(key).and_then(Value::as_array) {
            for e in arr {
                for cmd in ["reloadContinuationItemsCommand", "appendContinuationItemsAction"] {
                    if let Some(ci) = e
                        .pointer(&format!("/{cmd}/continuationItems"))
                        .and_then(Value::as_array)
                    {
                        items.extend(ci.iter());
                    }
                }
            }
        }
    }

    let comment_from_vm = |cvm: &Value| -> Option<Comment> {
        let key = cvm.get("commentKey").and_then(Value::as_str)?;
        let p = entities.get(key)?;
        let props = p.get("properties").unwrap_or(&Value::Null);
        let author = p.get("author").unwrap_or(&Value::Null);
        let toolbar = p.get("toolbar").unwrap_or(&Value::Null);
        let mut rt = props
            .get("content")
            .map(|c| parse_attributed(c, video_id))
            .unwrap_or_default();
        if rt.links.is_empty() {
            add_timestamp_links(&mut rt);
        }
        let hearted = cvm
            .get("toolbarStateKey")
            .and_then(Value::as_str)
            .and_then(|k| toolbar_states.get(k))
            .and_then(|s| s.get("heartState"))
            .and_then(Value::as_str)
            == Some("TOOLBAR_HEART_STATE_HEARTED");
        let likes = toolbar
            .get("likeCountNotliked")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0")
            .map(str::to_owned);
        let reply_count = toolbar
            .get("replyCount")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "0")
            .map(str::to_owned);
        Some(Comment {
            id: props
                .get("commentId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            author: author
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            author_avatar: author
                .get("avatarThumbnailUrl")
                .and_then(Value::as_str)
                .map(str::to_owned),
            author_channel_id: author
                .get("channelId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            text: rt,
            published: props
                .get("publishedTime")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            likes,
            reply_count,
            replies_token: None,
            pinned: cvm.get("pinnedText").is_some(),
            hearted,
            is_creator: author.get("isCreator").and_then(Value::as_bool).unwrap_or(false),
            verified: author.get("isVerified").and_then(Value::as_bool).unwrap_or(false),
        })
    };

    for item in items {
        if let Some(h) = item.get("commentsHeaderRenderer") {
            page.count = h.get("countText").and_then(text);
            if let Some(menu) = find_key(h, "sortFilterSubMenuRenderer") {
                if let Some(subs) = menu.get("subMenuItems").and_then(Value::as_array) {
                    for s in subs {
                        let (Some(title), Some(token)) = (
                            s.get("title").and_then(text),
                            s.get("serviceEndpoint").and_then(continuation_token),
                        ) else {
                            continue;
                        };
                        let selected = s.get("selected").and_then(Value::as_bool).unwrap_or(false);
                        page.sort_options.push((title, token, selected));
                    }
                }
            }
        } else if let Some(th) = item.get("commentThreadRenderer") {
            let Some(cvm) = th.pointer("/commentViewModel/commentViewModel") else {
                continue;
            };
            if let Some(mut c) = comment_from_vm(cvm) {
                c.replies_token = th
                    .pointer("/replies/commentRepliesRenderer/contents")
                    .and_then(continuation_token);
                page.comments.push(c);
            }
        } else if let Some(cvm) = item.get("commentViewModel") {
            if let Some(c) = comment_from_vm(cvm) {
                page.comments.push(c);
            }
        } else if let Some(ci) = item.get("continuationItemRenderer") {
            page.continuation = continuation_token(ci);
        }
    }
    page
}

// ---------------------------------------------------------------------------
// Channels & playlists
// ---------------------------------------------------------------------------

const CHANNEL_TABS: &[&str] = &["Videos", "Live", "Playlists", "Podcasts", "Releases", "Courses"];

pub fn parse_channel(resp: &Value, id: &str) -> ChannelPage {
    let mut page = ChannelPage {
        id: id.to_owned(),
        ..Default::default()
    };
    if let Some(vm) = resp.pointer("/header/pageHeaderRenderer/content/pageHeaderViewModel") {
        page.name = vm
            .pointer("/title/dynamicTextViewModel/text")
            .and_then(text)
            .unwrap_or_default();
        page.verified = vm
            .pointer("/title/dynamicTextViewModel/text/attachmentRuns")
            .is_some();
        page.avatar = vm
            .pointer("/image/decoratedAvatarViewModel/avatar/avatarViewModel/image")
            .and_then(|i| best_url(i, 176));
        page.banner = vm
            .pointer("/banner/imageBannerViewModel/image")
            .and_then(|i| best_url(i, 1060));
        page.description = vm
            .pointer("/description/descriptionPreviewViewModel/description")
            .and_then(text);
        for row in metadata_rows(vm) {
            for part in row {
                if part.text.starts_with('@') {
                    page.handle = Some(part.text);
                } else if part.text.contains("subscriber") {
                    page.subscribers = Some(part.text);
                } else if part.text.contains("video") {
                    page.video_count = Some(part.text);
                }
            }
        }
    } else if let Some(h) = resp.pointer("/header/c4TabbedHeaderRenderer") {
        page.name = h.get("title").and_then(text).unwrap_or_default();
        page.avatar = h.get("avatar").and_then(|a| best_url(a, 176));
        page.banner = h.get("banner").and_then(|b| best_url(b, 1060));
        page.subscribers = h.get("subscriberCountText").and_then(text);
        page.video_count = h.get("videosCountText").and_then(text);
        page.handle = h.get("channelHandleText").and_then(text);
    }
    if let Some(md) = resp.pointer("/metadata/channelMetadataRenderer") {
        if page.name.is_empty() {
            page.name = md.get("title").and_then(text).unwrap_or_default();
        }
        if page.description.is_none() {
            page.description = md.get("description").and_then(text);
        }
        if page.avatar.is_none() {
            page.avatar = md.get("avatar").and_then(|a| best_url(a, 176));
        }
    }
    page.subscribed = resp.get("header").and_then(subscribed_state);

    if let Some(tabs) = resp
        .pointer("/contents/twoColumnBrowseResultsRenderer/tabs")
        .and_then(Value::as_array)
    {
        for t in tabs {
            let Some(tr) = t.get("tabRenderer") else {
                continue;
            };
            let (Some(title), Some(params)) = (
                tr.get("title").and_then(Value::as_str),
                str_at(tr, "/endpoint/browseEndpoint/params"),
            ) else {
                continue;
            };
            if CHANNEL_TABS.contains(&title) {
                page.tabs.push(ChannelTab {
                    title: title.to_owned(),
                    params: params.to_owned(),
                });
            }
        }
    }
    page.content = parse_browse_feed(resp, true);
    page
}

pub fn parse_playlist(resp: &Value, id: &str) -> PlaylistPage {
    let mut page = PlaylistPage {
        id: id.to_owned(),
        ..Default::default()
    };
    if let Some(vm) = resp.pointer("/header/pageHeaderRenderer/content/pageHeaderViewModel") {
        page.title = vm
            .pointer("/title/dynamicTextViewModel/text")
            .and_then(text)
            .unwrap_or_default();
        page.description = vm
            .pointer("/description/descriptionPreviewViewModel/description")
            .and_then(text);
        if let Some(img) = vm.get("heroImage").and_then(|h| find_key(h, "image")) {
            page.thumbnails = thumbnails(img);
        }
        if let Some(rows) = vm
            .pointer("/metadata/contentMetadataViewModel/metadataRows")
            .and_then(Value::as_array)
        {
            for row in rows {
                for part in row
                    .get("metadataParts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(stack) = part.get("avatarStack") {
                        let t = find_key(stack, "text").and_then(text).unwrap_or_default();
                        page.owner = Some(t.strip_prefix("by ").unwrap_or(&t).to_owned());
                        page.owner_id = channel_browse_id(stack);
                    } else if let Some(t) = part.get("text").and_then(text) {
                        page.stats.push(t);
                    }
                }
            }
        }
    } else if let Some(h) = resp.pointer("/header/playlistHeaderRenderer") {
        page.title = h.get("title").and_then(text).unwrap_or_default();
        page.owner = h.get("ownerText").and_then(text);
        page.owner_id = h.get("ownerText").and_then(channel_browse_id);
        page.description = h.get("descriptionText").and_then(text);
        if let Some(n) = h.get("numVideosText").and_then(text) {
            page.stats.push(n);
        }
    }
    if page.title.is_empty() {
        if let Some(t) = resp.pointer("/metadata/playlistMetadataRenderer/title").and_then(text) {
            page.title = t;
        }
    }
    page.content = parse_browse_feed(resp, true);
    if page.thumbnails.is_empty() {
        if let Some(Item::Video(v)) = page.content.items.first() {
            page.thumbnails = v.thumbnails.clone();
        }
    }
    page
}

// ---------------------------------------------------------------------------
// Account
// ---------------------------------------------------------------------------

pub fn parse_guide_subscriptions(resp: &Value) -> Vec<ChannelItem> {
    let Some(sec) = find_key(resp, "guideSubscriptionsSectionRenderer") else {
        return Vec::new();
    };
    let mut out: Vec<ChannelItem> = Vec::new();
    for e in find_all(sec, "guideEntryRenderer") {
        let Some(id) = str_at(e, "/navigationEndpoint/browseEndpoint/browseId") else {
            continue;
        };
        if !id.starts_with("UC") || out.iter().any(|c| c.id == id) {
            continue;
        }
        out.push(ChannelItem {
            id: id.to_owned(),
            name: e.get("formattedTitle").and_then(text).unwrap_or_default(),
            avatar: e.get("thumbnail").and_then(|t| best_url(t, 48)),
            has_new_content: str_at(e, "/presentationStyle")
                == Some("GUIDE_ENTRY_PRESENTATION_STYLE_NEW_CONTENT"),
            ..Default::default()
        });
    }
    out
}

pub fn parse_account_menu(resp: &Value) -> Option<AccountInfo> {
    let h = find_key(resp, "activeAccountHeaderRenderer")?;
    Some(AccountInfo {
        name: h.get("accountName").and_then(text).unwrap_or_default(),
        handle: h.get("channelHandle").and_then(text),
        avatar: h.get("accountPhoto").and_then(|p| best_url(p, 88)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_variants() {
        assert_eq!(text(&json!({"simpleText": "a"})).as_deref(), Some("a"));
        assert_eq!(
            text(&json!({"runs": [{"text": "a"}, {"text": "b"}]})).as_deref(),
            Some("ab")
        );
        assert_eq!(text(&json!({"content": "c"})).as_deref(), Some("c"));
        assert_eq!(text(&json!({"runs": []})), None);
    }

    #[test]
    fn durations() {
        assert!(looks_like_duration("13:16"));
        assert!(looks_like_duration("1:02:03"));
        assert!(!looks_like_duration("LIVE"));
        assert!(!looks_like_duration("12"));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
    }

    #[test]
    fn timestamp_links() {
        let mut rt = RichText {
            text: "intro 0:30, main part at 12:05 and 1:02:03.".into(),
            links: vec![],
        };
        add_timestamp_links(&mut rt);
        let targets: Vec<_> = rt.links.iter().map(|l| l.target.clone()).collect();
        assert_eq!(
            targets,
            vec![LinkTarget::Seek(30), LinkTarget::Seek(725), LinkTarget::Seek(3723)]
        );
        assert_eq!(&rt.text[rt.links[1].start..rt.links[1].end], "12:05");
    }

    #[test]
    fn attributed_utf16_offsets() {
        let v = json!({
            "content": "😀 see https://x.y/a…",
            "commandRuns": [{"startIndex": 7, "length": 14, "onTap": {"innertubeCommand": {
                "urlEndpoint": {"url": "https://www.youtube.com/redirect?event=x&q=https%3A%2F%2Fx.y%2Fabc"}}}}]
        });
        let rt = parse_attributed(&v, "vid");
        assert_eq!(rt.links.len(), 1);
        assert_eq!(&rt.text[rt.links[0].start..rt.links[0].end], "https://x.y/a…");
        assert_eq!(rt.links[0].target, LinkTarget::Url("https://x.y/abc".into()));
    }

    fn fixture(name: &str) -> Option<Value> {
        let dir = std::env::var("SLOPTUBE_FIXTURES").ok()?;
        let data = std::fs::read(format!("{dir}/{name}.json")).ok()?;
        serde_json::from_slice(&data).ok()
    }

    #[test]
    fn fixtures_search() {
        let Some(v) = fixture("search") else { return };
        let page = parse_search(&v);
        assert!(page.items.len() >= 10, "{}", page.items.len());
        let Item::Video(first) = &page.items[0] else {
            panic!("expected video")
        };
        assert!(!first.title.is_empty());
        assert!(first.channel_id.as_deref().unwrap_or("").starts_with("UC"));
        assert!(first.duration.is_some());
        assert!(page.continuation.is_some());
    }

    #[test]
    fn fixtures_next() {
        let Some(v) = fixture("next") else { return };
        let d = parse_next(&v, "aqz-KE-bpKQ");
        assert!(d.title.starts_with("Big Buck Bunny"));
        assert_eq!(d.channel.id, "UCSMOQeBJ2RAnuFungnQOxLg");
        assert!(d.likes.is_some());
        assert!(d.related.len() >= 10);
        assert!(d.comments_token.is_some());
        assert!(!d.description.links.is_empty());
        let Item::Video(r) = &d.related[0] else {
            panic!("expected video")
        };
        assert!(r.views.as_deref().unwrap_or("").contains("view"), "{r:?}");
        assert!(r.published.is_some(), "{r:?}");
        assert!(r.channel_name.is_some(), "{r:?}");
    }

    #[test]
    fn fixtures_comments() {
        let Some(v) = fixture("comments") else { return };
        let c = parse_comments(&v, "aqz-KE-bpKQ");
        assert!(c.comments.len() >= 10);
        assert!(c.continuation.is_some());
        assert!(!c.sort_options.is_empty());
        assert!(c.comments.iter().any(|c| c.replies_token.is_some()));
    }

    #[test]
    fn fixtures_channel() {
        let Some(v) = fixture("channel") else { return };
        let ch = parse_channel(&v, "UCSMOQeBJ2RAnuFungnQOxLg");
        assert_eq!(ch.name, "Blender");
        assert_eq!(ch.handle.as_deref(), Some("@BlenderOfficial"));
        assert!(ch.subscribers.is_some());
        assert!(ch.tabs.iter().any(|t| t.title == "Videos"));
        let Some(v) = fixture("channel_videos") else { return };
        let page = parse_browse_feed(&v, true);
        assert!(page.items.len() >= 20);
        assert!(page.continuation.is_some());
    }

    #[test]
    fn fixtures_playlist() {
        let Some(v) = fixture("playlist") else { return };
        let pl = parse_playlist(&v, "PLa1F2ddGya_-UvuAqHAksYnB0qL9yWDO6");
        assert_eq!(pl.title, "Blender Fundamentals 2.8");
        assert_eq!(pl.owner.as_deref(), Some("Blender"));
        assert!(pl.content.items.len() >= 20);
    }
}
