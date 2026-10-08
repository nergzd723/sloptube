//! InnerTube HTTP client (YouTube WEB client).

use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::model::*;
use super::parse;
use crate::auth::{self, Session};

const BASE: &str = "https://www.youtube.com/youtubei/v1";
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";
const DEFAULT_CLIENT_VERSION: &str = "2.20261007.01.00";

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

struct State {
    visitor_data: Option<String>,
    client_version: String,
    region: Option<String>,
    session: Option<Session>,
    session_dirty: bool,
}

pub struct Client {
    http: reqwest::Client,
    state: RwLock<State>,
}

static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();

pub fn client() -> Arc<Client> {
    CLIENT
        .get_or_init(|| Arc::new(Client::new()))
        .clone()
}

/// A shared HTTP client for thumbnails and other plain downloads.
pub fn http() -> reqwest::Client {
    client().http.clone()
}

impl Client {
    fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
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
        // A fresh identity: let YouTube hand out new visitor data.
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

    /// Returns the session if cookies changed since the last call (for persisting).
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
            "clientName": "WEB",
            "clientVersion": st.client_version,
            "hl": "en",
            "userAgent": USER_AGENT,
            "platform": "DESKTOP",
            "osName": "X11",
            "browserName": "Chrome",
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
            "request": { "useSsl": true },
        })
    }

    fn headers(&self, with_auth: bool) -> HeaderMap {
        let st = self.state.read().unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
        h.insert("X-Youtube-Client-Name", HeaderValue::from_static("1"));
        if let Ok(v) = HeaderValue::from_str(&st.client_version) {
            h.insert("X-Youtube-Client-Version", v);
        }
        h.insert(header::ORIGIN, HeaderValue::from_static(auth::ORIGIN));
        h.insert(header::REFERER, HeaderValue::from_static("https://www.youtube.com/"));
        if let Some(vd) = st.visitor_data.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
            h.insert("X-Goog-Visitor-Id", vd);
        }
        if with_auth {
            if let Some(s) = st.session.as_ref().filter(|s| s.is_signed_in()) {
                if let Ok(v) = HeaderValue::from_str(&s.cookie_header("www.youtube.com")) {
                    h.insert(header::COOKIE, v);
                }
                if let Some(a) = s.authorization().and_then(|a| HeaderValue::from_str(&a).ok()) {
                    h.insert(header::AUTHORIZATION, a);
                }
                h.insert("X-Goog-AuthUser", HeaderValue::from_static("0"));
                h.insert("X-Origin", HeaderValue::from_static(auth::ORIGIN));
                h.insert("X-Youtube-Bootstrap-Logged-In", HeaderValue::from_static("true"));
            }
        }
        h
    }

    fn absorb_cookies(&self, resp: &reqwest::Response) {
        let host = resp.url().host_str().unwrap_or("www.youtube.com").to_owned();
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
            .headers(self.headers(true))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("request to {endpoint} failed"))?;
        self.absorb_cookies(&resp);
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .with_context(|| format!("invalid response from {endpoint}"))?;
        if !status.is_success() {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("YouTube returned {status}: {msg}");
        }
        log::debug!("{endpoint}: {status} in {:?}", started.elapsed());
        if let Some(vd) = v.pointer("/responseContext/visitorData").and_then(Value::as_str) {
            let mut st = self.state.write().unwrap();
            if st.visitor_data.as_deref() != Some(vd) {
                st.visitor_data = Some(vd.to_owned());
            }
        }
        Ok(v)
    }

    /// Fetch the web client's current version once, so requests look current.
    pub async fn refresh_client_version(&self) -> Result<()> {
        let html = self
            .http
            .get("https://www.youtube.com/sw.js_data")
            .header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .send()
            .await?
            .text()
            .await?;
        // sw.js_data contains the client version as the first "2.YYYYMMDD..." string.
        if let Some(pos) = html.find("\"2.20") {
            let rest = &html[pos + 1..];
            if let Some(end) = rest.find('"') {
                let ver = &rest[..end];
                if ver.split('.').count() == 4 {
                    self.state.write().unwrap().client_version = ver.to_owned();
                    log::debug!("InnerTube client version {ver}");
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Feeds
    // -----------------------------------------------------------------------

    pub async fn browse(&self, browse_id: &str, params: Option<&str>) -> Result<Value> {
        let mut body = json!({ "browseId": browse_id });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        self.post("browse", body).await
    }

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
        let body = self.http.get(url).send().await?.text().await?;
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
        let v = self.post("navigation/resolve_url", json!({ "url": url })).await?;
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

    pub async fn account_info(&self) -> Result<Option<AccountInfo>> {
        let v = self.post("account/account_menu", json!({})).await?;
        Ok(parse::parse_account_menu(&v))
    }

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
        let headers = self.headers(true);
        let resp = self.http.get(url).headers(headers).send().await?;
        self.absorb_cookies(&resp);
        if !resp.status().is_success() {
            bail!("stats ping returned {}", resp.status());
        }
        Ok(())
    }
}

fn utc_offset_minutes() -> i32 {
    gtk::glib::DateTime::now_local()
        .map(|d| (d.utc_offset().as_seconds() / 60) as i32)
        .unwrap_or(0)
}
