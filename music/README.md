# SlopTube Music

A lightweight native YouTube Music client for Linux, written in Rust with
GTK 4 and libadwaita. It looks and works like the YouTube Music phone app on a
phone, and like music.youtube.com on a desktop, adapting as the window is
resized. It is part of [SlopTube](../README.md) and runs in SlopTube's process
(`sloptube --music`, or **Open SlopTube Music** in SlopTube's account menu),
sharing its approach: InnerTube for data, yt-dlp for streams, GStreamer for
playback.

> [!WARNING]
> SlopTube Music is vibecoded: it was written largely by an AI coding
> assistant in one session and has had little testing. See [Status](#status).

## Why

The YouTube Music website is a heavy web app. SlopTube Music keeps what matters
and does it natively, with memory use as the first concern:

- GTK and libadwaita instead of a browser (the Google sign-in window is a
  separate, short-lived process);
- audio only: one `playbin3` with video disabled, Opus streams, small buffers;
- every list is virtualised (a `GtkListView` of shelves, songs, the queue);
- artwork is requested from Google's image servers at the size it is shown and
  kept in a small LRU texture cache;
- JSON is parsed into small model types and dropped at once, and glibc is
  asked to give freed memory back;
- the yt-dlp helper (a Python process, 55–80 MB, shared with SlopTube) only runs while needed and
  exits after five idle minutes.

As a separate program, the release build used about **93 MB** of resident
memory idle on Home and **117 MB** while playing, of which roughly 35 MB and
50 MB were the app's own memory; the rest was shared libraries. (Measured on
aarch64 under GTK's Broadway backend, before it moved into SlopTube's process,
where those libraries are shared with SlopTube.)

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
- **Google account** (optional): personalised Home, your YouTube Music
  library (playlists, liked music, songs, albums, artists, subscriptions,
  history), likes and dislikes, saving albums and playlists, subscribing to
  artists, Premium audio quality, and plays added to your YouTube history. See
  [Accounts and privacy](#accounts-and-privacy).
- **Local library**: liked songs, saved albums, playlists and artists, and
  listening history, kept on this computer (no account needed).
- **The queue is restored** at startup, paused; nothing is fetched until you
  press play.
- **MPRIS**: media keys and desktop media controls, including shuffle and
  loop status. **Background playback** after closing the window.
- Opens `music.youtube.com` links (from anywhere), and `youtube.com` and
  `youtu.be` links given with `sloptube --music`.
- **Runs alongside SlopTube**: both windows can be open at once, and when one
  starts playing the other pauses.

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
- **Accounts**: the sign-in dialog, the Firefox profile discovery and the
  sign-in window up to Google's page were tried. Everything that needs a
  signed-in session (account info, the library, likes, saving, subscribing,
  history, Premium streams) has not been tried yet; `--selftest` checks the
  account and library sections once a session is stored.

## Requirements

SlopTube's [requirements](../README.md#requirements) cover Music: GTK 4.18 or
newer, libadwaita 1.7 or newer, GStreamer 1.24 or newer, and yt-dlp with a
JavaScript runtime. On Arch Linux:

```sh
sudo pacman -S --needed gtk4 libadwaita gstreamer gst-plugins-base gst-plugins-good \
    yt-dlp yt-dlp-ejs deno webkitgtk-6.0 sqlite openssl
```

| Package | What for |
|---|---|
| `gtk4`, `libadwaita` | The interface. |
| `gstreamer`, `gst-plugins-base`, `gst-plugins-good` | playbin3, the HTTP source, the WebM demuxer, Opus decoding, audio output. |
| `yt-dlp`, `yt-dlp-ejs`, `deno` | Stream URLs. YouTube makes yt-dlp run JavaScript, so it needs a runtime such as Deno. |
| `webkitgtk-6.0` | Google's sign-in page in the sign-in dialog. Only with the `webkit` build feature (on by default). |
| `sqlite`, `openssl` | Importing cookies from browser profiles, and the keyring client. |
| `gnome-keyring` *(recommended)* | Or another Secret Service provider, to store the session. Without one it goes to a private file. |
| `gst-plugins-bad` *(optional)* | AAC decoding, for the rare song without an Opus stream. |

## Building and running

Music is built with SlopTube, from the repository root:

```sh
cargo build --release
./target/release/sloptube --music
```

[SlopTube's README](../README.md#building-and-installing) shows how to install
it, including Music's desktop file and icons. Built without the `webkit`
feature, signing in works only by importing a browser session.

Links can be passed on the command line, and go to the running instance:

```sh
sloptube https://music.youtube.com/watch?v=4D7u5KF7SP8
sloptube --music "https://music.youtube.com/playlist?list=RDCLAK5uy_n20FRYQXNt1p1wS55Nj2r14IouO5weaYU"
```

## Accounts and privacy

Without an account everything works except personal recommendations and the
YouTube library; likes, saved albums and history are kept on this computer.

Music shares SlopTube's sign-in: signing in or out in either app does so for
both. Use the avatar button in the header or Preferences; the sign-in dialog
and where the session is stored are described in [SlopTube's
README](../README.md#accounts-and-privacy). Cookies Google refreshes are saved
back every minute. Music's requests are signed with `SAPISIDHASH` for
`music.youtube.com`, and cookies only go to the domains they belong to.

Signed in, likes, dislikes, saving albums and playlists and subscribing to
artists go to your account (and are mirrored in the local library). With *Use
the account for audio* (on by default), yt-dlp gets the session through
`$XDG_RUNTIME_DIR/sloptube/cookies-music.txt`, a private tmpfs file, for
Premium quality and age-restricted songs. With *Add plays to YouTube history*
(on by default), each song that starts playing is reported the way the website
does it. Signing out removes the session, the cookie file and nothing else.

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
| `sloptube --music --selftest` | Without a window: check home, continuations, chips, explore, search and its continuation, suggestions, an album, an artist, a playlist, a radio and its continuation, lyrics, related, and two extractions, printing what came back and the memory used; with a stored session, also the account, the signed-in Home and each library section (names and counts only). |
| `RUST_LOG` | Log filter, `sloptube=info,sloptube_music=info` by default; `sloptube_music=debug` logs every request with its time. |
| `SLOPTUBE_MUSIC_WINDOW_SIZE` | Initial window size, e.g. `390x844` for the phone layout. |
| `SLOPTUBE_HELPER_IDLE` | Seconds before the idle yt-dlp helper (shared with SlopTube) exits (300; 0 keeps it running). |
| `SLOPTUBE_YTDLP`, `SLOPTUBE_YTDLP_PYTHON` | The yt-dlp to use, and the Python for its helper. |
| `SLOPTUBE_MUSIC_AUDIO_SINK` | A GStreamer audio sink to use instead of the default, e.g. `fakesink` to test silently. |
| `SLOPTUBE_MUSIC_FIXTURES` | Directory of saved responses for the parser tests (see below). |

## Files

| Path | Contents |
|---|---|
| `~/.config/sloptube-music/settings.json` | Preferences and search history. |
| `~/.local/share/sloptube-music/library.json` | Liked songs, saved albums/playlists/artists, history. |
| `~/.local/share/sloptube-music/queue.json` | The queue, restored at startup. |
| `$XDG_RUNTIME_DIR/sloptube/cookies-music.txt` | The session for yt-dlp, while signed in (the session itself is SlopTube's, see its README). |

## How it works

[docs/PLAN.md](docs/PLAN.md) describes the design and what comes next. In short:

| Path | Responsibility |
|---|---|
| `src/api/` | Music's InnerTube endpoints on the shared `WEB_REMIX` client core, tolerant parsers, model types. |
| `src/account.rs` | Handing the shared session (`common::account`) to yt-dlp. |
| `src/application.rs` | What SlopTube calls: setting up on first use, the window, links, the dialogs, and the playback hooks. |
| `src/extractor.rs` | Music's requests to the shared yt-dlp extractor (`common::extractor`), and picking the audio stream. |
| `src/player.rs` | The GStreamer player: audio-only playbin3, control thread, gapless hand-over. |
| `src/playback.rs` | The queue: radios, autoplay, shuffle/repeat, prefetching, retries, session restore. |
| `src/library.rs` | The local library. |
| `src/image.rs` | Dominant colours, on the shared image cache (`common::image`). |
| `src/mpris.rs` | MPRIS. |
| `src/ui/rows.rs` | `RowList`, the virtualised list every page is built on. |
| `src/ui/shelf.rs`, `cards.rs`, `art.rs` | Shelves, tiles, rows, artwork and tint widgets. |
| `src/ui/browse.rs`, `collection.rs`, `artist.rs`, `search.rs`, `library.rs` | Pages. |
| `src/ui/login.rs` | The shared sign-in dialog with Music's icon, and the account button. |
| `src/ui/now_playing.rs`, `player_bar.rs`, `queue.rs`, `lyrics.rs` | Now playing and the bars (the sheet between them is `common::sheet`, shared with SlopTube). |
| `src/ui/window.rs` | The adaptive window. |

## Development

From the repository root:

```sh
music/tools/fetch-fixtures.py target/music-fixtures
SLOPTUBE_MUSIC_FIXTURES=target/music-fixtures cargo test -p sloptube-music
cargo run -- --music --selftest
```

For screenshots without a session, GTK's Broadway backend shows the app in a
browser (`gtk4-broadwayd :7`, then run `sloptube --music` with
`GDK_BACKEND=broadway BROADWAY_DISPLAY=:7` and open http://localhost:8087).

## Known limitations

- Playlists can't be created or edited yet, and songs can't be added to them.
- No downloads, no time-synced lyrics, no video (Song/Video toggle).
- Podcasts open as playlists at best.
- Long playlists queue the part that's loaded when started from a track row.

## License

GNU General Public License, version 3 or later. Not affiliated with YouTube or
Google. Tab and action icons come from Google's Material Symbols (Apache-2.0).
