"""SlopTube's long-running yt-dlp worker.

Reads one JSON request per line on stdin and answers with one JSON line on
stdout. Keeping yt-dlp imported (and its caches warm) makes every extraction
after the first noticeably faster than spawning the CLI.
"""
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
                    key = f"{f['itag']}{'-drc' if f.get('isDrc') else ''}"
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


def extract(req):
    opts = {
        "quiet": True,
        "no_warnings": True,
        "skip_download": True,
        "noprogress": True,
        "noplaylist": True,
        "logger": _QuietLogger(),
    }
    if req.get("cookiefile"):
        opts["cookiefile"] = req["cookiefile"]
    _capture.ranges = {}
    _capture.tracking = None
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(req["url"], download=False)
    formats = []
    for f in info.get("formats") or []:
        if not f.get("url"):
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
        "captions": _captions(info.get("subtitles"))
        + _captions(info.get("automatic_captions"), lambda l: l.endswith("-orig")),
        "tracking": getattr(_capture, "tracking", None),
        "ranges": getattr(_capture, "ranges", None) or {},
    }


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
        send({"id": rid, "error": msg})
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
