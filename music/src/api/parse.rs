//! Tolerant parsers for YouTube Music (InnerTube `WEB_REMIX`) responses.
//!
//! YouTube rearranges its JSON often, so these look for the renderers they
//! know wherever they are instead of following long fixed paths: a response
//! whose layout changes tends to lose a field rather than the whole page.

use serde_json::Value;

use super::model::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The text of a `{simpleText}` or `{runs: [...]}` object.
pub fn text(v: &Value) -> Option<String> {
    if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    if let Some(s) = v.get("content").and_then(Value::as_str) {
        return Some(s.to_owned());
    }
    let runs = v.get("runs")?.as_array()?;
    Some(runs.iter().filter_map(|r| r.get("text").and_then(Value::as_str)).collect())
}

fn text_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(text).unwrap_or_default()
}

fn runs(v: &Value) -> &[Value] {
    v.get("runs").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer)?.as_str()
}

fn string_at(v: &Value, pointer: &str) -> Option<String> {
    str_at(v, pointer).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// First value under `key`, depth first.
pub fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if let Some(x) = map.get(key) {
                return Some(x);
            }
            map.values().find_map(|x| find_key(x, key))
        }
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// All values under `key` (not looking inside matches).
fn find_all<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(map) => {
            for (k, x) in map {
                if k == key {
                    out.push(x);
                } else {
                    find_all(x, key, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| find_all(x, key, out)),
        _ => {}
    }
}

/// The first `thumbnails` list under `v`.
pub fn thumbnails(v: &Value) -> Vec<Thumbnail> {
    let Some(list) = find_key(v, "thumbnails").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|t| {
            Some(Thumbnail {
                url: t.get("url")?.as_str()?.to_owned(),
                width: t.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                height: t.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
            })
        })
        .collect()
}

fn browse_target(endpoint: &Value) -> Option<BrowseTarget> {
    let b = endpoint.get("browseEndpoint")?;
    let id = b.get("browseId")?.as_str()?.to_owned();
    let params = b.get("params").and_then(Value::as_str).map(str::to_owned);
    let page_type = str_at(b, "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType");
    let kind = page_type
        .map(PageKind::from_page_type)
        .filter(|k| *k != PageKind::Other)
        .unwrap_or_else(|| PageKind::from_browse_id(&id));
    Some(BrowseTarget { id, params, kind })
}

fn watch_target(endpoint: &Value) -> Option<WatchTarget> {
    let w = endpoint.get("watchEndpoint").or_else(|| endpoint.get("watchPlaylistEndpoint"))?;
    let t = WatchTarget {
        video_id: string_at(w, "/videoId"),
        playlist_id: string_at(w, "/playlistId"),
        params: string_at(w, "/params"),
    };
    (t.video_id.is_some() || t.playlist_id.is_some()).then_some(t)
}

fn music_video_type(endpoint: &Value) -> Option<&str> {
    str_at(endpoint, "/watchEndpoint/watchEndpointMusicSupportedConfigs/watchEndpointMusicConfig/musicVideoType")
}

fn is_video_type(t: &str) -> bool {
    matches!(
        t,
        "MUSIC_VIDEO_TYPE_OMV" | "MUSIC_VIDEO_TYPE_UGC" | "MUSIC_VIDEO_TYPE_OFFICIAL_SOURCE_MUSIC"
    )
}

/// The account's rating of an item, from its menu's like button (signed in only).
fn like_status(r: &Value) -> Option<bool> {
    let menu = r.get("menu")?;
    match find_key(menu, "likeStatus")?.as_str()? {
        "LIKE" => Some(true),
        "INDIFFERENT" | "DISLIKE" => Some(false),
        _ => None,
    }
}

fn is_explicit(r: &Value) -> bool {
    r.get("badges")
        .is_some_and(|b| b.to_string().contains("MUSIC_EXPLICIT_BADGE"))
}

fn wide_thumbnails(thumbs: &[Thumbnail]) -> bool {
    thumbs
        .iter()
        .max_by_key(|t| t.width)
        .is_some_and(|t| t.height > 0 && t.width * 10 > t.height * 13)
}

/// The continuation token of a shelf or section list, in either of the shapes YouTube uses.
fn continuation(container: &Value) -> Option<String> {
    if let Some(c) = container.get("continuations").and_then(Value::as_array) {
        for c in c {
            for kind in ["nextContinuationData", "nextRadioContinuationData", "reloadContinuationData"] {
                if let Some(t) = string_at(c, &format!("/{kind}/continuation")) {
                    return Some(t);
                }
            }
        }
    }
    let last = container.get("contents").and_then(Value::as_array)?.last()?;
    string_at(last, "/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
}

// ---------------------------------------------------------------------------
// Subtitle runs: "Song • Daft Punk & Pharrell Williams • Random Access Memories • 4:09"
// ---------------------------------------------------------------------------

const KIND_WORDS: &[&str] = &[
    "Song", "Video", "Album", "Single", "EP", "Playlist", "Artist", "Episode", "Podcast", "Profile", "Station",
    "Chart", "Mix", "Audiobook",
];

enum Segment {
    Artists(Vec<ArtistRef>),
    Album(AlbumRef),
    Duration(u32),
    Kind(String),
    Extra(String),
    Year,
    Plain(String),
}

fn is_dot(t: &str) -> bool {
    matches!(t.trim(), "•" | "·")
}

fn is_joiner(t: &str) -> bool {
    matches!(t.trim(), "" | "&" | "," | "and" | "x" | "×")
}

fn split_segments(runs: &[Value]) -> Vec<Vec<&Value>> {
    let mut out = vec![Vec::new()];
    for r in runs {
        let t = r.get("text").and_then(Value::as_str).unwrap_or("");
        if is_dot(t) {
            out.push(Vec::new());
        } else {
            out.last_mut().unwrap().push(r);
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

fn run_text(r: &Value) -> &str {
    r.get("text").and_then(Value::as_str).unwrap_or("")
}

fn run_target(r: &Value) -> Option<BrowseTarget> {
    r.get("navigationEndpoint").and_then(browse_target)
}

fn classify(seg: &[&Value]) -> Segment {
    let targets: Vec<Option<BrowseTarget>> = seg.iter().map(|r| run_target(r)).collect();
    if targets
        .iter()
        .flatten()
        .any(|t| matches!(t.kind, PageKind::Artist | PageKind::Profile))
    {
        let artists = seg
            .iter()
            .zip(&targets)
            .filter(|(r, _)| !is_joiner(run_text(r)))
            .map(|(r, t)| ArtistRef {
                name: run_text(r).trim().to_owned(),
                id: t
                    .as_ref()
                    .filter(|t| matches!(t.kind, PageKind::Artist | PageKind::Profile))
                    .map(|t| t.id.clone()),
            })
            .collect();
        return Segment::Artists(artists);
    }
    if let Some((r, t)) = seg
        .iter()
        .zip(&targets)
        .find_map(|(r, t)| t.as_ref().filter(|t| t.kind == PageKind::Album).map(|t| (r, t)))
    {
        return Segment::Album(AlbumRef {
            name: run_text(r).trim().to_owned(),
            id: t.id.clone(),
        });
    }
    let t: String = seg.iter().map(|r| run_text(r)).collect();
    let t = t.trim();
    if let Some(d) = parse_duration(t) {
        Segment::Duration(d)
    } else if KIND_WORDS.contains(&t) {
        Segment::Kind(t.to_owned())
    } else if t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()) {
        Segment::Year
    } else if ["plays", "views", "likes", "listeners", "subscribers", "audience", "songs", "song", "episodes"]
        .iter()
        .any(|w| t.ends_with(w))
    {
        Segment::Extra(t.to_owned())
    } else {
        Segment::Plain(t.to_owned())
    }
}

fn artists_in(runs: &[Value]) -> Vec<ArtistRef> {
    split_segments(runs)
        .iter()
        .find_map(|s| match classify(s) {
            Segment::Artists(a) => Some(a),
            _ => None,
        })
        .unwrap_or_default()
}

fn card_kind(page: PageKind, subtitle: &str) -> CardKind {
    let first = subtitle.split(" • ").next().unwrap_or("").trim();
    match page {
        PageKind::Album => match first {
            "Single" => CardKind::Single,
            "EP" => CardKind::Ep,
            _ => CardKind::Album,
        },
        PageKind::Artist => CardKind::Artist,
        PageKind::Profile => CardKind::Profile,
        PageKind::Podcast => CardKind::Podcast,
        _ => CardKind::Playlist,
    }
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// Parse one entry of a shelf's `contents`.
pub fn parse_item(v: &Value) -> Option<Item> {
    if let Some(r) = v.get("musicTwoRowItemRenderer") {
        return parse_two_row(r);
    }
    if let Some(r) = v.get("musicResponsiveListItemRenderer") {
        return parse_list_item(r);
    }
    if let Some(r) = v.get("musicNavigationButtonRenderer") {
        return parse_nav_button(r);
    }
    None
}

fn play_overlay(r: &Value) -> Option<WatchTarget> {
    let overlay = r.get("thumbnailOverlay").or_else(|| r.get("overlay"))?;
    overlay
        .pointer("/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint")
        .and_then(watch_target)
}

fn parse_two_row(r: &Value) -> Option<Item> {
    let title = text_of(r, "title");
    let nav = r.get("navigationEndpoint")?;
    let thumbs = r.get("thumbnailRenderer").map(thumbnails).unwrap_or_default();
    let sub_runs = r.get("subtitle").map(runs).unwrap_or(&[]);
    let subtitle = text_of(r, "subtitle");
    let artists = artists_in(sub_runs);
    let play = play_overlay(r);

    if let Some(w) = nav.get("watchEndpoint") {
        let video_id = string_at(w, "/videoId")?;
        let wide = str_at(r, "/aspectRatio").is_some_and(|a| a.contains("16_9")) || wide_thumbnails(&thumbs);
        let extra = split_segments(sub_runs).iter().rev().find_map(|s| match classify(s) {
            Segment::Extra(e) => Some(e),
            _ => None,
        });
        return Some(Item::Track(Track {
            video_id,
            title,
            artists,
            thumbnails: thumbs,
            is_video: music_video_type(nav).map(is_video_type).unwrap_or(wide),
            extra,
            playlist_id: string_at(w, "/playlistId"),
            ..Default::default()
        }));
    }
    if nav.get("watchPlaylistEndpoint").is_some() {
        let play = watch_target(nav);
        let id = play.as_ref().and_then(|p| p.playlist_id.clone())?;
        return Some(Item::Card(Card {
            kind: CardKind::Mix,
            target: BrowseTarget {
                id: format!("VL{id}"),
                params: None,
                kind: PageKind::Playlist,
            },
            title,
            subtitle,
            artists,
            thumbnails: thumbs,
            play,
        }));
    }
    let target = browse_target(nav)?;
    Some(Item::Card(Card {
        kind: card_kind(target.kind, &subtitle),
        target,
        title,
        subtitle,
        artists,
        thumbnails: thumbs,
        play,
    }))
}

fn flex_columns(r: &Value) -> Vec<&Value> {
    r.get("flexColumns")
        .and_then(Value::as_array)
        .map(|cols| {
            cols.iter()
                .filter_map(|c| c.pointer("/musicResponsiveListItemFlexColumnRenderer/text"))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_list_item(r: &Value) -> Option<Item> {
    let cols = flex_columns(r);
    let title_v = cols.first()?;
    let title = text(title_v).unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    let title_nav = runs(title_v).first().and_then(|r| r.get("navigationEndpoint"));
    let thumbs = r.get("thumbnail").map(thumbnails).unwrap_or_default();
    let overlay = play_overlay(r);
    let video_id = string_at(r, "/playlistItemData/videoId")
        .or_else(|| title_nav.and_then(|n| string_at(n, "/watchEndpoint/videoId")))
        .or_else(|| overlay.as_ref().and_then(|o| o.video_id.clone()));

    // Albums, artists and playlists open a page; songs, videos and episodes play.
    if let Some(target) = r
        .get("navigationEndpoint")
        .and_then(browse_target)
        .filter(|t| t.kind != PageKind::Episode)
    {
        let parts: Vec<String> = cols[1..]
            .iter()
            .filter_map(|c| text(c))
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        let subtitle = parts.join(" • ");
        let artists = cols[1..].iter().flat_map(|c| artists_in(runs(c))).collect();
        return Some(Item::Card(Card {
            kind: card_kind(target.kind, &subtitle),
            target,
            title,
            subtitle,
            artists,
            thumbnails: thumbs,
            play: overlay,
        }));
    }

    let video_id = video_id?;
    let mut t = Track {
        video_id,
        title,
        thumbnails: thumbs,
        ..Default::default()
    };
    let mvt = title_nav
        .and_then(music_video_type)
        .or_else(|| {
            r.pointer("/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint")
                .and_then(music_video_type)
        });
    t.is_video = mvt.map(is_video_type).unwrap_or_else(|| wide_thumbnails(&t.thumbnails));
    for (i, col) in cols[1..].iter().enumerate() {
        for seg in split_segments(runs(col)) {
            match classify(&seg) {
                Segment::Artists(a) => {
                    if t.artists.is_empty() {
                        t.artists = a;
                    }
                }
                Segment::Album(a) => t.album = Some(a),
                Segment::Duration(d) => t.duration = Some(d),
                Segment::Kind(k) => match k.as_str() {
                    "Video" => t.is_video = true,
                    "Song" | "Episode" => t.is_video = false,
                    _ => {}
                },
                Segment::Extra(e) => t.extra = Some(e),
                Segment::Year => {}
                // An uploader without a channel link, e.g. on community playlists.
                Segment::Plain(p) => {
                    if t.artists.is_empty() && i == 0 && !p.is_empty() {
                        t.artists.push(ArtistRef { name: p, id: None });
                    }
                }
            }
        }
    }
    if let Some(fixed) = r.get("fixedColumns").and_then(Value::as_array) {
        for c in fixed {
            if let Some(d) = c
                .pointer("/musicResponsiveListItemFixedColumnRenderer/text")
                .and_then(text)
                .and_then(|s| parse_duration(&s))
            {
                t.duration = Some(d);
            }
        }
    }
    t.index = r.get("index").and_then(text).and_then(|s| s.trim().parse().ok());
    t.liked = like_status(r);
    t.explicit = is_explicit(r);
    t.unavailable = str_at(r, "/musicItemRendererDisplayPolicy") == Some("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT");
    t.playlist_id = title_nav.and_then(|n| string_at(n, "/watchEndpoint/playlistId"));
    t.set_video_id = string_at(r, "/playlistItemData/playlistSetVideoId");
    Some(Item::Track(t))
}

fn parse_nav_button(r: &Value) -> Option<Item> {
    let target = r.get("clickCommand").and_then(browse_target)?;
    Some(Item::Mood(MoodTile {
        title: text_of(r, "buttonText"),
        color: r
            .pointer("/solid/leftStripeColor")
            .and_then(Value::as_u64)
            .map(|c| c as u32),
        target,
    }))
}

/// A queue entry (`playlistPanelVideoRenderer`).
fn parse_panel_video(r: &Value) -> Option<Track> {
    let nav = r.get("navigationEndpoint");
    let video_id = string_at(r, "/videoId").or_else(|| nav.and_then(|n| string_at(n, "/watchEndpoint/videoId")))?;
    let mut t = Track {
        video_id,
        title: text_of(r, "title"),
        thumbnails: r.get("thumbnail").map(thumbnails).unwrap_or_default(),
        duration: r.get("lengthText").and_then(text).and_then(|s| parse_duration(&s)),
        explicit: is_explicit(r),
        playlist_id: nav.and_then(|n| string_at(n, "/watchEndpoint/playlistId")),
        set_video_id: string_at(r, "/playlistSetVideoId"),
        ..Default::default()
    };
    t.is_video = nav
        .and_then(music_video_type)
        .map(is_video_type)
        .unwrap_or_else(|| wide_thumbnails(&t.thumbnails));
    t.liked = like_status(r);
    let byline = r.get("longBylineText").or_else(|| r.get("shortBylineText"));
    for (i, seg) in byline.map(runs).map(split_segments).unwrap_or_default().iter().enumerate() {
        match classify(seg) {
            Segment::Artists(a) if t.artists.is_empty() => t.artists = a,
            Segment::Album(a) => t.album = Some(a),
            Segment::Plain(p) if i == 0 && t.artists.is_empty() => t.artists.push(ArtistRef { name: p, id: None }),
            _ => {}
        }
    }
    Some(t)
}

// ---------------------------------------------------------------------------
// Shelves
// ---------------------------------------------------------------------------

fn shelf_kind(items: &[Item], list_items: usize) -> ShelfKind {
    if !items.is_empty() && items.iter().all(|i| matches!(i, Item::Mood(_))) {
        ShelfKind::Moods
    } else if list_items * 2 > items.len() {
        ShelfKind::TrackColumns
    } else {
        ShelfKind::Carousel
    }
}

fn contents_items(x: &Value, key: &str) -> (Vec<Item>, usize) {
    let mut list_items = 0;
    let items = x
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    if v.get("musicResponsiveListItemRenderer").is_some() {
                        list_items += 1;
                    }
                    parse_item(v)
                })
                .collect()
        })
        .unwrap_or_default();
    (items, list_items)
}

fn carousel(x: &Value) -> Option<Shelf> {
    let h = x
        .pointer("/header/musicCarouselShelfBasicHeaderRenderer")
        .or_else(|| x.pointer("/header/musicCarouselShelfHeaderRenderer"))
        .unwrap_or(&Value::Null);
    let (items, list_items) = contents_items(x, "contents");
    if items.is_empty() {
        return None;
    }
    let more = h
        .pointer("/moreContentButton/buttonRenderer/navigationEndpoint")
        .and_then(browse_target)
        .or_else(|| runs(h.get("title").unwrap_or(&Value::Null)).first().and_then(run_target));
    Some(Shelf {
        title: text_of(h, "title"),
        strapline: h.get("strapline").and_then(text).filter(|s| !s.is_empty()),
        more,
        kind: shelf_kind(&items, list_items),
        items,
        ..Default::default()
    })
}

fn list_shelf(x: &Value) -> Option<Shelf> {
    let (items, _) = contents_items(x, "contents");
    let continuation = continuation(x);
    if items.is_empty() && continuation.is_none() {
        return None;
    }
    let title_v = x.get("title").unwrap_or(&Value::Null);
    let more = x
        .get("bottomEndpoint")
        .and_then(browse_target)
        .or_else(|| runs(title_v).first().and_then(run_target));
    Some(Shelf {
        title: text(title_v).unwrap_or_default(),
        more,
        kind: ShelfKind::List,
        items,
        continuation,
        ..Default::default()
    })
}

fn grid(x: &Value) -> Option<Shelf> {
    let (items, _) = contents_items(x, "items");
    if items.is_empty() {
        return None;
    }
    let title = x
        .pointer("/header/gridHeaderRenderer/title")
        .and_then(text)
        .unwrap_or_default();
    let kind = if items.iter().all(|i| matches!(i, Item::Mood(_))) {
        ShelfKind::Moods
    } else {
        ShelfKind::Grid
    };
    Some(Shelf {
        title,
        kind,
        items,
        continuation: continuation(x),
        ..Default::default()
    })
}

/// The "Top result" card in search.
fn card_shelf(x: &Value) -> Option<Shelf> {
    let title_runs = runs(x.get("title").unwrap_or(&Value::Null));
    let nav = title_runs
        .first()
        .and_then(|r| r.get("navigationEndpoint"))
        .or_else(|| x.get("onTap"));
    let subtitle = text_of(x, "subtitle");
    let thumbs = x.get("thumbnail").map(thumbnails).unwrap_or_default();
    let title = text_of(x, "title");
    let mut items = Vec::new();
    if let Some(nav) = nav {
        if let Some(target) = browse_target(nav) {
            items.push(Item::Card(Card {
                kind: card_kind(target.kind, &subtitle),
                target,
                title,
                artists: artists_in(runs(x.get("subtitle").unwrap_or(&Value::Null))),
                subtitle,
                thumbnails: thumbs,
                play: None,
            }));
        } else if let Some(w) = watch_target(nav).filter(|w| w.video_id.is_some()) {
            let sub_runs = runs(x.get("subtitle").unwrap_or(&Value::Null));
            let mut t = Track {
                video_id: w.video_id.unwrap_or_default(),
                title,
                artists: artists_in(sub_runs),
                thumbnails: thumbs,
                playlist_id: w.playlist_id,
                ..Default::default()
            };
            t.is_video = music_video_type(nav)
                .map(is_video_type)
                .unwrap_or_else(|| wide_thumbnails(&t.thumbnails));
            for seg in split_segments(sub_runs) {
                match classify(&seg) {
                    Segment::Album(a) => t.album = Some(a),
                    Segment::Duration(d) => t.duration = Some(d),
                    _ => {}
                }
            }
            items.push(Item::Track(t));
        }
    }
    let (more, _) = contents_items(x, "contents");
    items.extend(more);
    if items.is_empty() {
        return None;
    }
    let header = x
        .pointer("/header/musicCardShelfHeaderBasicRenderer/title")
        .and_then(text)
        .unwrap_or_else(|| "Top result".into());
    Some(Shelf {
        title: header,
        kind: ShelfKind::List,
        items,
        ..Default::default()
    })
}

fn description_shelf(x: &Value) -> Option<Shelf> {
    let body = text_of(x, "description");
    if body.trim().is_empty() {
        return None;
    }
    let title = x.get("header").and_then(text).filter(|t| !t.is_empty()).unwrap_or_else(|| "About".into());
    Some(Shelf {
        title,
        kind: ShelfKind::Text,
        text: Some(body.replace("\r\n", "\n")),
        ..Default::default()
    })
}

/// Collect every shelf under `v`, in order.
fn walk_shelves(v: &Value, out: &mut Vec<Shelf>) {
    match v {
        Value::Object(map) => {
            for (k, x) in map {
                match k.as_str() {
                    "musicCarouselShelfRenderer" => out.extend(carousel(x)),
                    "musicShelfRenderer" | "musicPlaylistShelfRenderer" => out.extend(list_shelf(x)),
                    "gridRenderer" => out.extend(grid(x)),
                    "musicCardShelfRenderer" => out.extend(card_shelf(x)),
                    "musicDescriptionShelfRenderer" => out.extend(description_shelf(x)),
                    "musicResponsiveListItemRenderer" | "musicTwoRowItemRenderer" => {
                        // Loose results: search puts each in a section of its own.
                        let wrapped = serde_json::json!({ k.as_str(): x });
                        if let Some(item) = parse_item(&wrapped) {
                            match out.last_mut() {
                                Some(s) if s.kind == ShelfKind::List && s.title.is_empty() => s.items.push(item),
                                _ => out.push(Shelf {
                                    kind: ShelfKind::List,
                                    items: vec![item],
                                    ..Default::default()
                                }),
                            }
                        }
                    }
                    "header" | "frameworkUpdates" | "responseContext" | "musicTastebuilderShelfRenderer"
                    | "musicSideAlignedItemRenderer" | "menu" | "chipCloudRenderer" | "trackingParams" => {}
                    _ => walk_shelves(x, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| walk_shelves(x, out)),
        _ => {}
    }
}

fn chips(v: &Value) -> Vec<Chip> {
    let mut found = Vec::new();
    find_all(v, "chipCloudChipRenderer", &mut found);
    found
        .into_iter()
        .filter_map(|c| {
            let nav = c.get("navigationEndpoint")?;
            let params = string_at(nav, "/browseEndpoint/params").or_else(|| string_at(nav, "/searchEndpoint/params"));
            Some(Chip {
                title: text_of(c, "text"),
                params,
                selected: c.get("isSelected").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .filter(|c| !c.title.is_empty())
        .collect()
}

fn header_title(v: &Value) -> Option<String> {
    let h = v.get("header")?;
    ["musicHeaderRenderer", "musicImmersiveHeaderRenderer", "musicVisualHeaderRenderer", "musicResponsiveHeaderRenderer"]
        .iter()
        .find_map(|k| find_key(h, k))
        .and_then(|r| r.get("title"))
        .and_then(text)
        .filter(|t| !t.is_empty())
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// Home, explore, moods, charts, a mood category, new releases, search results...
pub fn parse_browse(v: &Value) -> BrowsePage {
    let mut page = BrowsePage {
        title: header_title(v),
        ..Default::default()
    };
    let contents = v.get("contents").unwrap_or(&Value::Null);
    if let Some(sl) = find_key(contents, "sectionListRenderer") {
        if let Some(c) = sl.get("contents") {
            walk_shelves(c, &mut page.shelves);
        }
        page.continuation = continuation(sl);
        if let Some(h) = sl.get("header") {
            page.chips = chips(h);
        }
    } else {
        walk_shelves(contents, &mut page.shelves);
    }
    page
}

/// A further page of a sectioned browse response (home).
pub fn parse_browse_continuation(v: &Value) -> BrowsePage {
    let mut page = BrowsePage::default();
    if let Some(c) = v.pointer("/continuationContents/sectionListContinuation") {
        if let Some(contents) = c.get("contents") {
            walk_shelves(contents, &mut page.shelves);
        }
        page.continuation = continuation(c);
    } else if let Some(actions) = v.get("onResponseReceivedActions") {
        walk_shelves(actions, &mut page.shelves);
        page.continuation = find_key(actions, "continuationCommand")
            .and_then(|c| c.get("token"))
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    page
}

/// More items of a list shelf (filtered search results, playlist tracks).
pub fn parse_shelf_continuation(v: &Value) -> (Vec<Item>, Option<String>) {
    if let Some(c) = v.get("continuationContents") {
        for key in ["musicShelfContinuation", "musicPlaylistShelfContinuation", "gridContinuation"] {
            if let Some(s) = c.get(key) {
                let field = if key == "gridContinuation" { "items" } else { "contents" };
                let (items, _) = contents_items(s, field);
                return (items, continuation(s));
            }
        }
    }
    // Newer shape: appendContinuationItemsAction.
    if let Some(items) = find_key(v.get("onResponseReceivedActions").unwrap_or(&Value::Null), "continuationItems")
        .and_then(Value::as_array)
    {
        let parsed = items.iter().filter_map(parse_item).collect();
        let token = items
            .last()
            .and_then(|l| string_at(l, "/continuationItemRenderer/continuationEndpoint/continuationCommand/token"));
        return (parsed, token);
    }
    (Vec::new(), None)
}

fn track_items(items: Vec<Item>) -> Vec<Track> {
    items
        .into_iter()
        .filter_map(|i| match i {
            Item::Track(t) => Some(t),
            _ => None,
        })
        .collect()
}

/// An album (`MPREb_…`) or playlist (`VL…`).
pub fn parse_collection(v: &Value, id: &str) -> CollectionPage {
    let kind = if id.starts_with("MPRE") {
        PageKind::Album
    } else {
        PageKind::Playlist
    };
    let mut page = CollectionPage {
        id: id.to_owned(),
        kind,
        ..Default::default()
    };
    let header = ["musicResponsiveHeaderRenderer", "musicDetailHeaderRenderer", "musicImmersiveHeaderRenderer"]
        .iter()
        .find_map(|k| find_key(v, k));
    if let Some(h) = header {
        page.title = text_of(h, "title");
        page.subtitle = text_of(h, "subtitle");
        page.second_subtitle = text_of(h, "secondSubtitle");
        page.description = h
            .get("description")
            .and_then(|d| find_key(d, "description").and_then(text).or_else(|| text(d)))
            .map(|d| d.replace("\r\n", "\n"))
            .filter(|d| !d.trim().is_empty());
        page.thumbnails = h.get("thumbnail").map(thumbnails).unwrap_or_default();
        if let Some(strap) = h.get("straplineTextOne") {
            for seg in split_segments(runs(strap)) {
                match classify(&seg) {
                    Segment::Artists(a) => page.artists.extend(a),
                    Segment::Plain(p) if !p.is_empty() => page.artists.push(ArtistRef { name: p, id: None }),
                    _ => {}
                }
            }
        }
        if page.artists.is_empty() {
            // Playlists name their owner in a "facepile".
            if let Some(owner) = h
                .get("facepile")
                .and_then(|f| find_key(f, "text"))
                .and_then(text)
                .filter(|t| !t.is_empty())
            {
                page.artists.push(ArtistRef { name: owner, id: None });
            }
        }
        if let Some(buttons) = h.get("buttons") {
            page.playlist_id = find_key(buttons, "playNavigationEndpoint")
                .and_then(watch_target)
                .and_then(|w| w.playlist_id);
        }
    }

    let shelf = find_key(v, "musicPlaylistShelfRenderer").or_else(|| {
        // The album's own track list, not an artist-style shelf elsewhere.
        v.pointer("/contents/twoColumnBrowseResultsRenderer/secondaryContents")
            .and_then(|s| find_key(s, "musicShelfRenderer"))
            .or_else(|| find_key(v, "musicShelfRenderer"))
    });
    if let Some(s) = shelf {
        let (items, _) = contents_items(s, "contents");
        page.tracks = track_items(items);
        page.continuation = continuation(s);
        if page.playlist_id.is_none() {
            page.playlist_id = string_at(s, "/playlistId");
        }
    }
    if page.playlist_id.is_none() {
        page.playlist_id = match kind {
            PageKind::Playlist => Some(id.trim_start_matches("VL").to_owned()),
            _ => page.tracks.iter().find_map(|t| t.playlist_id.clone()),
        };
    }
    if let Some(secondary) = v.pointer("/contents/twoColumnBrowseResultsRenderer/secondaryContents") {
        let mut shelves = Vec::new();
        walk_shelves(secondary, &mut shelves);
        page.shelves = shelves.into_iter().filter(|s| s.kind != ShelfKind::List).collect();
    }

    if kind == PageKind::Album {
        let album = AlbumRef {
            name: page.title.clone(),
            id: id.to_owned(),
        };
        for t in &mut page.tracks {
            if t.artists.is_empty() {
                t.artists = page.artists.clone();
            }
            if t.album.is_none() {
                t.album = Some(album.clone());
            }
            if t.thumbnails.is_empty() {
                t.thumbnails = page.thumbnails.clone();
            }
        }
    }
    page
}

/// More tracks of a playlist.
pub fn parse_collection_continuation(v: &Value) -> (Vec<Track>, Option<String>) {
    let (items, token) = parse_shelf_continuation(v);
    (track_items(items), token)
}

pub fn parse_artist(v: &Value, id: &str) -> ArtistPage {
    let mut page = ArtistPage {
        id: id.to_owned(),
        ..Default::default()
    };
    let header = v.get("header").and_then(|h| {
        ["musicImmersiveHeaderRenderer", "musicVisualHeaderRenderer", "musicHeaderRenderer"]
            .iter()
            .find_map(|k| find_key(h, k))
    });
    if let Some(h) = header {
        page.name = text_of(h, "title");
        page.description = h.get("description").and_then(text).filter(|d| !d.trim().is_empty());
        page.thumbnails = h
            .get("thumbnail")
            .or_else(|| h.get("foregroundThumbnail"))
            .map(thumbnails)
            .unwrap_or_default();
        page.subscribers = h
            .pointer("/subscriptionButton/subscribeButtonRenderer/subscriberCountText")
            .and_then(text)
            .filter(|s| !s.is_empty());
        page.audience = h.get("monthlyListenerCount").and_then(text).filter(|s| !s.is_empty());
        page.shuffle = h
            .pointer("/playButton/buttonRenderer/navigationEndpoint")
            .and_then(watch_target);
        page.radio = h
            .pointer("/startRadioButton/buttonRenderer/navigationEndpoint")
            .and_then(watch_target);
    }
    if let Some(sl) = find_key(v.get("contents").unwrap_or(&Value::Null), "sectionListRenderer") {
        walk_shelves(sl.get("contents").unwrap_or(&Value::Null), &mut page.shelves);
    }
    if page.description.is_none() {
        page.description = page
            .shelves
            .iter()
            .find(|s| s.kind == ShelfKind::Text)
            .and_then(|s| s.text.clone());
    }
    page
}

fn parse_panel(panel: &Value, q: &mut WatchQueue) {
    for (i, item) in panel
        .get("contents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let r = item
            .get("playlistPanelVideoRenderer")
            .or_else(|| item.pointer("/playlistPanelVideoWrapperRenderer/primaryRenderer/playlistPanelVideoRenderer"));
        let Some(r) = r else { continue };
        if let Some(t) = parse_panel_video(r) {
            if r.get("selected").and_then(Value::as_bool) == Some(true) && q.selected.is_none() {
                q.selected = Some(q.tracks.len());
            }
            let _ = i;
            q.tracks.push(t);
        }
    }
    q.continuation = continuation(panel);
    if q.playlist_id.is_none() {
        q.playlist_id = string_at(panel, "/playlistId");
    }
}

/// The `next` response: queue, lyrics and related browse ids.
pub fn parse_next(v: &Value) -> WatchQueue {
    let mut q = WatchQueue::default();
    if let Some(panel) = find_key(v, "playlistPanelRenderer") {
        parse_panel(panel, &mut q);
    }
    if let Some(tabs) = find_key(v, "watchNextTabbedResultsRenderer")
        .and_then(|t| t.get("tabs"))
        .and_then(Value::as_array)
    {
        for tab in tabs {
            let Some(r) = tab.get("tabRenderer") else { continue };
            if r.get("unselectable").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(b) = r.get("endpoint").and_then(|e| e.get("browseEndpoint")) else {
                continue;
            };
            let id = b.get("browseId").and_then(Value::as_str).map(str::to_owned);
            match str_at(b, "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType") {
                Some("MUSIC_PAGE_TYPE_TRACK_LYRICS") => q.lyrics = id,
                Some("MUSIC_PAGE_TYPE_TRACK_RELATED") => q.related = id,
                _ => {}
            }
        }
    }
    q
}

pub fn parse_next_continuation(v: &Value) -> WatchQueue {
    let mut q = WatchQueue::default();
    if let Some(panel) = v.pointer("/continuationContents/playlistPanelContinuation") {
        parse_panel(panel, &mut q);
    }
    q
}

pub fn parse_lyrics(v: &Value) -> Option<Lyrics> {
    let d = find_key(v.get("contents")?, "musicDescriptionShelfRenderer")?;
    let text = text_of(d, "description").replace("\r\n", "\n");
    if text.trim().is_empty() {
        return None;
    }
    Some(Lyrics {
        text,
        source: d.get("footer").and_then(super::parse::text).filter(|s| !s.is_empty()),
    })
}

/// The signed-in account, or `None` when the session isn't signed in.
pub fn parse_account_menu(v: &Value) -> Option<AccountInfo> {
    let h = find_key(v, "activeAccountHeaderRenderer")?;
    let name = h.get("accountName").and_then(text).unwrap_or_default();
    if name.is_empty() {
        return None;
    }
    Some(AccountInfo {
        name,
        handle: h.get("channelHandle").and_then(text).filter(|s| !s.is_empty()),
        avatar: h
            .get("accountPhoto")
            .map(thumbnails)
            .and_then(|t| thumb_url(&t, 96)),
    })
}

pub fn parse_suggestions(v: &Value) -> Suggestions {
    let mut s = Suggestions::default();
    let mut found = Vec::new();
    find_all(v, "searchSuggestionRenderer", &mut found);
    for r in found {
        let q = r
            .pointer("/navigationEndpoint/searchEndpoint/query")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| r.get("suggestion").and_then(text));
        if let Some(q) = q.filter(|q| !q.is_empty()) {
            s.queries.push(q);
        }
    }
    let mut items = Vec::new();
    find_all(v, "musicResponsiveListItemRenderer", &mut items);
    s.items = items.into_iter().filter_map(parse_list_item).collect();
    s
}

#[cfg(test)]
mod tests {
    //! Run against real responses saved by `tools/fetch-fixtures.py`:
    //! `SLOPTUBE_MUSIC_FIXTURES=target/fixtures cargo test`.

    use super::*;

    fn fixture(name: &str) -> Option<Value> {
        let dir = std::env::var("SLOPTUBE_MUSIC_FIXTURES").ok()?;
        let data = std::fs::read(std::path::Path::new(&dir).join(format!("{name}.json"))).ok()?;
        Some(serde_json::from_slice(&data).expect("bad fixture"))
    }

    fn tracks(shelf: &Shelf) -> Vec<&Track> {
        shelf
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Track(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn home() {
        let Some(v) = fixture("home") else { return };
        let page = parse_browse(&v);
        assert!(page.shelves.len() >= 2, "{:#?}", page.shelves.len());
        assert!(page.chips.len() >= 4);
        assert!(page.continuation.is_some());
        for s in &page.shelves {
            assert!(!s.title.is_empty());
            assert!(!s.items.is_empty());
        }
        if let Some(v) = fixture("home_continuation") {
            let more = parse_browse_continuation(&v);
            assert!(!more.shelves.is_empty());
        }
    }

    #[test]
    fn explore_and_moods() {
        if let Some(v) = fixture("explore") {
            let page = parse_browse(&v);
            assert!(page.shelves.iter().any(|s| s.kind == ShelfKind::Moods));
            assert!(page.shelves.iter().any(|s| s.kind == ShelfKind::Carousel));
        }
        if let Some(v) = fixture("moods") {
            let page = parse_browse(&v);
            assert!(page.shelves.len() >= 2);
            assert!(page.shelves.iter().all(|s| s.kind == ShelfKind::Moods));
            assert!(!page.shelves[0].title.is_empty());
        }
        if let Some(v) = fixture("mood") {
            let page = parse_browse(&v);
            assert!(!page.shelves.is_empty());
        }
        if let Some(v) = fixture("new_releases") {
            let page = parse_browse(&v);
            assert_eq!(page.shelves[0].kind, ShelfKind::Grid);
            assert!(page.shelves[0].items.len() > 10);
        }
        if let Some(v) = fixture("charts") {
            assert!(!parse_browse(&v).shelves.is_empty());
        }
    }

    #[test]
    fn search() {
        let Some(v) = fixture("search") else { return };
        let page = parse_browse(&v);
        assert!(page.chips.iter().any(|c| c.title == "Songs" && c.params.is_some()));
        let all: Vec<&Item> = page.shelves.iter().flat_map(|s| &s.items).collect();
        assert!(all.iter().any(|i| matches!(i, Item::Card(c) if c.kind == CardKind::Album)));
        assert!(all.iter().any(|i| matches!(i, Item::Card(c) if c.kind == CardKind::Artist)));
        assert!(all.iter().any(|i| matches!(i, Item::Track(t) if !t.artists.is_empty())));
        if let Some(v) = fixture("search_songs") {
            let page = parse_browse(&v);
            let songs = tracks(&page.shelves[0]);
            assert!(songs.len() >= 10);
            for t in songs {
                assert!(!t.artists.is_empty(), "{t:?}");
                assert!(t.duration.is_some(), "{t:?}");
                assert!(!t.is_video, "{t:?}");
            }
        }
    }

    #[test]
    fn artist() {
        let Some(v) = fixture("artist") else { return };
        let page = parse_artist(&v, "UC_kRDKYrUlrbtrSiyu5Tflg");
        assert_eq!(page.name, "Daft Punk");
        assert!(page.description.is_some());
        assert!(page.shuffle.is_some() && page.radio.is_some());
        assert!(!page.thumbnails.is_empty());
        let top = &page.shelves[0];
        assert_eq!(top.kind, ShelfKind::List);
        assert!(top.more.is_some());
        assert!(tracks(top).iter().all(|t| t.album.is_some() && !t.artists.is_empty()));
        assert!(page.shelves.iter().any(|s| s.title == "Albums" && s.kind == ShelfKind::Carousel));
    }

    #[test]
    fn album_and_playlist() {
        if let Some(v) = fixture("album") {
            let page = parse_collection(&v, "MPREb_K8qWMWVqXGi");
            assert_eq!(page.title, "Random Access Memories");
            assert_eq!(page.tracks.len(), 13);
            assert!(page.playlist_id.as_deref().is_some_and(|p| p.starts_with("OLAK")));
            assert_eq!(page.artists[0].name, "Daft Punk");
            assert!(page.artists[0].id.is_some());
            for (i, t) in page.tracks.iter().enumerate() {
                assert_eq!(t.index, Some(i as u32 + 1));
                assert!(t.duration.is_some());
                assert!(!t.artists.is_empty());
            }
        }
        if let Some(v) = fixture("playlist") {
            let page = parse_collection(&v, "VLRDCLAK5uy_n20FRYQXNt1p1wS55Nj2r14IouO5weaYU");
            assert!(!page.title.is_empty());
            assert!(page.tracks.len() >= 10);
            assert!(page.tracks.iter().all(|t| !t.artists.is_empty() && t.duration.is_some()));
        }
    }

    #[test]
    fn account_menu() {
        let v = serde_json::json!({"actions": [{"openPopupAction": {"popup": {"multiPageMenuRenderer": {"header": {
            "activeAccountHeaderRenderer": {
                "accountName": {"runs": [{"text": "Ada"}]},
                "channelHandle": {"runs": [{"text": "@ada"}]},
                "accountPhoto": {"thumbnails": [{"url": "https://yt3.ggpht.com/abc=s88-c-k", "width": 88, "height": 88}]}
            }}}}}}]});
        let a = parse_account_menu(&v).unwrap();
        assert_eq!(a.name, "Ada");
        assert_eq!(a.handle.as_deref(), Some("@ada"));
        assert!(a.avatar.unwrap().ends_with("=w96-h96-l90-rj"));
        assert!(parse_account_menu(&serde_json::json!({"actions": []})).is_none());
    }

    #[test]
    fn queue() {
        let Some(v) = fixture("next") else { return };
        let q = parse_next(&v);
        assert!(q.tracks.len() >= 10);
        assert_eq!(q.selected, Some(0));
        assert!(q.continuation.is_some());
        assert!(q.related.is_some());
        assert!(q.tracks.iter().all(|t| !t.artists.is_empty() && t.duration.is_some()));
        if let Some(v) = fixture("next_continuation") {
            let more = parse_next_continuation(&v);
            assert!(more.tracks.len() >= 10);
            assert!(more.continuation.is_some());
        }
        if let Some(v) = fixture("lyrics") {
            let l = parse_lyrics(&v).expect("lyrics");
            assert!(l.text.lines().count() > 10);
        }
        if let Some(v) = fixture("related") {
            let page = parse_browse(&v);
            assert!(page.shelves.len() >= 2);
        }
    }
}
