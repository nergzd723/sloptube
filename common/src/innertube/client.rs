//! The InnerTube HTTP client core: request context and headers, signing with
//! the shared session, visitor data, the client version, and errors. There is
//! one per client profile (`web()` for SlopTube, `web_remix()` for Music);
//! each app adds its endpoints on top.

use std::sync::{OnceLock, RwLock};

use anyhow::{Context, Result, bail};
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::json;
use super::model::AccountInfo;
use crate::session;

pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

/// Which web client a [`Client`] speaks as.
pub struct Profile {
    /// `clientName` and the `X-Youtube-Client-Name` number.
    pub name: &'static str,
    pub id: &'static str,
    pub host: &'static str,
    pub origin: &'static str,
    pub default_version: &'static str,
    /// How the client's version starts in `sw.js_data` (`"2.20`, `"1.20`).
    pub version_marker: &'static str,
    /// For error messages ("YouTube returned ...").
    pub label: &'static str,
    /// Send the extra fields the YouTube website sends (`osName`, `browserName`, `useSsl`).
    pub full_context: bool,
}

pub const WEB: Profile = Profile {
    name: "WEB",
    id: "1",
    host: "www.youtube.com",
    origin: "https://www.youtube.com",
    default_version: "2.20261007.01.00",
    version_marker: "\"2.20",
    label: "YouTube",
    full_context: true,
};

pub const WEB_REMIX: Profile = Profile {
    name: "WEB_REMIX",
    id: "67",
    host: "music.youtube.com",
    origin: "https://music.youtube.com",
    default_version: "1.20261006.10.00",
    version_marker: "\"1.20",
    label: "YouTube Music",
    full_context: false,
};

struct State {
    visitor_data: Option<String>,
    client_version: String,
    region: Option<String>,
    /// The session generation `visitor_data` belongs to.
    identity: u64,
}

pub struct Client {
    profile: &'static Profile,
    state: RwLock<State>,
}

/// One HTTP client (and connection pool) for the whole process: InnerTube,
/// thumbnails and artwork, pings.
pub fn http() -> reqwest::Client {
    static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client")
    })
    .clone()
}

/// SlopTube's client (`WEB`, www.youtube.com).
pub fn web() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| Client::new(&WEB))
}

/// Music's client (`WEB_REMIX`, music.youtube.com).
pub fn web_remix() -> &'static Client {
    static C: OnceLock<Client> = OnceLock::new();
    C.get_or_init(|| Client::new(&WEB_REMIX))
}

impl Client {
    fn new(profile: &'static Profile) -> Self {
        Self {
            profile,
            state: RwLock::new(State {
                visitor_data: None,
                client_version: profile.default_version.to_owned(),
                region: None,
                identity: session::generation(),
            }),
        }
    }

    pub fn profile(&self) -> &'static Profile {
        self.profile
    }

    pub fn is_signed_in(&self) -> bool {
        session::is_signed_in()
    }

    /// Two-letter region code for content (`None`: YouTube guesses from the IP).
    pub fn set_region(&self, region: Option<String>) {
        self.state.write().unwrap().region = region.filter(|r| !r.is_empty());
    }

    /// Forget the visitor data when the identity changed: a new identity gets
    /// fresh visitor data from YouTube.
    fn follow_identity(&self) {
        let generation = session::generation();
        if self.state.read().unwrap().identity != generation {
            let mut st = self.state.write().unwrap();
            st.identity = generation;
            st.visitor_data = None;
        }
    }

    fn context(&self) -> Value {
        let st = self.state.read().unwrap();
        let mut client = json!({
            "clientName": self.profile.name,
            "clientVersion": st.client_version,
            "hl": "en",
            "userAgent": USER_AGENT,
            "platform": "DESKTOP",
            "utcOffsetMinutes": utc_offset_minutes(),
        });
        if self.profile.full_context {
            client["osName"] = json!("X11");
            client["browserName"] = json!("Chrome");
        }
        if let Some(gl) = &st.region {
            client["gl"] = json!(gl);
        }
        if let Some(vd) = &st.visitor_data {
            client["visitorData"] = json!(vd);
        }
        let mut context = json!({
            "client": client,
            "user": { "lockedSafetyMode": false },
        });
        if self.profile.full_context {
            context["request"] = json!({ "useSsl": true });
        }
        context
    }

    /// Headers for a request to this client's host, signed when signed in.
    pub fn headers(&self) -> HeaderMap {
        let st = self.state.read().unwrap();
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
        h.insert("X-Youtube-Client-Name", HeaderValue::from_static(self.profile.id));
        if let Ok(v) = HeaderValue::from_str(&st.client_version) {
            h.insert("X-Youtube-Client-Version", v);
        }
        h.insert(header::ORIGIN, HeaderValue::from_static(self.profile.origin));
        if let Ok(v) = HeaderValue::from_str(&format!("{}/", self.profile.origin)) {
            h.insert(header::REFERER, v);
        }
        if let Some(vd) = st.visitor_data.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
            h.insert("X-Goog-Visitor-Id", vd);
        }
        session::sign(&mut h, self.profile.host, self.profile.origin);
        h
    }

    pub async fn post(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        self.follow_identity();
        body["context"] = self.context();
        let url = format!("{}/youtubei/v1/{endpoint}?prettyPrint=false", self.profile.origin);
        let started = std::time::Instant::now();
        let resp = http()
            .post(&url)
            .headers(self.headers())
            .json(&body)
            .send()
            .await
            .with_context(|| format!("request to {endpoint} failed"))?;
        session::absorb(resp.headers(), resp.url().host_str().unwrap_or(self.profile.host));
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("reading the {endpoint} response failed"))?;
        let v: Value = serde_json::from_slice(&bytes).with_context(|| format!("invalid response from {endpoint}"))?;
        drop(bytes);
        // Responses are big and dropped right after parsing.
        crate::memory::trim_soon();
        if !status.is_success() {
            let msg = v.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error");
            bail!("{} returned {status}: {msg}", self.profile.label);
        }
        log::debug!("{endpoint}: {status} in {:?}", started.elapsed());
        // Continuations only work with the visitor id that produced them, so keep
        // the first one.
        if let Some(vd) = v.pointer("/responseContext/visitorData").and_then(Value::as_str) {
            let mut st = self.state.write().unwrap();
            if st.visitor_data.is_none() {
                st.visitor_data = Some(vd.to_owned());
            }
        }
        Ok(v)
    }

    pub async fn browse(&self, browse_id: &str, params: Option<&str>) -> Result<Value> {
        let mut body = json!({ "browseId": browse_id });
        if let Some(p) = params {
            body["params"] = json!(p);
        }
        self.post("browse", body).await
    }

    /// Fetch the web client's current version once, so requests look current.
    pub async fn refresh_client_version(&self) -> Result<()> {
        let body = http()
            .get(format!("{}/sw.js_data", self.profile.origin))
            .header(header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .send()
            .await?
            .text()
            .await?;
        if let Some(pos) = body.find(self.profile.version_marker) {
            let rest = &body[pos + 1..];
            if let Some(end) = rest.find('"') {
                let ver = &rest[..end];
                if ver.split('.').count() == 4 {
                    self.state.write().unwrap().client_version = ver.to_owned();
                    log::debug!("{} client version {ver}", self.profile.name);
                }
            }
        }
        Ok(())
    }

    /// Who is signed in; `None` when YouTube considers the session signed out.
    pub async fn account_info(&self) -> Result<Option<AccountInfo>> {
        let v = self.post("account/account_menu", json!({})).await?;
        Ok(json::parse_account_menu(&v))
    }

    /// A signed GET to a playback-tracking URL (watch history, plays).
    pub async fn ping(&self, url: impl reqwest::IntoUrl) -> Result<()> {
        let resp = http().get(url).headers(self.headers()).send().await?;
        session::absorb(resp.headers(), resp.url().host_str().unwrap_or(self.profile.host));
        if !resp.status().is_success() {
            bail!("ping returned {}", resp.status());
        }
        Ok(())
    }
}

fn utc_offset_minutes() -> i32 {
    gtk::glib::DateTime::now_local()
        .map(|d| (d.utc_offset().as_seconds() / 60) as i32)
        .unwrap_or(0)
}
