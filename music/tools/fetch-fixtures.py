#!/usr/bin/env python3
"""Save real YouTube Music (InnerTube WEB_REMIX) responses for the parser tests.

Usage: tools/fetch-fixtures.py DIR

Only Python's standard library is needed and no cookies are sent. The parser
tests in src/api/parse.rs read these files when SLOPTUBE_MUSIC_FIXTURES=DIR.
"""
import json
import os
import re
import sys
import urllib.request

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
      "Chrome/146.0.0.0 Safari/537.36")
CLIENT_VERSION = "1.20261006.01.00"
VISITOR = None

# Stable public content: Daft Punk, "Get Lucky", Random Access Memories.
ARTIST = "UC_kRDKYrUlrbtrSiyu5Tflg"
VIDEO = "5NV6Rdv1a3I"
ALBUM = "MPREb_K8qWMWVqXGi"
PLAYLIST = "VLRDCLAK5uy_n20FRYQXNt1p1wS55Nj2r14IouO5weaYU"


def post(endpoint, body):
    global VISITOR
    client = {"clientName": "WEB_REMIX", "clientVersion": CLIENT_VERSION, "hl": "en"}
    headers = {
        "Content-Type": "application/json",
        "User-Agent": UA,
        "Origin": "https://music.youtube.com",
        "Referer": "https://music.youtube.com/",
        "X-Youtube-Client-Name": "67",
        "X-Youtube-Client-Version": CLIENT_VERSION,
    }
    if VISITOR:
        client["visitorData"] = VISITOR
        headers["X-Goog-Visitor-Id"] = VISITOR
    body = dict(body, context={"client": client})
    req = urllib.request.Request(
        f"https://music.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false",
        data=json.dumps(body).encode(), headers=headers)
    with urllib.request.urlopen(req, timeout=30) as r:
        v = json.load(r)
    # Continuations only work with the visitor id that produced them.
    VISITOR = VISITOR or v.get("responseContext", {}).get("visitorData")
    return v


def first(pattern, value):
    m = re.search(pattern, json.dumps(value, separators=(",", ":")))
    return m.group(1) if m else None


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)

    def save(name, value):
        with open(os.path.join(out, name + ".json"), "w") as f:
            json.dump(value, f, indent=1, ensure_ascii=False)
        print(f"{name}.json")

    home = post("browse", {"browseId": "FEmusic_home"})
    save("home", home)
    token = first(r'"nextContinuationData":\{"continuation":"([^"]+)"', home)
    if token:
        save("home_continuation", post("browse", {"continuation": token}))
    save("explore", post("browse", {"browseId": "FEmusic_explore"}))
    save("moods", post("browse", {"browseId": "FEmusic_moods_and_genres"}))
    save("charts", post("browse", {"browseId": "FEmusic_charts"}))
    save("new_releases", post("browse", {"browseId": "FEmusic_new_releases_albums"}))
    search = post("search", {"query": "daft punk"})
    save("search", search)
    save("search_songs", post("search", {"query": "daft punk", "params": "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D"}))
    save("artist", post("browse", {"browseId": ARTIST}))
    save("album", post("browse", {"browseId": ALBUM}))
    save("playlist", post("browse", {"browseId": PLAYLIST}))
    nxt = post("next", {
        "videoId": VIDEO, "playlistId": "RDAMVM" + VIDEO, "params": "wAEB",
        "isAudioOnly": True, "enablePersistentPlaylistPanel": True,
        "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
    })
    save("next", nxt)
    radio = first(r'"nextRadioContinuationData":\{"continuation":"([^"]+)"', nxt)
    if radio:
        save("next_continuation", post("next", {
            "continuation": radio, "isAudioOnly": True, "enablePersistentPlaylistPanel": True}))
    lyrics = first(r'"browseId":"(MPLYt[^"]+)"', nxt)
    if lyrics:
        save("lyrics", post("browse", {"browseId": lyrics}))
    related = first(r'"browseId":"(MPTRt[^"]+)"', nxt)
    if related:
        save("related", post("browse", {"browseId": related}))
    mood = first(r'"browseId":"FEmusic_moods_and_genres_category","params":"([^"]+)"', post(
        "browse", {"browseId": "FEmusic_moods_and_genres"}))
    if mood:
        save("mood", post("browse", {"browseId": "FEmusic_moods_and_genres_category", "params": mood}))


if __name__ == "__main__":
    main()
