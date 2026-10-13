//! SlopTube's InnerTube endpoints, on the shared `WEB` client core
//! (`common::innertube::client`: context, signing, visitor data, errors).

use std::ops::Deref;
use std::sync::OnceLock;

use anyhow::{Result, anyhow, bail};
use common::innertube::client as core;
pub use common::innertube::client::{USER_AGENT, http};
use serde_json::{Value, json};

use super::model::*;
use super::parse;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchFilter {
    #[default]
    All,
    Videos,
    Channels,
    Playlists,
    Live,
}

impl SearchFilter {
    fn params(self) -> Option<&'static str> {
        match self {
            SearchFilter::All => None,
            SearchFilter::Videos => Some("EgIQAQ%3D%3D"),
            SearchFilter::Channels => Some("EgIQAg%3D%3D"),
            SearchFilter::Playlists => Some("EgIQAw%3D%3D"),
            SearchFilter::Live => Some("EgJAAQ%3D%3D"),
        }
    }
}

pub struct Client(&'static core::Client);

impl Deref for Client {
    type Target = core::Client;

    fn deref(&self) -> &core::Client {
        self.0
    }
}

pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| Client(core::web()))
}

impl Client {
    // -----------------------------------------------------------------------
    // Feeds
    // -----------------------------------------------------------------------

    pub async fn browse_continuation(&self, token: &str) -> Result<Value> {
        self.post("browse", json!({ "continuation": token })).await
    }

    pub async fn feed(&self, browse_id: &str, params: Option<&str>) -> Result<FeedPage> {
        let v = self.browse(browse_id, params).await?;
        Ok(parse::parse_browse_feed(&v, true))
    }

    pub async fn feed_more(&self, token: &str) -> Result<FeedPage> {
        let v = self.browse_continuation(token).await?;
        Ok(parse::parse_browse_feed(&v, true))
    }

    pub async fn shelves(&self, browse_id: &str, params: Option<&str>) -> Result<FeedPage> {
        let v = self.browse(browse_id, params).await?;
        Ok(parse::parse_shelves(&v))
    }

    pub async fn search(&self, query: &str, filter: SearchFilter) -> Result<FeedPage> {
        let mut body = json!({ "query": query });
        if let Some(p) = filter.params() {
            body["params"] = json!(p);
        }
        let v = self.post("search", body).await?;
        Ok(parse::parse_search(&v))
    }

    pub async fn search_more(&self, token: &str) -> Result<FeedPage> {
        let v = self.post("search", json!({ "continuation": token })).await?;
        Ok(parse::parse_search(&v))
    }

    pub async fn suggestions(&self, query: &str) -> Result<Vec<String>> {
        let url = url::Url::parse_with_params(
            "https://suggestqueries-clients6.youtube.com/complete/search",
            &[("client", "firefox"), ("ds", "yt"), ("hl", "en"), ("q", query)],
        )?;
        let body = http().get(url).send().await?.text().await?;
        // Sometimes wrapped in JSONP or prefixed; parse from the first '['.
        let start = body.find('[').unwrap_or(0);
        let end = body.rfind(']').map(|e| e + 1).unwrap_or(body.len());
        let v: Value = serde_json::from_str(&body[start..end]).map_err(|e| {
            anyhow!("bad suggestions response ({e}): {}", body.chars().take(80).collect::<String>())
        })?;
        Ok(v.get(1)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect())
            .unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // Watch page
    // -----------------------------------------------------------------------

    pub async fn video(&self, id: &str, playlist_id: Option<&str>) -> Result<VideoDetails> {
        let mut body = json!({ "videoId": id, "racyCheckOk": true, "contentCheckOk": true });
        if let Some(p) = playlist_id {
            body["playlistId"] = json!(p);
        }
        let v = self.post("next", body).await?;
        let d = parse::parse_next(&v, id);
        if d.title.is_empty() {
            let reason = parse::find_key(&v, "playabilityStatus")
                .and_then(|p| p.get("reason"))
                .and_then(parse::text)
                .unwrap_or_else(|| "Video unavailable".into());
            bail!(reason);
        }
        Ok(d)
    }

    pub async fn related_more(&self, token: &str) -> Result<FeedPage> {
        let v = self.post("next", json!({ "continuation": token })).await?;
        Ok(parse::parse_next_continuation(&v))
    }

    pub async fn comments(&self, token: &str, video_id: &str) -> Result<CommentsPage> {
        let v = self.post("next", json!({ "continuation": token })).await?;
        Ok(parse::parse_comments(&v, video_id))
    }

    // -----------------------------------------------------------------------
    // Channels & playlists
    // -----------------------------------------------------------------------

    /// Resolve a youtube.com URL (handles, /c/ names, ...) into a browse id.
    pub async fn resolve_url(&self, url: &str) -> Result<String> {
        let v = match self.post("navigation/resolve_url", json!({ "url": url })).await {
            Ok(v) => v,
            // YouTube answers "404 Not Found: Requested entity was not found".
            Err(e) if e.to_string().contains("404") => {
                bail!("There's no channel at {}", url.trim_start_matches("https://www.youtube.com/"))
            }
            Err(e) => return Err(e),
        };
        parse::find_key(&v, "browseEndpoint")
            .and_then(|e| e.get("browseId"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("could not resolve {url}"))
    }

    pub async fn channel(&self, id: &str) -> Result<ChannelPage> {
        let id = if id.starts_with("UC") {
            id.to_owned()
        } else {
            self.resolve_url(&format!("https://www.youtube.com/{id}")).await?
        };
        let v = self.browse(&id, None).await?;
        let mut page = parse::parse_channel(&v, &id);
        // Load the Videos tab right away; the home tab is a mixed bag of shelves.
        if let Some(tab) = page.tabs.iter().find(|t| t.title == "Videos").cloned() {
            if let Ok(content) = self.feed(&id, Some(&tab.params)).await {
                page.content = content;
            }
        } else if page.content.items.is_empty() {
            let shelves = parse::parse_shelves(&v);
            page.content.items = shelves.shelves.into_iter().flat_map(|s| s.items).collect();
        }
        Ok(page)
    }

    pub async fn channel_tab(&self, id: &str, params: &str) -> Result<FeedPage> {
        self.feed(id, Some(params)).await
    }

    pub async fn playlist(&self, id: &str) -> Result<PlaylistPage> {
        let v = self.browse(&format!("VL{id}"), None).await?;
        let page = parse::parse_playlist(&v, id);
        if page.title.is_empty() && page.content.items.is_empty() {
            bail!(page
                .content
                .message
                .unwrap_or_else(|| "This playlist is unavailable".into()));
        }
        Ok(page)
    }

    // -----------------------------------------------------------------------
    // Account
    // -----------------------------------------------------------------------

    pub async fn guide_subscriptions(&self) -> Result<Vec<ChannelItem>> {
        let v = self.post("guide", json!({})).await?;
        Ok(parse::parse_guide_subscriptions(&v))
    }

    pub async fn rate(&self, video_id: &str, status: LikeStatus, previous: LikeStatus) -> Result<()> {
        let endpoint = match status {
            LikeStatus::Like => "like/like",
            LikeStatus::Dislike => "like/dislike",
            LikeStatus::Indifferent => {
                if previous == LikeStatus::Indifferent {
                    return Ok(());
                }
                "like/removelike"
            }
        };
        self.post(endpoint, json!({ "target": { "videoId": video_id } }))
            .await
            .map(drop)
    }

    pub async fn set_subscribed(&self, channel_id: &str, subscribed: bool) -> Result<()> {
        let (endpoint, params) = if subscribed {
            ("subscription/subscribe", "EgIIAhgA")
        } else {
            ("subscription/unsubscribe", "CgIIAhgA")
        };
        self.post(endpoint, json!({ "channelIds": [channel_id], "params": params }))
            .await
            .map(drop)
    }

    pub async fn set_watch_later(&self, video_id: &str, add: bool) -> Result<()> {
        let action = if add {
            json!({ "addedVideoId": video_id, "action": "ACTION_ADD_VIDEO" })
        } else {
            json!({ "removedVideoId": video_id, "action": "ACTION_REMOVE_VIDEO_BY_VIDEO_ID" })
        };
        self.post(
            "browse/edit_playlist",
            json!({ "playlistId": "WL", "actions": [action] }),
        )
        .await
        .map(drop)
    }

    /// Fire a playback-statistics ping (watch history / resume position).
    pub async fn stats_ping(&self, url: &str) -> Result<()> {
        self.ping(url).await
    }
}
