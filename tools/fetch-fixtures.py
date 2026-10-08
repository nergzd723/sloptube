#!/usr/bin/env python3
"""Download fresh InnerTube (YouTube web API) responses for the parser tests.

The parser tests in src/api/parse.rs look for these files in the directory
named by $SLOPTUBE_FIXTURES and are skipped when it isn't set:

    tools/fetch-fixtures.py target/fixtures
    SLOPTUBE_FIXTURES=target/fixtures cargo test

Re-run it whenever YouTube changes its responses and a parser needs fixing.
Only the Python standard library is used; no account or cookies are sent.
"""
import json
import re
import sys
import urllib.request
from pathlib import Path

UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36"
FALLBACK_VERSION = "2.20261007.01.00"

# Stable public content used by the assertions in the tests.
VIDEO = "aqz-KE-bpKQ"  # Big Buck Bunny 60fps 4K (Blender)
CHANNEL = "UCSMOQeBJ2RAnuFungnQOxLg"  # Blender
CHANNEL_VIDEOS_PARAMS = "EgZ2aWRlb3PyBgQKAjoA"
PLAYLIST = "PLa1F2ddGya_-UvuAqHAksYnB0qL9yWDO6"  # Blender Fundamentals 2.8
SEARCH = "sturgeon cooking"


def client_version() -> str:
    """The web client's current version, as the app itself does it."""
    try:
        req = urllib.request.Request("https://www.youtube.com/sw.js_data", headers={"User-Agent": UA})
        data = urllib.request.urlopen(req, timeout=20).read().decode("utf-8", "replace")
        m = re.search(r'"(2\.20\d{6}\.\d{2}\.\d{2})"', data)
        if m:
            return m.group(1)
    except OSError as e:
        print(f"warning: could not fetch client version ({e}), using {FALLBACK_VERSION}")
    return FALLBACK_VERSION


def call(version: str, endpoint: str, body: dict) -> dict:
    body = dict(body)
    body["context"] = {"client": {"clientName": "WEB", "clientVersion": version, "hl": "en", "gl": "US"}}
    req = urllib.request.Request(
        f"https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false",
        data=json.dumps(body).encode(),
        headers={
            "Content-Type": "application/json",
            "User-Agent": UA,
            "X-Youtube-Client-Name": "1",
            "X-Youtube-Client-Version": version,
            "Origin": "https://www.youtube.com",
        },
    )
    return json.load(urllib.request.urlopen(req, timeout=30))


def find_comments_token(next_response: dict) -> str:
    """Continuation token of the comment section in a /next response."""
    def walk(x):
        if isinstance(x, dict):
            if x.get("sectionIdentifier") == "comment-item-section":
                for c in x.get("contents", []):
                    token = c.get("continuationItemRenderer", {}).get("continuationEndpoint", {}) \
                        .get("continuationCommand", {}).get("token")
                    if token:
                        return token
            for v in x.values():
                found = walk(v)
                if found:
                    return found
        elif isinstance(x, list):
            for v in x:
                found = walk(v)
                if found:
                    return found
        return None

    token = walk(next_response)
    if not token:
        sys.exit("error: no comments token in the /next response (comments disabled?)")
    return token


def main() -> None:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else "target/fixtures")
    out.mkdir(parents=True, exist_ok=True)
    version = client_version()
    print(f"client version {version} -> {out}/")

    fixtures = {
        "search": ("search", {"query": SEARCH}),
        "next": ("next", {"videoId": VIDEO}),
        "channel": ("browse", {"browseId": CHANNEL}),
        "channel_videos": ("browse", {"browseId": CHANNEL, "params": CHANNEL_VIDEOS_PARAMS}),
        "playlist": ("browse", {"browseId": f"VL{PLAYLIST}"}),
    }
    responses = {}
    for name, (endpoint, body) in fixtures.items():
        responses[name] = call(version, endpoint, body)
        (out / f"{name}.json").write_text(json.dumps(responses[name]))
        print(f"  {name}.json")

    token = find_comments_token(responses["next"])
    comments = call(version, "next", {"continuation": token})
    (out / "comments.json").write_text(json.dumps(comments))
    print("  comments.json")


if __name__ == "__main__":
    main()
