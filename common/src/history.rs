//! Reporting what plays to the account's YouTube history, as YouTube's own
//! players do: a "playback" ping once playing starts, then "watchtime" pings
//! for the stretches played (every 30 s, and when playback pauses or ends).
//! The URLs come with the stream (`extractor::Tracking`).

use std::collections::HashMap;
use std::time::Instant;

use crate::extractor::Tracking;
use crate::innertube::client::Client;

pub struct Tracker {
    client: &'static Client,
    playback_url: Option<String>,
    watchtime_url: Option<String>,
    /// Sent as `c=` (YouTube Music's player names its client).
    client_name: Option<&'static str>,
    /// Client playback nonce: one per play, ties its pings together.
    cpn: String,
    sent_playback: bool,
    segment_start: f64,
    last_ping: Instant,
    duration: f64,
}

impl Tracker {
    /// `client` sends the pings (with the account's cookies); `start` is
    /// where playback starts.
    pub fn new(client: &'static Client, tracking: &Tracking, start: f64) -> Self {
        Self {
            client,
            playback_url: tracking.playback.clone(),
            watchtime_url: tracking.watchtime.clone(),
            client_name: None,
            cpn: cpn(),
            sent_playback: false,
            segment_start: start,
            last_ping: Instant::now(),
            duration: 0.0,
        }
    }

    /// Say which client the pings come from (`c=`), as YouTube Music's player does.
    pub fn naming_client(mut self) -> Self {
        self.client_name = Some(self.client.profile().name);
        self
    }

    /// Playing at `pos`: the first call reports the play, later ones the time
    /// played every 30 s.
    pub fn tick(&mut self, pos: f64, duration: f64) {
        self.duration = duration;
        if !self.sent_playback {
            self.sent_playback = true;
            self.segment_start = pos;
            self.last_ping = Instant::now();
            if let Some(u) = &self.playback_url {
                self.send(stats_url(u, &self.cpn, self.client_name, pos, None, duration));
            }
        } else if self.last_ping.elapsed().as_secs() >= 30 {
            self.watched(pos, duration);
        }
    }

    /// Paused or stopped at `pos`: report the time played since the last ping.
    pub fn flush(&mut self, pos: f64, duration: f64) {
        if duration > 0.0 {
            self.duration = duration;
        }
        if self.sent_playback && pos > self.segment_start {
            self.watched(pos, self.duration);
        }
    }

    /// Played to the end.
    pub fn finish(&mut self) {
        self.flush(self.duration, self.duration);
    }

    fn watched(&mut self, pos: f64, duration: f64) {
        if let Some(u) = &self.watchtime_url {
            self.send(stats_url(u, &self.cpn, self.client_name, pos, Some((self.segment_start, pos)), duration));
        }
        self.segment_start = pos;
        self.last_ping = Instant::now();
    }

    fn send(&self, url: String) {
        let client = self.client;
        crate::runtime::spawn(async move {
            if let Err(e) = client.ping(&url).await {
                log::debug!("watch history ping failed: {e:#}");
            }
        });
    }
}

fn cpn() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
    (0..16)
        .map(|_| ALPHABET[gtk::glib::random_int_range(0, ALPHABET.len() as i32) as usize] as char)
        .collect()
}

fn stats_url(base: &str, cpn: &str, client: Option<&str>, cmt: f64, segment: Option<(f64, f64)>, len: f64) -> String {
    let Ok(mut url) = url::Url::parse(base) else {
        return base.to_owned();
    };
    let mut set: HashMap<&str, String> = HashMap::new();
    set.insert("ver", "2".into());
    set.insert("cpn", cpn.to_owned());
    set.insert("cmt", format!("{cmt:.3}"));
    set.insert("el", "detailpage".into());
    if let Some(c) = client {
        set.insert("c", c.to_owned());
    }
    if len > 0.0 {
        set.insert("len", format!("{len:.3}"));
    }
    if let Some((st, et)) = segment {
        set.insert("st", format!("{st:.3}"));
        set.insert("et", format!("{et:.3}"));
        set.insert("state", "playing".into());
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !set.contains_key(k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut q = url.query_pairs_mut();
        q.clear();
        for (k, v) in &kept {
            q.append_pair(k, v);
        }
        for (k, v) in &set {
            q.append_pair(k, v);
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_url_replaces_our_parameters_and_keeps_the_rest() {
        let u = stats_url(
            "https://s.youtube.com/api/stats/watchtime?ns=yt&cmt=0&docid=abc",
            "CPN",
            Some("WEB_REMIX"),
            12.5,
            Some((2.0, 12.5)),
            200.0,
        );
        let q: HashMap<String, String> = url::Url::parse(&u).unwrap().query_pairs().into_owned().collect();
        assert_eq!(q["ns"], "yt");
        assert_eq!(q["docid"], "abc");
        assert_eq!(q["cmt"], "12.500");
        assert_eq!(q["st"], "2.000");
        assert_eq!(q["et"], "12.500");
        assert_eq!(q["len"], "200.000");
        assert_eq!(q["cpn"], "CPN");
        assert_eq!(q["c"], "WEB_REMIX");
        assert_eq!(cpn().len(), 16);
    }
}
