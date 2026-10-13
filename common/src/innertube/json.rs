//! Low-level helpers for InnerTube JSON. YouTube reshuffles its responses
//! regularly, so the parsers built on these look for what they need wherever
//! it is instead of following long fixed paths.

use serde_json::Value;

use super::model::{AccountInfo, LikeStatus, Thumbnail, thumb_url};

/// Display text from `{simpleText}`, `{runs:[{text}]}`, `{content}`, `{text}` or a
/// plain string. Blank text counts as none.
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

pub fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str)
}

/// A non-empty string at `pointer`.
pub fn string_at(v: &Value, pointer: &str) -> Option<String> {
    str_at(v, pointer).filter(|s| !s.is_empty()).map(str::to_owned)
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

/// Every value stored under `key` (not looking inside matches).
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

/// Protocol-relative URLs (`//yt3.ggpht.com/...`) made absolute.
pub fn fix_url(u: &str) -> String {
    match u.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => u.to_owned(),
    }
}

fn thumbnail_list(list: &[Value]) -> Vec<Thumbnail> {
    list.iter()
        .filter_map(|t| {
            Some(Thumbnail {
                url: fix_url(t.get("url")?.as_str()?),
                width: t.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                height: t.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
            })
        })
        .collect()
}

/// Thumbnails right under `v`: `{thumbnails:[..]}`, `{sources:[..]}`, `{image:{sources}}`
/// or an array.
pub fn thumbnails(v: &Value) -> Vec<Thumbnail> {
    v.get("thumbnails")
        .or_else(|| v.get("sources"))
        .or_else(|| v.pointer("/image/sources"))
        .and_then(Value::as_array)
        .or_else(|| v.as_array())
        .map(|a| thumbnail_list(a))
        .unwrap_or_default()
}

/// The first `thumbnails` list anywhere under `v`.
pub fn find_thumbnails(v: &Value) -> Vec<Thumbnail> {
    find_key(v, "thumbnails")
        .and_then(Value::as_array)
        .map(|a| thumbnail_list(a))
        .unwrap_or_default()
}

/// The token of the first `continuationCommand` under `v`.
pub fn command_token(v: &Value) -> Option<String> {
    find_key(v, "continuationCommand")
        .and_then(|c| c.get("token"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The continuation token of a shelf or section list, in either shape YouTube
/// uses: an old `continuations` list, or a `continuationItemRenderer` last item.
pub fn continuation(container: &Value) -> Option<String> {
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

/// "4:09", "1:02:03" or "100:00" (long mixes) -> seconds.
pub fn parse_duration(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut total: u64 = 0;
    for p in parts {
        if p.is_empty() || p.len() > 3 || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        total = total.checked_mul(60)?.checked_add(p.parse().ok()?)?;
    }
    Some(total)
}

pub fn looks_like_duration(s: &str) -> bool {
    parse_duration(s).is_some()
}

/// A `likeStatus` value.
pub fn like_status(s: &str) -> Option<LikeStatus> {
    match s {
        "LIKE" => Some(LikeStatus::Like),
        "DISLIKE" => Some(LikeStatus::Dislike),
        "INDIFFERENT" => Some(LikeStatus::Indifferent),
        _ => None,
    }
}

/// Who is signed in, from an `account/account_menu` response; `None` when
/// YouTube treated the request as signed out.
pub fn parse_account_menu(v: &Value) -> Option<AccountInfo> {
    let h = find_key(v, "activeAccountHeaderRenderer")?;
    let name = h.get("accountName").and_then(text)?;
    Some(AccountInfo {
        name,
        handle: h.get("channelHandle").and_then(text),
        avatar: h.get("accountPhoto").map(thumbnails).and_then(|t| thumb_url(&t, 96)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_variants() {
        assert_eq!(text(&json!({"simpleText": "a"})).as_deref(), Some("a"));
        assert_eq!(text(&json!({"runs": [{"text": "a"}, {"text": "b"}]})).as_deref(), Some("ab"));
        assert_eq!(text(&json!({"content": "c"})).as_deref(), Some("c"));
        assert_eq!(text(&json!("plain")).as_deref(), Some("plain"));
        assert_eq!(text(&json!({"runs": []})), None);
        assert_eq!(text(&json!({"simpleText": "  "})), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("4:09"), Some(249));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration("100:00"), Some(6000));
        assert_eq!(parse_duration("LIVE"), None);
        assert_eq!(parse_duration("12"), None);
        assert_eq!(parse_duration("1::2"), None);
        assert!(looks_like_duration("13:16") && !looks_like_duration("1:2:3:4"));
    }

    #[test]
    fn thumbnails_and_continuations() {
        let v = json!({"thumbnail": {"thumbnails": [{"url": "//yt3.ggpht.com/a=s88", "width": 88, "height": 88}]}});
        assert_eq!(find_thumbnails(&v)[0].url, "https://yt3.ggpht.com/a=s88");
        assert!(thumbnails(&v).is_empty());
        assert_eq!(thumbnails(&v["thumbnail"]).len(), 1);
        let shelf = json!({"contents": [{}, {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "T"}}}}]});
        assert_eq!(continuation(&shelf).as_deref(), Some("T"));
        assert_eq!(command_token(&shelf).as_deref(), Some("T"));
    }

    #[test]
    fn account_menu() {
        let v = json!({"actions": [{"header": {"activeAccountHeaderRenderer": {
            "accountName": {"simpleText": "Ann"}, "channelHandle": {"simpleText": "@ann"},
            "accountPhoto": {"thumbnails": [{"url": "https://yt3.ggpht.com/x=s88-c", "width": 88, "height": 88}]}}}}]});
        let a = parse_account_menu(&v).unwrap();
        assert_eq!((a.name.as_str(), a.handle.as_deref()), ("Ann", Some("@ann")));
        assert!(a.avatar.unwrap().starts_with("https://yt3.ggpht.com/x=w96-h96"));
        let signed_out = json!({"activeAccountHeaderRenderer": {"accountName": {"simpleText": ""}}});
        assert!(parse_account_menu(&signed_out).is_none());
        assert!(parse_account_menu(&json!({"actions": []})).is_none());
        // The shape the account menu really has.
        let v = json!({"actions": [{"openPopupAction": {"popup": {"multiPageMenuRenderer": {"header": {
            "activeAccountHeaderRenderer": {
                "accountName": {"runs": [{"text": "Ada"}]},
                "channelHandle": {"runs": [{"text": "@ada"}]},
                "accountPhoto": {"thumbnails": [{"url": "https://yt3.ggpht.com/abc=s88-c-k", "width": 88, "height": 88}]}
            }}}}}}]});
        let a = parse_account_menu(&v).unwrap();
        assert_eq!((a.name.as_str(), a.handle.as_deref()), ("Ada", Some("@ada")));
        assert!(a.avatar.unwrap().ends_with("=w96-h96-l90-rj"));
    }
}
