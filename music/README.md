# SlopTube Music

A lightweight native YouTube Music client for Linux, written in Rust with
GTK 4 and libadwaita. It looks and works like the YouTube Music phone app on a
phone, and like music.youtube.com on a desktop, adapting as the window is
resized. A sibling of [SlopTube](https://github.com/nergzd723/sloptube), sharing its approach: InnerTube
for data, yt-dlp for streams, GStreamer for playback.

> [!WARNING]
> SlopTube Music is vibecoded: it was written largely by an AI coding
> assistant in one session and has had little testing. See [Status](#status).

## Why

The YouTube Music website is a heavy web app. SlopTube Music keeps what matters
and does it natively, with memory use as the first concern:

- GTK and libadwaita instead of a browser; no WebKit;
- audio only: one `playbin3` with video disabled, Opus streams, small buffers;
- every list is virtualised (a `GtkListView` of shelves, songs, the queue);
- artwork is requested from Google's image servers at the size it is shown and
  kept in a small LRU texture cache;
- JSON is parsed into small model types and dropped at once, and glibc is
  asked to give freed memory back;
- the yt-dlp helper (a Python process, 55–80 MB) only runs while needed and
  exits after five idle minutes.

The release build uses about **93 MB** of resident memory idle on Home and
**117 MB** while playing, of which roughly 35 MB and 50 MB are the app's own
memory; the rest is shared libraries. (Measured on aarch64 under GTK's
Broadway backend; a Wayland session adds the GL driver's libraries.)

## Features

- **Home** with the mood chips (Relax, Workout, Focus, Energize, …), Quick
  picks as song columns, and carousels that load more as you scroll.
- **Explore**: new releases, charts, moods & genres, trending.
- **Search** with suggestions (queries and artists/albums/songs), history, and
  filter chips (Songs, Videos, Albums, Artists, playlists, …) with endless
  results.
- **Album, playlist and artist pages**: tinted with the artwork's colour,
  Play / Shuffle / Mix, top songs, albums, singles, videos, similar artists.
- **Playback like YouTube Music**: playing a song starts its radio, which keeps
  extending itself; albums and playlists queue their tracks; autoplay continues
  with a radio when the queue ends.
- **Gapless playback** and **instant skips**: the next track's stream is
  resolved as soon as a track starts and handed to GStreamer before the current
  one ends.
- **Shuffle** (restorable order), **repeat** off/all/one, previous restarts
  the song after 3 seconds.
- **Now playing**: on a phone, the artwork, title, Radio/Album/Share,
  seek bar and transport, with **Up next**, **Lyrics** and **Related** in a
  bottom sheet; on a desktop, the artwork beside those tabs, with a full
  transport bar (volume, repeat, shuffle, like) along the bottom.
- **Mini player** on phones: tap it or drag it up to open Now playing, swipe
  it sideways for next/previous.
- **Local library**: liked songs, saved albums, playlists and artists, and
  listening history, kept on this computer (no account needed).
- **The queue is restored** at startup, paused; nothing is fetched until you
  press play.
- **MPRIS**: media keys and desktop media controls, including shuffle and
  loop status. **Background playback** after closing the window.
- Opens `music.youtube.com`, `youtube.com` and `youtu.be` links.

## Status

Developed and tried only on one machine (aarch64, Arch Linux ARM) and only
through GTK's Broadway backend, with audio sent to a silent sink:

- **Tried**: browsing Home (with continuations), Explore, an album, an artist
  and search in both the desktop and phone layouts; playing from Quick picks
  and from an album; the radio queue; the gapless switch to the next album
  track; prefetching; restoring the queue; the Now playing page with Up next
  and Lyrics; clean exit on SIGTERM. The parsers have tests against real
  responses, and `--selftest` exercises every endpoint and yt-dlp.
- **Not tried yet**: audible playback through a real sound server, a real
  Wayland/X11 session, touch input and the drag gestures (sheet, mini player
  swipes), MPRIS, background playback, the light style, Preferences, long
  playlists, and machines other than this one.

## Requirements

GTK 4.18 or newer, libadwaita 1.7 or newer, GStreamer 1.24 or newer, and
yt-dlp with a JavaScript runtime. On Arch Linux:

```sh
sudo pacman -S --needed gtk4 libadwaita gstreamer gst-plugins-base gst-plugins-good \
    yt-dlp yt-dlp-ejs deno
```

| Package | What for |
|---|---|
| `gtk4`, `libadwaita` | The interface. |
| `gstreamer`, `gst-plugins-base`, `gst-plugins-good` | playbin3, the HTTP source, the WebM demuxer, Opus decoding, audio output. |
| `yt-dlp`, `yt-dlp-ejs`, `deno` | Stream URLs. YouTube makes yt-dlp run JavaScript, so it needs a runtime such as Deno. |
| `gst-plugins-bad` *(optional)* | AAC decoding, for the rare song without an Opus stream. |

Building needs Rust 1.96 or newer and `pkgconf`.

## Building and running

```sh
cargo build --release
./target/release/sloptube-music
```

To install for your user:

```sh
install -Dm755 target/release/sloptube-music ~/.local/bin/sloptube-music
install -Dm644 data/io.github.nergzd.SlopTubeMusic.desktop -t ~/.local/share/applications/
install -Dm644 data/icons/scalable/apps/io.github.nergzd.SlopTubeMusic.svg -t ~/.local/share/icons/hicolor/scalable/apps/
install -Dm644 data/icons/scalable/apps/io.github.nergzd.SlopTubeMusic-symbolic.svg -t ~/.local/share/icons/hicolor/symbolic/apps/
```

Links can be passed on the command line, and go to the running instance:

```sh
sloptube-music https://music.youtube.com/watch?v=4D7u5KF7SP8
sloptube-music "https://music.youtube.com/playlist?list=RDCLAK5uy_n20FRYQXNt1p1wS55Nj2r14IouO5weaYU"
```

## Keyboard shortcuts

| Keys | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>F</kbd>, <kbd>/</kbd> | Search |
| <kbd>Ctrl</kbd>+<kbd>L</kbd> | Open a link |
| <kbd>Ctrl</kbd>+<kbd>J</kbd> | Show or hide Now playing |
| <kbd>Space</kbd>, <kbd>K</kbd> | Play or pause |
| <kbd>Shift</kbd>+<kbd>N</kbd>, <kbd>Ctrl</kbd>+<kbd>→</kbd> | Next song |
| <kbd>Shift</kbd>+<kbd>P</kbd>, <kbd>Ctrl</kbd>+<kbd>←</kbd> | Previous song |
| <kbd>J</kbd> / <kbd>L</kbd> | Back / forward 10 seconds |
| <kbd>+</kbd> / <kbd>−</kbd>, <kbd>M</kbd> | Volume, mute |
| <kbd>S</kbd>, <kbd>R</kbd> | Shuffle, repeat |
| <kbd>Esc</kbd> | Close Now playing, or go back |
| <kbd>Ctrl</kbd>+<kbd>R</kbd>, <kbd>F5</kbd> | Refresh |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Preferences |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |

## Command line and environment

| | Effect |
|---|---|
| `--selftest` | Without a window: check home, continuations, chips, explore, search and its continuation, suggestions, an album, an artist, a playlist, a radio and its continuation, lyrics, related, and two extractions, printing what came back and the memory used. |
| `RUST_LOG` | Log filter, `sloptube_music=info` by default; `sloptube_music=debug` logs every request with its time. |
| `SLOPTUBE_MUSIC_WINDOW_SIZE` | Initial window size, e.g. `390x844` for the phone layout. |
| `SLOPTUBE_MUSIC_HELPER_IDLE` | Seconds before the idle yt-dlp helper exits (300; 0 keeps it running). |
| `SLOPTUBE_MUSIC_YTDLP`, `SLOPTUBE_MUSIC_YTDLP_PYTHON` | The yt-dlp to use, and the Python for its helper. |
| `SLOPTUBE_MUSIC_AUDIO_SINK` | A GStreamer audio sink to use instead of the default, e.g. `fakesink` to test silently. |
| `SLOPTUBE_MUSIC_FIXTURES` | Directory of saved responses for the parser tests (see below). |

## Files

| Path | Contents |
|---|---|
| `~/.config/sloptube-music/settings.json` | Preferences and search history. |
| `~/.local/share/sloptube-music/library.json` | Liked songs, saved albums/playlists/artists, history. |
| `~/.local/share/sloptube-music/queue.json` | The queue, restored at startup. |

## How it works

[docs/PLAN.md](docs/PLAN.md) describes the design and what comes next. In short:

| Path | Responsibility |
|---|---|
| `src/api/` | InnerTube `WEB_REMIX` client, tolerant parsers, model types. |
| `src/extractor.rs`, `src/ytdlp_helper.py` | Audio stream URLs through a resident yt-dlp helper (idle exit, CLI fallback, cache, shared in-flight requests). |
| `src/player.rs` | The GStreamer player: audio-only playbin3, control thread, gapless hand-over. |
| `src/playback.rs` | The queue: radios, autoplay, shuffle/repeat, prefetching, retries, session restore. |
| `src/library.rs` | The local library. |
| `src/image.rs` | Artwork at display size, LRU texture cache, dominant colours. |
| `src/mpris.rs` | MPRIS. |
| `src/ui/rows.rs` | `RowList`, the virtualised list every page is built on. |
| `src/ui/shelf.rs`, `cards.rs`, `art.rs` | Shelves, tiles, rows, artwork and tint widgets. |
| `src/ui/browse.rs`, `collection.rs`, `artist.rs`, `search.rs`, `library.rs` | Pages. |
| `src/ui/now_playing.rs`, `player_bar.rs`, `sheet.rs`, `queue.rs`, `lyrics.rs` | Now playing, the bars and the sheet between them. |
| `src/ui/window.rs` | The adaptive window. |

## Development

```sh
tools/fetch-fixtures.py target/fixtures
SLOPTUBE_MUSIC_FIXTURES=target/fixtures cargo test
cargo run -- --selftest
```

For screenshots without a session, GTK's Broadway backend shows the app in a
browser (`gtk4-broadwayd :7`, then run with `GDK_BACKEND=broadway
BROADWAY_DISPLAY=:7` and open http://localhost:8087).

## Known limitations

- No Google account yet: no personal recommendations, synced library,
  playlists or likes on YouTube. The library is local.
- No downloads, no time-synced lyrics, no video (Song/Video toggle).
- Podcasts open as playlists at best.
- Long playlists queue the part that's loaded when started from a track row.

## License

GNU General Public License, version 3 or later. Not affiliated with YouTube or
Google. Tab and action icons come from Google's Material Symbols (Apache-2.0).
