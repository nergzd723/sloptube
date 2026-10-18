"""The long-running yt-dlp worker SlopTube and SlopTube Music share.

Reads one JSON request per line on stdin and answers with one JSON line on
stdout. Keeping yt-dlp imported (and its caches warm) makes every extraction
after the first noticeably faster than spawning the CLI. Requests with
"audio_only" (Music's) get only the audio formats back, without captions or
DASH ranges.
"""
import hashlib
import json
import sys
import threading
from concurrent.futures import ThreadPoolExecutor

_out_lock = threading.Lock()


def send(obj):
    data = json.dumps(obj, separators=(",", ":"))
    with _out_lock:
        sys.stdout.write(data + "\n")
        sys.stdout.flush()


try:
    import yt_dlp
except Exception as exc:  # pragma: no cover - reported to the app
    send({"id": 0, "fatal": f"cannot import yt_dlp: {exc}"})
    sys.exit(2)

# Extra data yt-dlp doesn't expose, captured from the raw player responses:
#  * DASH byte ranges (initRange/indexRange) so the app can build a DASH
#    manifest and seek precisely with HTTP range requests;
#  * playback-tracking URLs so the app can report real watch progress
#    (yt-dlp itself can only mark videos as fully watched).
_capture = threading.local()
try:
    from yt_dlp.extractor.youtube import YoutubeIE

    _orig_formats = YoutubeIE._extract_formats_and_subtitles

    def _capturing_formats(self, video_id, player_responses, *args, **kwargs):
        ranges = getattr(_capture, "ranges", None)
        if ranges is not None:
            for pr in player_responses or []:
                if not isinstance(pr, dict):
                    continue
                for f in (pr.get("streamingData") or {}).get("adaptiveFormats") or []:
                    ir, xr = f.get("initRange"), f.get("indexRange")
                    if not (f.get("itag") and ir and xr):
                        continue
                    # One itag can cover many streams (dubbed audio tracks, DRC), and
                    # yt-dlp then renames them "140-0", "140-1", ...; the content
                    # length tells the streams apart, see _ranges_by_format_id.
                    key = (str(f["itag"]), str(f.get("contentLength") or ""))
                    ranges.setdefault(key, {
                        "init": [int(ir["start"]), int(ir["end"])],
                        "index": [int(xr["start"]), int(xr["end"])],
                        "mime": f.get("mimeType"),
                        "bitrate": f.get("bitrate"),
                        "audio_sample_rate": f.get("audioSampleRate"),
                    })
                pt = pr.get("playbackTracking")
                if pt and getattr(_capture, "tracking", None) is None:
                    _capture.tracking = {
                        "playback": (pt.get("videostatsPlaybackUrl") or {}).get("baseUrl"),
                        "watchtime": (pt.get("videostatsWatchtimeUrl") or {}).get("baseUrl"),
                    }
        return _orig_formats(self, video_id, player_responses, *args, **kwargs)

    YoutubeIE._extract_formats_and_subtitles = _capturing_formats
except Exception:  # pragma: no cover
    pass


def _ranges_for(raw_ranges, f):
    """Byte ranges captured for the stream behind a yt-dlp format, if any."""
    itag = str(f.get("format_id") or "").split("-")[0]
    size = f.get("filesize")
    if size is not None:
        r = raw_ranges.get((itag, str(size)))
        if r is not None:
            return r
    # Without a size, only an itag with a single stream is unambiguous.
    matches = [r for (i, _), r in raw_ranges.items() if i == itag]
    return matches[0] if len(matches) == 1 else None


class _QuietLogger:
    def debug(self, msg):
        pass

    def info(self, msg):
        pass

    def warning(self, msg):
        pass

    def error(self, msg):
        pass


FORMAT_KEYS = (
    "format_id", "url", "manifest_url", "protocol", "ext", "vcodec", "acodec",
    "width", "height", "fps", "tbr", "abr", "vbr", "asr", "audio_channels",
    "filesize", "filesize_approx", "dynamic_range", "language",
    "language_preference", "format_note", "http_headers", "has_drm",
)


def _is_audio(f):
    return f.get("acodec") not in (None, "none") and f.get("vcodec") in (None, "none")


def _captions(tracks, keep=None):
    out = []
    for lang, entries in (tracks or {}).items():
        if keep and not keep(lang):
            continue
        for e in entries or []:
            if e.get("ext") == "vtt" and e.get("url"):
                out.append({"lang": lang, "name": e.get("name") or lang, "url": e["url"]})
                break
    return out


# YoutubeDL instances are reused: each keeps the player JS, ytcfg and other
# caches, which saves about a second per extraction (more when signed in). One
# instance serves one request at a time; idle ones wait here, keyed by their
# options and the contents of the cookie file (the app rewrites it before every
# extraction, and yt-dlp only reads it when created).
_pool_lock = threading.Lock()
_pool = {}
_POOL_KEYS = 4


def _cookie_digest(path):
    try:
        with open(path, "rb") as f:
            return hashlib.sha1(f.read()).hexdigest()
    except OSError:
        return None


def _checkout(cookiefile, clients):
    key = (cookiefile, cookiefile and _cookie_digest(cookiefile), clients)
    with _pool_lock:
        idle = _pool.pop(key, [])
        ydl = idle.pop() if idle else None
        _pool[key] = idle  # (re)inserted last: the most recently used key
        while len(_pool) > _POOL_KEYS:
            del _pool[next(iter(_pool))]
    if ydl is None:
        opts = {
            "quiet": True,
            "no_warnings": True,
            "skip_download": True,
            "noprogress": True,
            "noplaylist": True,
            "logger": _QuietLogger(),
        }
        if cookiefile:
            opts["cookiefile"] = cookiefile
        if clients:
            opts["extractor_args"] = {"youtube": {"player_client": list(clients)}}
        ydl = yt_dlp.YoutubeDL(opts)
    return key, ydl


def _checkin(key, ydl):
    with _pool_lock:
        if key in _pool:
            _pool[key].append(ydl)


def _extract_info(url, cookiefile, clients):
    key, ydl = _checkout(cookiefile, clients)
    _capture.ranges = {}
    _capture.tracking = None
    info = ydl.extract_info(url, download=False)
    _checkin(key, ydl)
    return info


# Signed out, yt-dlp asks its "web" client too, but YouTube only offers SABR
# streams there (which yt-dlp skips), and solving its JavaScript challenge
# costs most of a second per video. Video requests skip it, falling back to
# yt-dlp's defaults when that finds nothing playable.
_SIGNED_OUT_CLIENTS = ("default", "-web")


def _playable(info):
    return any(f.get("url") for f in info.get("formats") or [])


def extract(req):
    url, cookiefile = req["url"], req.get("cookiefile") or None
    audio_only = bool(req.get("audio_only"))
    info = None
    if not cookiefile and not audio_only:
        try:
            info = _extract_info(url, None, _SIGNED_OUT_CLIENTS)
        except yt_dlp.utils.DownloadError as exc:
            if _is_network_error(exc):
                raise
        if info is not None and not _playable(info):
            info = None
    if info is None:
        info = _extract_info(url, cookiefile, None)
    raw_ranges = {} if audio_only else getattr(_capture, "ranges", None) or {}
    ranges = {}
    formats = []
    for f in info.get("formats") or []:
        r = _ranges_for(raw_ranges, f)
        if r is not None and f.get("format_id"):
            ranges[f["format_id"]] = r
        if not f.get("url") or (audio_only and not _is_audio(f)):
            continue
        formats.append({k: f[k] for k in FORMAT_KEYS if f.get(k) is not None})
    return {
        "id": info.get("id"),
        "title": info.get("title"),
        "duration": info.get("duration"),
        "is_live": bool(info.get("is_live")),
        "live_status": info.get("live_status"),
        "formats": formats,
        "chapters": [
            {"start": c.get("start_time"), "end": c.get("end_time"), "title": c.get("title")}
            for c in info.get("chapters") or []
        ],
        "captions": [] if audio_only else _captions(info.get("subtitles"))
        + _captions(info.get("automatic_captions"), lambda l: l.endswith("-orig")),
        "tracking": getattr(_capture, "tracking", None),
        "ranges": ranges,
    }


def _is_network_error(exc):
    """Whether a DownloadError comes from not reaching the server at all."""
    try:
        from yt_dlp.networking.exceptions import TransportError
    except ImportError:  # pragma: no cover - older yt-dlp
        return False
    e = (getattr(exc, "exc_info", None) or (None, None, None))[1]
    seen = set()
    while e is not None and id(e) not in seen:
        if isinstance(e, TransportError):
            return True
        seen.add(id(e))
        e = getattr(e, "cause", None) or e.__cause__ or e.__context__
    return False


def handle(line):
    try:
        req = json.loads(line)
    except ValueError:
        return
    rid = req.get("id", 0)
    try:
        if req.get("op") == "extract":
            send({"id": rid, "ok": extract(req)})
        elif req.get("op") == "version":
            send({"id": rid, "ok": {"version": yt_dlp.version.__version__}})
        else:
            send({"id": rid, "error": f"unknown op {req.get('op')!r}"})
    except yt_dlp.utils.DownloadError as exc:
        msg = str(exc)
        if msg.startswith("ERROR: "):
            msg = msg[7:]
        send({"id": rid, "error": msg, "network": _is_network_error(exc)})
    except Exception as exc:  # noqa: BLE001
        send({"id": rid, "error": f"{type(exc).__name__}: {exc}"})


def main():
    pool = ThreadPoolExecutor(max_workers=4)
    send({"id": 0, "ready": yt_dlp.version.__version__})
    for line in sys.stdin:
        if line.strip():
            pool.submit(handle, line)
    pool.shutdown(wait=False, cancel_futures=True)


main()
