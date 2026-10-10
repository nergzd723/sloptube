//! InnerTube HTTP client (YouTube Music web client, `WEB_REMIX`).

use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{Context, Result, bail};
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::model::*;
use super::parse;
use crate::auth::{self, Session};

const BASE: &str = "https://music.youtube.com/youtubei/v1";
const ORIGIN: &str = auth::ORIGIN;
pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";
const DEFAULT_CLIENT_VERSION: &str = "1.20261006.10.00";

struct State {
    visitor_data: Option<String>,
    client_version: String,
    region: Option<String>,
    session: Option<Session>,
    /// Google refreshed some cookies since the session was last saved.
    session_dirty: bool,
}

pub struct Client {
    http: reqwest::Client,
    state: RwLock<State>,
}

static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();

pub fn client() -> Arc<Client> {
    CLIENT.get_or_init(|| Arc::new(Client::new())).clone()
}

/// A shared HTTP client for artwork and other plain downloads.
pub fn http() -> reqwest::Client {
    client().http.clone()
}

impl Client {
    fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .pool_max_idle_per_host(4)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        Self {
            http,
            state: RwLock::new(State {
                visitor_data: None,
                client_version: DEFAULT_CLIENT_VERSION.to_owned(),
                region: None,
                session: None,
                session_dirty: false,
            }),
        }
    }

    pub fn set_session(&self, session: Option<Session>) {
        let mut st = self.state.write().unwrap();
        st.session = session;
        st.session_dirty = false;
        // A different identity: let YouTube hand out fresh visitor data.
        st.visitor_data = None;
    }

    pub fn session(&self) -> Option<Session> {
        self.state.read().unwrap().session.clone()
    }

    pub fn is_signed_in(&self) -> bool {
        self.state
            .read()
            .unwrap()
            .session
            .as_ref()
            .is_some_and(Session::is_signed_in)
    }

    /// The session, if its cookies changed since the last call (for saving).
    pub fn take_dirty_session(&self) -> Option<Session> {
        let mut st = self.state.write().unwrap();
        if st.session_dirty {
            st.session_dirty = false;
            st.session.clone()
        } else {
            None
        }
    }

    pub fn set_region(&self, region: Option<String>) {
        self.state.write().unwrap().region = region.filter(|r| !r.is_empty());
    }

    fn context(&self) -> Value {
        let st = self.state.read().unwrap();
        let mut client = json!({
            "clientName": "WEB_REMIX",
            "clientVersion": st.client_version,
            "hl": "en",
            "userAgent": USER_AGENT,
            "platform": "DESKTOP",
            "utcOffsetMinutes": utc_offset_minutes(),
        });
        if let Some(gl) = &st.region {
            client["gl"] = json!(gl);
        }
        if let Some(vd) = &st.visitor_data {
            client["visitorData"] = json!(vd);
        }
        json!({
            "client": client,
            "user": { "lockedSafetyMode": false },
        })
    }

    fn headers(&self) -> HeaderMap {
        let st = self.state.read().unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
        h.insert("X-Youtube-Client-Name", HeaderValue::from_static("67"));
        if let Ok(v) = HeaderValue::from_str(&st.client_version) {
            h.insert("X-Youtube-Client-Version", v);
        }
        h.insert(header::ORIGIN, HeaderValue::from_static(ORIGIN));
        h.insert(header::REFERER, HeaderValue::from_static("https://music.youtube.com/"));
        // Continuation tokens only work together with the visitor id that produced them.
        if let Some(vd) = st.visitor_data.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
            h.insert("X-Goog-Visitor-Id", vd);
        }
        if let Some(s) = st.session.as_ref().filter(|s| s.is_signed_in()) {
            if let Ok(v) = HeaderValue::from_str(&s.cookie_header(auth::HOST)) {
                h.insert(header::COOKIE, v);
            }
            if let Some(a) = s.authorization().and_then(|a| HeaderValue::from_str(&a).ok()) {
                h.insert(header::AUTHORIZATION, a);
            }
            h.insert("X-Goog-AuthUser", HeaderValue::from_static("0"));
            h.insert("X-Origin", HeaderValue::from_static(ORIGIN));
            h.insert("X-Youtube-Bootstrap-Logged-In", HeaderValue::from_static("true"));
        }
        h
    }

    /// Keep cookies Google refreshes through `Set-Cookie`.
    fn absorb_cookies(&self, resp: &reqwest::Response) {
        let host = resp.url().host_str().unwrap_or(auth::HOST).to_owned();
        let set: Vec<String> = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect();
        if set.is_empty() {
            return;
        }
        let mut st = self.state.write().unwrap();
        if let Some(session) = st.session.as_mut() {
            let mut changed = false;
            for c in &set {
                changed |= session.apply_set_cookie(c, &host);
            }
            st.session_dirty |= changed;
        }
    }

    pub async fn post(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        body["context"] = self.context();
        let url = format!("{BASE}/{endpoint}?prettyPrint=false");
        let started = std::time::Instant::now();
        let resp = self
            .http
            .post(&url)
            .headers(self.headers())
            .json(&body)
            .send()
            .await
            .with_context(|| format!("request to {endpoint} failed"))?;
        self.absorb_cookies(&resp);
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("reading the {endpoint} response failed"))?;
        let v: Value = serde_json::from_slice(&bytes).with_context(|| format!("invalid response from {endpoint}"))?;
        drop(bytes);
        // The parsed document is dropped by the caller right after parsing.
        crate::memory::trim_soon();
        if !status.is_success() {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("YouTube Music returned {status}: {msg}");
        }
        log::debug!("{endpoint}: {status} in {:?}", started.elapsed());
        if let Some(vd) = v.pointer("/responseContext/visitorData").and_then(Value::as_str) {
            let mut st = self.state.write().unwrap();
            if st.visitor_data.is_none() {
                st.visitor_data = Some(vd.to_owned());
            }
        }
        Ok(v)
    }

    /// Fetch the web client's current version, so requests look current.
    pub async fn refresh_client_version(&self) -> Result<()> {
        let body = self
            .http
            .get("https://music.youtube.com/sw.js_data")
            .header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .send()
            .await?
            .text()
            .await?;
        // The first "1.YYYYMMDD.xx.xx" string is the WEB_REMIX version.
        if let Some(pos) = body.find("\"1.20") {
            let rest = &body[pos + 1..];
            if let Some(end) = rest.find('"') {
                let ver = &rest[..end];
                if ver.split('.').count() == 4 {
                    self.state.write().unwrap().client_version = ver.to_owned();
                    log::debug!("WEB_REMIX client version {ver}");
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Browsing
    // -----------------------------------------------------------------------

    async fn browse(&self, browse_id: &str, params: Option<&str>) -> Result<Value> {
        let mut body = json!({ "browseId": browse_id });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        self.post("browse", body).await
    }

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

    /// Who is signed in; `None` when YouTube considers the session signed out.
    pub async fn account_info(&self) -> Result<Option<AccountInfo>> {
        let v = self.post("account/account_menu", json!({})).await?;
        Ok(parse::parse_account_menu(&v))
    }

    /// Like, dislike or clear the rating of a song.
    pub async fn rate_song(&self, video_id: &str, rating: Rating) -> Result<()> {
        self.post(rate_endpoint(rating), json!({ "target": { "videoId": video_id } }))
            .await
            .map(drop)
    }

    /// Save an album or playlist to the library (like) or remove it.
    pub async fn rate_playlist(&self, playlist_id: &str, rating: Rating) -> Result<()> {
        self.post(rate_endpoint(rating), json!({ "target": { "playlistId": playlist_id } }))
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
        let resp = self.http.get(url).headers(self.headers()).send().await?;
        self.absorb_cookies(&resp);
        if !resp.status().is_success() {
            bail!("history ping returned {}", resp.status());
        }
        Ok(())
    }

    pub async fn lyrics(&self, browse_id: &str) -> Result<Option<Lyrics>> {
        let v = self.browse(browse_id, None).await?;
        Ok(parse::parse_lyrics(&v))
    }
}

fn rate_endpoint(rating: Rating) -> &'static str {
    match rating {
        Rating::Like => "like/like",
        Rating::Dislike => "like/dislike",
        Rating::Indifferent => "like/removelike",
    }
}

fn utc_offset_minutes() -> i32 {
    gtk::glib::DateTime::now_local()
        .map(|d| (d.utc_offset().as_seconds() / 60) as i32)
        .unwrap_or(0)
}
