//! Music's InnerTube endpoints, on the shared `WEB_REMIX` client core
//! (`common::innertube::client`: context, signing, visitor data, errors).

use std::ops::Deref;
use std::sync::OnceLock;

use anyhow::{Result, bail};
use common::innertube::client as core;
pub use common::innertube::client::USER_AGENT;
use serde_json::json;

use super::model::*;
use super::parse;

pub struct Client(&'static core::Client);

impl Deref for Client {
    type Target = core::Client;

    fn deref(&self) -> &core::Client {
        self.0
    }
}

pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| Client(core::web_remix()))
}

impl Client {
    // -----------------------------------------------------------------------
    // Browsing
    // -----------------------------------------------------------------------

    /// A page of shelves: home (with a mood chip's params), explore, moods, charts, ...
    pub async fn browse_page(&self, browse_id: &str, params: Option<&str>) -> Result<BrowsePage> {
        let v = self.browse(browse_id, params).await?;
        Ok(parse::parse_browse(&v))
    }

    pub async fn browse_more(&self, token: &str) -> Result<BrowsePage> {
        let v = self.post("browse", json!({ "continuation": token })).await?;
        Ok(parse::parse_browse_continuation(&v))
    }

    pub async fn collection(&self, browse_id: &str) -> Result<CollectionPage> {
        let v = self.browse(browse_id, None).await?;
        let page = parse::parse_collection(&v, browse_id);
        if page.title.is_empty() && page.tracks.is_empty() {
            bail!("This {} isn't available", if browse_id.starts_with("MPRE") { "album" } else { "playlist" });
        }
        Ok(page)
    }

    pub async fn collection_more(&self, token: &str) -> Result<(Vec<Track>, Option<String>)> {
        let v = self.post("browse", json!({ "continuation": token })).await?;
        Ok(parse::parse_collection_continuation(&v))
    }

    pub async fn artist(&self, browse_id: &str) -> Result<ArtistPage> {
        let v = self.browse(browse_id, None).await?;
        let page = parse::parse_artist(&v, browse_id);
        if page.name.is_empty() && page.shelves.is_empty() {
            bail!("This artist isn't available");
        }
        Ok(page)
    }

    // -----------------------------------------------------------------------
    // Search
    // -----------------------------------------------------------------------

    pub async fn search(&self, query: &str, params: Option<&str>) -> Result<BrowsePage> {
        let mut body = json!({ "query": query });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        let v = self.post("search", body).await?;
        Ok(parse::parse_browse(&v))
    }

    pub async fn search_more(&self, token: &str) -> Result<(Vec<Item>, Option<String>)> {
        let v = self.post("search", json!({ "continuation": token })).await?;
        Ok(parse::parse_shelf_continuation(&v))
    }

    pub async fn suggestions(&self, query: &str) -> Result<Suggestions> {
        let v = self
            .post("music/get_search_suggestions", json!({ "input": query }))
            .await?;
        Ok(parse::parse_suggestions(&v))
    }

    // -----------------------------------------------------------------------
    // Playing
    // -----------------------------------------------------------------------

    /// The queue for a song (its radio when no playlist is given) or a playlist.
    pub async fn queue(&self, target: &WatchTarget) -> Result<WatchQueue> {
        let mut body = json!({
            "isAudioOnly": true,
            "enablePersistentPlaylistPanel": true,
            "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
        });
        if let Some(v) = &target.video_id {
            body["videoId"] = json!(v);
        }
        match (&target.playlist_id, &target.video_id) {
            (Some(p), _) => body["playlistId"] = json!(p),
            (None, Some(v)) => {
                body["playlistId"] = json!(format!("RDAMVM{v}"));
                body["params"] = json!("wAEB");
            }
            (None, None) => bail!("nothing to play"),
        }
        if let Some(p) = &target.params {
            body["params"] = json!(p);
        }
        let v = self.post("next", body).await?;
        Ok(parse::parse_next(&v))
    }

    pub async fn queue_more(&self, token: &str) -> Result<WatchQueue> {
        let v = self
            .post(
                "next",
                json!({ "continuation": token, "isAudioOnly": true, "enablePersistentPlaylistPanel": true }),
            )
            .await?;
        Ok(parse::parse_next_continuation(&v))
    }

    /// The lyrics and related tabs of a track: a small `next` request without a queue.
    pub async fn track_tabs(&self, video_id: &str) -> Result<WatchQueue> {
        let v = self
            .post("next", json!({ "videoId": video_id, "isAudioOnly": true }))
            .await?;
        Ok(parse::parse_next(&v))
    }

    // -----------------------------------------------------------------------
    // Account
    // -----------------------------------------------------------------------

    /// Like, dislike or clear the rating of a song.
    pub async fn rate_song(&self, video_id: &str, rating: LikeStatus) -> Result<()> {
        self.post(rating.endpoint(), json!({ "target": { "videoId": video_id } }))
            .await
            .map(drop)
    }

    /// Save an album or playlist to the library (like) or remove it.
    pub async fn rate_playlist(&self, playlist_id: &str, rating: LikeStatus) -> Result<()> {
        self.post(rating.endpoint(), json!({ "target": { "playlistId": playlist_id } }))
            .await
            .map(drop)
    }

    pub async fn set_subscribed(&self, channel_id: &str, subscribed: bool) -> Result<()> {
        let endpoint = if subscribed {
            "subscription/subscribe"
        } else {
            "subscription/unsubscribe"
        };
        self.post(endpoint, json!({ "channelIds": [channel_id] })).await.map(drop)
    }

    /// Report a play to the account's history (the player response's tracking URL).
    pub async fn report_play(&self, tracking_url: &str) -> Result<()> {
        const CPN_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let cpn: String = (0..16)
            .map(|_| CPN_CHARS[rand::random_range(0..CPN_CHARS.len())] as char)
            .collect();
        let mut url = url::Url::parse(tracking_url)?;
        url.query_pairs_mut()
            .append_pair("ver", "2")
            .append_pair("c", "WEB_REMIX")
            .append_pair("cpn", &cpn);
        self.ping(url).await
    }

    pub async fn lyrics(&self, browse_id: &str) -> Result<Option<Lyrics>> {
        let v = self.browse(browse_id, None).await?;
        Ok(parse::parse_lyrics(&v))
    }
}
