"""SlopTube Music's long-running yt-dlp worker.

Reads one JSON request per line on stdin and answers with one JSON line on
stdout. Keeping yt-dlp imported makes every extraction after the first much
faster than running the CLI. Only audio formats are returned.
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
    "format_id", "url", "manifest_url", "protocol", "ext", "acodec", "abr", "asr",
    "audio_channels", "filesize", "language", "language_preference", "format_note",
    "http_headers", "has_drm",
)


def is_audio(f):
    return f.get("acodec") not in (None, "none") and f.get("vcodec") in (None, "none")


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
    with yt_dlp.YoutubeDL(opts) as ydl:
        info = ydl.extract_info(req["url"], download=False)
    formats = [
        {k: f[k] for k in FORMAT_KEYS if f.get(k) is not None}
        for f in info.get("formats") or []
        if f.get("url") and is_audio(f)
    ]
    return {
        "id": info.get("id"),
        "title": info.get("title"),
        "duration": info.get("duration"),
        "is_live": bool(info.get("is_live")),
        "formats": formats,
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
    pool = ThreadPoolExecutor(max_workers=3)
    send({"id": 0, "ready": yt_dlp.version.__version__})
    for line in sys.stdin:
        if line.strip():
            pool.submit(handle, line)
    pool.shutdown(wait=False, cancel_futures=True)


main()
