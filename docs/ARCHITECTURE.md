# SlopTube architecture

This document is for people changing SlopTube. It explains how the pieces fit
together and, more importantly, why several of them work the way they do: many
decisions here are workarounds for specific behaviour of YouTube, GStreamer or
the hardware, and undoing them brings the original bug back. Each section names
the files involved.

## Overview

```
                 GTK main thread
  ┌───────────────────────────────────────────────┐
  │  ui::*  (window, pages, watch sheet, player)  │
  │  Playback controller, Account, settings,      │
  │  image cache, GStreamer bus watch             │
  └──────┬─────────────────────┬──────────────────┘
         │ runtime::run()      │ ctl() / ctl_await()
         ▼                     ▼
  ┌──────────────────┐   ┌───────────────────────┐   ┌─────────────────────────┐
  │ tokio runtime    │   │ "sloptube-gst"        │   │ GStreamer streaming     │
  │ "sloptube-io"    │   │ control thread:       │──►│ threads: playbin3 →     │
  │ InnerTube HTTPS, │   │ state changes, seeks  │   │ decoders →              │
  │ images, keyring, │   └───────────────────────┘   │ gtk4paintablesink       │
  │ yt-dlp helper I/O│                               └─────────────────────────┘
  └───────┬──────────┘
          │ JSON lines on stdin/stdout
          ▼
  ┌──────────────────┐   ┌──────────────────────────────────┐
  │ python: yt-dlp   │   │ yt-dlp CLI, one process per      │
  │ helper (resident)│   │ download (gio::Subprocess)       │
  └──────────────────┘   └──────────────────────────────────┘
```

A video goes through these steps:

1. The UI calls `Playback::open` with a `VideoItem` (from a card or a link).
2. In parallel, `api::Client::video` fetches the watch page data (InnerTube
   `next`: title, description, channel, related videos, comment token) and
   `extractor` asks the yt-dlp helper for the stream URLs.
3. `player::formats::select` picks the video and audio streams for this
   machine's decoders.
4. MP4 streams are wrapped in a generated DASH manifest (`player::dash`); WebM
   streams are passed as two URLs.
5. `Player::load` hands them to `playbin3` on the control thread. Frames are
   decoded, in hardware where possible, and reach the window as DMABufs through
   `gtk4paintablesink`.

## Source map

| Path | Responsibility |
|---|---|
| `src/main.rs` | Environment fixes, GStreamer init, static sink registration, `--selftest` and `--diagnose` flags. |
| `src/application.rs` | `AdwApplication`: startup (CSS, icons, settings, decoder ranks, stored session), actions and accelerators, opening links, About dialog, shortcuts dialog. |
| `src/config.rs` | App ID and well-known directories. |
| `src/settings.rs` | Preferences as JSON, main-thread only. |
| `src/runtime.rs` | The tokio runtime and the bridge from GLib futures. |
| `src/api/client.rs` | InnerTube HTTP client: request context, headers, authentication, cookies, endpoints. |
| `src/api/parse.rs` | Tolerant parsers turning InnerTube JSON into `api::model` types, and their tests. |
| `src/api/model.rs` | Plain data types: `VideoItem`, `Item`, `FeedPage`, `VideoDetails`, `RichText`, `Comment`, ... |
| `src/auth/mod.rs` | Cookie jar, `SAPISIDHASH`, `Set-Cookie` handling, keyring storage, the cookie file for yt-dlp. |
| `src/auth/chromium.rs` | Finding Chromium-family profiles and decrypting their cookies. |
| `src/account.rs` | The signed-in state as a GObject the UI binds to. |
| `src/extractor.rs`, `src/ytdlp_helper.py` | Stream extraction through a resident yt-dlp process, with a CLI fallback and a cache. |
| `src/player/decoders.rs` | Decoder discovery and hardware/software ranking. |
| `src/player/formats.rs` | Stream selection and the quality menu. |
| `src/player/dash.rs` | DASH manifests for MP4 streams. |
| `src/player/mod.rs` | The GStreamer player: pipeline, control thread, loading, seeking, stats. |
| `src/playback.rs` | What is playing: metadata, stream selection, queue and autoplay, retries, resume positions, watch history. |
| `src/image.rs` | Image download, decoding at display size, and the texture cache. |
| `src/downloads.rs` | Downloads through the yt-dlp CLI and their index. |
| `src/mpris.rs` | MPRIS service. |
| `src/diagnose.rs`, `src/selftest.rs` | `--diagnose` and `--selftest`. |
| `src/ui/window.rs` | Main window: adaptive layout, tabs, sidebar, the watch sheet and mini player, player shortcuts. |
| `src/ui/mod.rs` | `Nav` and `navigate`, rich-text markup and `sloptube:` links, URL routing, small helpers. |
| `src/ui/pages.rs` | Home, Subscriptions and Library tabs, shared page chrome, the account button. |
| `src/ui/explore.rs`, `search.rs`, `channel.rs`, `playlist.rs`, `downloads.rs` | The other pages. |
| `src/ui/feed.rs`, `card.rs` | The paginated, virtualised feed and its cards. |
| `src/ui/watch.rs`, `player_view.rs`, `comments.rs` | The watch page, the video surface with its controls, comments. |
| `src/ui/sheet.rs`, `mini_player.rs` | The draggable watch sheet and the mini player bar. |
| `src/ui/aspect.rs` | `AspectBin`, a height-for-width container for 16:9 content. |
| `src/ui/login.rs`, `preferences.rs` | Sign-in dialog and Preferences. |
| `data/` | Stylesheet, icons, `.desktop` file, compiled into a GResource by `build.rs`. |
| `tools/fetch-fixtures.py` | Downloads fresh InnerTube responses for the parser tests. |

## Threads and processes

**GTK main thread.** All widgets, the `Playback` controller, `Account`,
settings (a `thread_local`) and the image cache live here. Nothing on this
thread may block on the network or on GStreamer.

**tokio runtime** (`runtime.rs`, two worker threads named `sloptube-io`). HTTP
(reqwest), Secret Service access (oo7) and the yt-dlp helper's pipes. UI code
starts a GLib future with `glib::spawn_future_local`, runs the network part with
`runtime::run(async { ... })`, awaits the tokio `JoinHandle`, and touches widgets
again once it is back on the main thread. `runtime::spawn` is for fire-and-forget
work.

**GStreamer control thread** (`player/mod.rs`, `sloptube-gst`). Every pipeline
state change and flushing seek runs here, queued in order through `ctl()` or
`ctl_await()`. `gtk4paintablesink` sometimes needs the main thread itself, for
example to query DMABuf formats while a decoder negotiates. If the main thread
were blocked in `set_state` waiting for that decoder, both would wait forever.
That deadlock happened in practice, so keep blocking GStreamer calls off the
main thread.

**GStreamer streaming threads** belong to GStreamer. Bus messages reach the
main thread through a bus watch.

**yt-dlp helper** (`extractor.rs`, `ytdlp_helper.py`): one resident Python
process, see [Streams](#streams-yt-dlp).

**Downloads** (`downloads.rs`) run the `yt-dlp` CLI, one `gio::Subprocess`
each, and parse its progress lines on the main loop.

**MPRIS** (`mpris.rs`, the `mpris-server` crate) runs on the main loop as
`org.mpris.MediaPlayer2.io.github.nergzd.SlopTube`.

## Data: InnerTube

InnerTube is the JSON API behind the YouTube website. `api/client.rs` talks to
it as the desktop web client (`clientName: WEB`):

- **Requests** are `POST https://www.youtube.com/youtubei/v1/<endpoint>` with a
  `context` naming the client version, `hl=en`, and `gl` when a region is set.
  The client version is refreshed at startup from
  `https://www.youtube.com/sw.js_data`, so requests look current.
- **Headers**: `X-Youtube-Client-Name`/`-Version`, `Origin` and `Referer`, a
  Chrome user agent, and `X-Goog-Visitor-Id` once a response has supplied
  `visitorData`. Signed in, also `Cookie`, `Authorization` (see
  [Accounts](#accounts)), `X-Goog-AuthUser: 0`, `X-Origin` and
  `X-Youtube-Bootstrap-Logged-In`.
- **Cookies**: `Set-Cookie` headers in responses update the session and mark it
  dirty; `Account` writes dirty sessions back to the keyring every 60 seconds,
  so a session refreshed by Google stays valid.

| Endpoint | Used for |
|---|---|
| `browse` | Feeds (`FEwhat_to_watch`, `FEsubscriptions`, `FEhistory`, `FEplaylist_aggregation`, `FEchannels`), channels and their tabs, playlists (`VL…`), Explore destinations, continuations. |
| `search` | Search, with filter `params`. |
| `next` | Watch page data, related videos, comments and their continuations. |
| `guide` | Subscriptions in the sidebar. |
| `account/account_menu` | Name, handle and avatar. |
| `like/like`, `like/dislike`, `like/removelike` | Rating. |
| `subscription/subscribe`, `subscription/unsubscribe` | Subscribing. |
| `browse/edit_playlist` | Watch later. |
| `navigation/resolve_url` | Turning `/@handle` links into channel IDs. |

Search suggestions come from `suggestqueries-clients6.youtube.com`
(`client=firefox`, a plain JSON array). Watch-history pings go to the
playback-tracking URLs described under [Playback controller](#playback-controller).

Things YouTube changed that shaped the code:

- Signed out, `FEwhat_to_watch` comes back empty, so the Home tab shows a
  sign-in prompt instead of a feed.
- `FEtrending` returns HTTP 400. Explore therefore shows YouTube's destination
  channels (Music, Live, Gaming, News, Sports, Learning, Fashion & Beauty, IDs in
  `ui/explore.rs`) as shelves. They are loaded when the tab is first shown.
- Most lists use `lockupViewModel`. Search results still use `videoRenderer`.
- Comment text and authors are not in the comment items; they arrive as
  entities in `frameworkUpdates.entityBatchUpdate.mutations`
  (`commentEntityPayload`), which the items reference by key.

### Parsers

YouTube reshuffles its JSON often, so `api/parse.rs` avoids long fixed paths.
The feed parsers run a `Collector` over the whole response: it walks every
object, turns each renderer it recognises (`videoRenderer` and its variants,
`lockupViewModel`, channel, playlist and mix renderers, shelves, continuation
items) into an `Item`, and doesn't descend into keys in `SKIP_KEYS`: Shorts
shelves and items, ad renderers, and noise such as `frameworkUpdates` and
engagement panels. Shorts are also caught by their endpoints
(`reelWatchEndpoint`, `/shorts/` URLs) and by shelf icon or title. A response
whose layout changes therefore tends to lose a field rather than the whole page.

Text with links (descriptions, comments) comes as attributed strings whose
`commandRuns` give offsets in UTF-16 code units. `parse_attributed` converts
them to byte offsets for Rust strings and maps each link to a `LinkTarget`
(seek, video, channel, search or URL); `add_timestamp_links` adds links for
bare timestamps. The UI renders them as Pango markup with `sloptube:` URIs,
which `ui::mod` routes back into the app.

The parser tests in `parse.rs` run against saved real responses, see
[Testing](#testing-and-debugging).

## Streams: yt-dlp

Resolving YouTube stream URLs needs signature deciphering, JavaScript
challenges and frequent fixes, so SlopTube leaves it to yt-dlp.

Importing yt-dlp takes a noticeable time, so instead of running the CLI per
video, `extractor.rs` keeps one Python process with yt-dlp imported:

- It finds `yt-dlp` (`SLOPTUBE_YTDLP` or `PATH`), takes the interpreter from the
  script's shebang (or `SLOPTUBE_YTDLP_PYTHON`) and, when the file is the
  official zipapp release, puts it on `PYTHONPATH`.
- It runs `python -u -c <ytdlp_helper.py>`; the script is compiled into the
  binary with `include_str!`. The helper answers with
  `{"id": 0, "ready": "<yt-dlp version>"}` (or `"fatal"`) within 20 seconds.
- **Protocol**: one JSON object per line in each direction. Requests are
  `{"id": N, "op": "extract", "url": "...", "cookiefile": "..."}` (or
  `"op": "version"`); answers are `{"id": N, "ok": {...}}` or
  `{"id": N, "error": "..."}`. The helper handles up to four requests at once
  and the app matches answers to requests by `id`.
- The answer is a trimmed info dict: `id`, `title`, `duration`, `is_live`,
  `live_status`, `formats` (selected keys only), `chapters`, `captions`,
  `tracking` and `ranges`.

`tracking` and `ranges` are not something yt-dlp provides. The helper wraps
`YoutubeIE._extract_formats_and_subtitles` to read them from the raw player
responses: the `initRange`/`indexRange` of every adaptive format (needed for
DASH, below) and the `playbackTracking` URLs (needed for watch history). One
itag can cover many streams (dubbed audio tracks, DRC), which yt-dlp renames
`140-0`, `140-1`, ...; the helper captures ranges by itag and content length
and hands them back keyed by yt-dlp's final `format_id`. This
is the most fragile part of SlopTube: if a yt-dlp update renames that method,
extraction still works but those two fields come back empty, which means plain
HTTP streams for MP4 and no history.

If the helper cannot be started, extraction falls back to `yt-dlp -J` per
video, without `ranges` or `tracking`. Results are cached for two hours per
video. When signed in (and *Use account for video streams* is on), the session
is written as a Netscape cookie file to `$XDG_RUNTIME_DIR/sloptube/cookies.txt`
and passed as `cookiefile`.

## Playback

### Decoders (`player/decoders.rs`)

`probe()` lists GStreamer's video decoders and records, for AV1, VP9 and H.264,
whether any decoder exists and whether a hardware one does. A decoder counts as
hardware when its element class contains `Hardware`, which covers VA-API,
V4L2 stateful and stateless, and others. Alpha decode bins such as
`vp9alphadecodebin` are ignored; they would otherwise count as VP9 decoders.
It also checks for Opus and AAC decoders.

`set_hardware_decoding(true)` raises every hardware decoder to at least
`PRIMARY + 1`, so it beats software decoders such as `avdec_*` (`PRIMARY`);
`false` sets them to `NONE`. The original ranks are remembered.

### Stream selection (`player/formats.rs`)

`codec_order` lists the decodable codecs by preference: the codec chosen in
Preferences, then, with hardware decoding on, the hardware-decodable ones
(AV1, VP9, H.264), then the rest by software decoding cost (H.264, VP9, AV1).

`select` filters the video streams to decodable codecs at or below the target
height (the maximum quality, or the quality picked in the menu) and takes the
best by this key, compared in order:

1. exactly the height picked in the quality menu, if any;
2. hardware-decodable codec;
3. height;
4. frame rate allowed by the 60 fps setting;
5. position in `codec_order`;
6. frame rate;
7. bitrate.

The audio stream is chosen by, in order: the original (or default) track and
yt-dlp's language preference; a container matching the video (AAC in MP4 when
the video is MP4 with DASH ranges, otherwise Opus in WebM); no dynamic range
compression; Opus over AAC; bitrate.

Other cases: live videos play their HLS manifest, audio-only mode plays the
best audio stream alone, and when no separate streams work, an old muxed stream
(itag 18) or any manifest is the fallback. `quality_options` builds the quality
menu, one entry per height.

### Two ways to play YouTube's streams

YouTube serves each quality as one file with a byte index: `sidx` for MP4,
`Cues` for WebM. GStreamer can't seek both kinds the same way:

- **MP4** (AV1 or H.264 with AAC). `qtdemux` can't seek YouTube's fragmented MP4
  over HTTP. `dash.rs` therefore writes a small on-demand MPD with a
  `SegmentBase` (`indexRange` and `Initialization` from the captured `ranges`)
  for the chosen video and audio to `$XDG_RUNTIME_DIR/sloptube/`, and playbin
  plays it through `dashdemux2`, which fetches exactly the fragments it needs
  with range requests. Seeks are fast and buffering stays bounded. Without
  ranges, the streams are played as plain HTTP instead, and seeking may not work.
- **WebM** (VP9 with Opus). `dashdemux2` can't seek WebM `SegmentBase` streams,
  but `matroskademux` seeks fine over HTTP through the `Cues`. The video URL goes
  to playbin's `uri` and the audio URL to its `suburi`.

### The player (`player/mod.rs`)

The pipeline is one `playbin3`:

- `video-sink` is `gtk4paintablesink`. With the default `static-sink` feature
  it is compiled in (`gst-plugin-gtk4`) and registered in `main.rs`. It accepts
  DMABufs, so hardware-decoded frames go to GTK without CPU copies.
- `flags` are only `video+audio+soft-volume`. Subtitle overlay, software
  deinterlacing and colour balance are off because they would pull frames out
  of DMABuf memory.
- `audio-filter` is `scaletempo`, so speed changes keep the pitch.
- `buffer-duration` 8 s and `buffer-size` 16 MB. For `dashdemux2`/`hlsdemux2`,
  `element-setup` sets `max-buffering-time` 30 s, `high-watermark-time` 2.5 s
  and `low-watermark-time` 0.8 s: quick start, bounded memory.
- `source-setup` puts yt-dlp's HTTP headers on every HTTP source.
- Volume is a perceptual 0–1 value applied as its cube.

**Loading.** `load()` increments a generation counter and, on the control
thread, sets the pipeline to `NULL`, sets `uri`/`suburi`, volume and mute, goes
to `READY`, posts an application message `sloptube-load` carrying the
generation, and goes to `PAUSED`. The bus handler ignores every message until it
sees the marker of the current generation, so errors and state changes left
over from the previous video can't affect the new one. The start position is
applied once the pipeline has prerolled. `NoPreroll` means a live stream.

**Seeking.** Seeks are flushing seeks on the control thread. Qualcomm's Iris
V4L2 decoder (and maybe other V4L2 stateful decoders) stops producing VP9 and
AV1 frames after a flush. For decoders named `v4l2*` but not `v4l2sl*`,
`seek_with_decoder_reset` therefore closes and reopens the decoder around the
seek. The order of the steps is documented in the function and matters: blocking
the decoder's input, flushing the sink before closing the decoder (its output
thread may be waiting in a prerolled sink), seeking through both the demuxer pad
and the pipeline with one seqnum, then reopening. `SLOPTUBE_NO_DECODER_RESET`
turns this off.

**Shutdown.** On quit, `shutdown()` sets the pipeline to `NULL` on the control
thread while the main loop keeps running, for up to 1.5 seconds, because the
sink may need the main thread to finish.

**Stats.** `stats()` reports the decoder element, whether it is hardware,
whether frames are DMABufs, the frame size, format and rate, and rendered and
dropped frames. *Stats for nerds* and `--diagnose` show it.

### Playback controller

`Playback` (`playback.rs`) is what the UI talks to. It owns the `Player` and the current
`VideoItem`, `VideoDetails`, `StreamInfo` and `Selection`, and emits signals
when they change.

- **Opening** fetches details and streams in parallel (see the
  [Overview](#overview)). Downloaded files play through `open_file`, with
  details fetched when online.
- **Quality and audio-only** changes reselect and reload at the current
  position.
- **Queue**: a playlist from the watch page data, or with autoplay the first
  related video. Shift+N/P and MPRIS Next/Previous move through it.
- **Errors**: YouTube URLs expire and CDN nodes sometimes misbehave, for
  example by ignoring range requests. Errors whose text suggests that (403, 410,
  `Could not read`, timeouts, ...) trigger a fresh extraction and reload at the
  same position, at most twice; the count resets once playback runs.
- **Resume positions** go to `~/.local/share/sloptube/positions.json` every
  10 seconds, for videos longer than 2 minutes, between 15 seconds in and
  20 seconds before the end; the 400 most recent are kept.
- **Watch history**, when signed in and enabled: a ping to the `playback` URL
  when playback starts, then a ping to the `watchtime` URL every 30 seconds and
  at the end, as the website does. Each carries a random client playback nonce
  (`cpn`) and the position (`cmt`); watch-time pings also carry the segment
  watched since the last one (`st`/`et`).

## User interface

### Window layout (`ui/window.rs`)

```
AdwApplicationWindow
└── AdwToastOverlay
    └── GtkOverlay
        ├── AdwOverlaySplitView
        │   ├── sidebar: tabs, then subscriptions when signed in
        │   └── content: GtkBox (vertical)
        │       ├── AdwViewStack: one AdwNavigationView per tab
        │       │   (Home, Explore, Subscriptions, Library)
        │       ├── GtkRevealer: MiniPlayer
        │       └── AdwViewSwitcherBar (bottom tabs)
        └── overlay: PlayerSheet
            └── WatchView
```

Breakpoints:

| Condition | Effect |
|---|---|
| `max-width: 860sp` | The sidebar collapses, the bottom tab bar appears, the window gets the `narrow` CSS class. |
| `max-width: 520sp` | `phone` CSS class: one column, edge-to-edge thumbnails. |
| `min-width: 1000sp` (watch view) | Two columns: Up next moves beside the details. |

Pages are pushed onto the current tab's `AdwNavigationView` by
`ui::navigate(widget, Nav::...)`, which also handles videos (they open the
sheet), and is what `sloptube:` links and command-line URLs end up calling.
Selecting the current tab again in the sidebar pops back to its root.

Pages that are off screen (the other tabs, and pages under the current one) have
their content hidden until they are mapped again (`hide_content_while_unmapped`
in `window.rs`). On every press and release GTK sets `:active` on each widget
from the pressed one up to the window, which restyles every widget whose style
depends on an ancestor's state. Style updates skip hidden widgets but not pages
a stack merely stops drawing, so without this each touch restyled every feed
the user had opened, 15–25 ms per press and per release.

### Feeds (`ui/feed.rs`, `ui/card.rs`)

`FeedView` is a `GtkGridView`, or a `GtkListView` for compact lists, over a
`gio::ListStore` of `BoxedAnyObject(Item)`, so only cards near the visible
area exist (`GtkGridView` keeps up to `max-columns` × 30 of them). It is
given a `Loader`, a closure that returns a page for an optional continuation
token, and loads the next page when the view is scrolled near the end. That
check only runs when the view is mapped and has a real size: without that guard
a feed whose content didn't fill the view kept loading pages forever. A chips
row, an empty state and an error state with a retry button are built in.

Cards come in grid and compact styles and have a menu: Save to Watch later,
Play as audio, Download, Go to channel, Copy link.

Cards and their channel avatars act on release, so a touch swipe that starts on
one still scrolls. A press that lands while a list is still gliding after a
fling only stops it, as on phones: `track_scrolling` notes whether each press
on a scrolled window landed while it was moving, and the click handlers check
`press_stopped_scrolling` (both in `ui/mod.rs`) before opening anything.

### Watch page and player

`WatchView` (`watch.rs`) holds the player at the top, inside an `AspectBin`
frame, and below it an `AdwNavigationView` with the details. Comments and
replies are pushed into that navigation view. The frame's maximum height is a
fraction of the window (60 % narrow, 72 % wide, all of it in fullscreen), and
the frame may shrink below 16:9 (letterboxing) so the details stay reachable.

`PlayerView` (`player_view.rs`) shows the sink's paintable in a `GtkPicture`
inside a `GtkGraphicsOffload`, so the compositor can scan DMABuf frames out
directly. On top are the auto-hiding controls and a settings popover with
quality, speed, audio only and *Stats for nerds*. A tap or click is acted on
only when it is released, so the start of a drag never toggles the controls.

### The watch sheet (`ui/sheet.rs`)

`PlayerSheet` makes the watch page draggable into the mini player, like the
Android app. Its `progress` property runs from 0 (mini player) to 1 (full
page).

- The `WatchView` always keeps its full size and is only translated down by
  `(1 − progress) × travel`, where `travel` is the mini player thumbnail's
  position. A drag therefore never relayouts the details, which keeps it smooth.
- During motion the real player frame is hidden and a floating `GtkPicture`
  showing the same paintable is interpolated from the player's rectangle to the
  mini player thumbnail's. The details fade with
  `smoothstep((progress − 0.12) / 0.6)`, and the mini bar fades in below 0.35.
- `attach_collapse_drag` (on the video) follows vertical drags once they pass
  10 px, measured from that point so the sheet doesn't jump, and gives up on
  horizontal ones; pulling up more than 80 px at full size asks for fullscreen.
  The video moves with the sheet, so the pointer is tracked in surface (window)
  coordinates: `GestureDrag`'s widget-relative offsets shrink as the sheet
  moves, which made it bounce between two positions.
  `attach_expand_drag` (on the mini bar) follows upward drags, opens on a tap,
  and dismisses on a horizontal swipe past 35 % of the width. A drag released
  less than 64 px up falls back to the bar whatever its speed: thumbs flick
  the feed upward from near the bar, and even a short flick beats the fling
  speed below.
- On release, a fling faster than 1.4 progress units per second decides the
  direction; otherwise the sheet opens when progress is above 0.66 (when the drag
  started open) or 0.25 (when it started closed). `animate_to` runs an
  `AdwSpringAnimation` (damping ratio 0.92, mass 1, stiffness 520) starting
  from the finger's velocity.
- The sheet is locked in fullscreen.

### Sizing pitfalls (`ui/aspect.rs`, `ui/mini_player.rs`)

`AspectBin` reports its height from its width (`HeightForWidth`). It ignores
its child's natural size, which for a video is the stream's pixel size. When
its width is fixed with `set_size_request`, it uses that width instead of the
`for_size` it is asked about: boxes measure children at widths they will never
get, and the mini player's 96 px thumbnail was once measured at ~540 px wide and
made the bar about 300 px tall instead of 68 px.

### Images (`image.rs`)

Images are downloaded on the tokio runtime and decoded with a gdk-pixbuf
`PixbufLoader` at the size they are shown, rounded up to a bucket (48, 96, 176,
256, 360, 480, 720 or 1280 px wide, otherwise 1920). JPEG decoding can scale
while decoding, so a thumbnail costs a few hundred KB instead of several MB.
Textures are cached up to 40 MB, evicting the least recently used. Together with
loading Explore only when it is shown, this brought idle memory from about
640 MB to about 270 MB.

### Styling

`data/style.css` and the icons are compiled into a GResource by `build.rs`.
libadwaita loads no stylesheet at all when `GTK_THEME` is set, and some setups
set `GTK_THEME=Adwaita:dark` globally, so `main.rs` removes `GTK_THEME=Adwaita`
and `Adwaita:*` from the process environment and keeps only the dark
preference.

## Accounts

`auth::Session` is a list of cookies, where they came from, and the account's
name, handle and avatar. It counts as signed in when it has a `SAPISID` (or
`__Secure-3PAPISID`) cookie and one of the `SID` cookies. Requests are
authorised with

```
Authorization: SAPISIDHASH <ts>_<sha1("<ts> <SAPISID> https://www.youtube.com")>
```

followed by `SAPISID1PHASH` and `SAPISID3PHASH` values computed the same way
from the `__Secure-1PAPISID` and `__Secure-3PAPISID` cookies when present.

**Storage.** `auth::store` saves the session as JSON in the Secret Service
(through oo7) with the attributes `application=io.github.nergzd.SlopTube` and
`type=youtube-session`. Without a Secret Service it uses
`~/.local/share/sloptube/session.json` with mode 0600.

**`Account`** (`account.rs`) is a GObject singleton with `signed-in`, `busy`,
`name`, `handle` and `avatar` properties. At startup it loads the stored
session, checks it with `account/account_menu`, and from then on saves cookies
that Google refreshed every 60 seconds.

**In-app sign-in** (`ui/login.rs`, feature `webkit`) loads Google's sign-in page
for YouTube in a WebKitGTK `WebView` with an ephemeral `NetworkSession`. When a
page on `youtube.com` finishes loading, it reads all cookies from the session's
cookie manager and, if they make a signed-in session, takes them.

**Browser import** (`auth/chromium.rs`) looks for `Default` and `Profile N`
directories of Chromium, Google Chrome, Brave, Vivaldi and Microsoft Edge. It
copies the `Cookies` database (the browser may have it locked) and decrypts the
values: `v10` with a key derived from the password `peanuts`, `v11` with the
browser's "Safe Storage" password from the Secret Service; the key is
PBKDF2-SHA1 with salt `saltysalt` and one iteration, the cipher AES-128-CBC with
an IV of 16 spaces. Databases with schema version 24 or later prefix each value
with a 32-byte hash that is stripped.

## Other parts

**Downloads** (`downloads.rs`) run `yt-dlp` with `--progress-template` and
`--print after_move:...` so the app can read progress and the final path from
stdout, a `-f` selector from the download quality, the output template
`%(title).150B [%(id)s].%(ext)s` in the download folder, MKV as the merge format
for video, and the cookie file when signed in. The list and thumbnails are kept
in `~/.local/share/sloptube/`.

**MPRIS** (`mpris.rs`) exposes play, pause, stop, next, previous, seeking,
volume, playback rate, *Quit*, and metadata (title, channel, artwork, length).
*Raise* brings the window back after background playback.

**Background playback** (`window.rs`, `close_request`): closing the window while
something plays, with the setting on, hides the window and sends a
notification instead of quitting. MPRIS Raise or launching SlopTube again shows
it.

## Testing and debugging

### Unit and parser tests

`cargo test` runs the unit tests (stream selection, DASH manifests, cookie and
date parsing, rich text, ...). The parser tests need real responses and are
skipped unless `SLOPTUBE_FIXTURES` names a directory with them:

```sh
tools/fetch-fixtures.py target/fixtures
SLOPTUBE_FIXTURES=target/fixtures cargo test
```

The script needs only Python's standard library and sends no cookies. It saves
`search.json`, `next.json`, `comments.json`, `channel.json`,
`channel_videos.json` and `playlist.json` for stable public content (Blender's
channel and Big Buck Bunny). When YouTube changes something, re-fetch, see which
assertions fail, and fix the parser.

### `--selftest`

Runs the network layer without a window: lists the decoders found, then
exercises search and its continuation, the watch page, comments, a channel, a
playlist, Explore, suggestions, two extractions (cold and warm) and stream
selection, printing what it got.

### `--diagnose`

`sloptube --diagnose[=VIDEO_ID]` starts a separate instance (it never talks to
a running one), plays the video muted with settings kept in memory, and prints a
line per second:

```
diagnose t= 5s state=Playing pos=32.3s decoder=v4l2av1dec hw=true dmabuf=true frames=1920x1080 NV12 60.00fps rendered=143 dropped=0 rss=345MB(anon 93 file 251) error=""
```

It seeks to 1:00 after 7 seconds, saves a frame rendered by GTK's renderer and
GDK's own download of it as `diagnose-rendered.png` and
`diagnose-download.png` in `$XDG_RUNTIME_DIR/sloptube/`, and quits after 14
seconds. Comparing the two PNGs tells colour conversion problems in the
renderer from problems in the stream. `SLOPTUBE_DIAGNOSE_SECS` changes the
duration, `SLOPTUBE_DIAGNOSE_IDLE` measures without playing (a memory baseline),
and `SLOPTUBE_DIAGNOSE_SHEET` drives the watch sheet through several positions
and an open/close animation instead of seeking, printing the window's layout
heights.

### Logs

`RUST_LOG=sloptube=debug` adds every InnerTube request with its status and
time, decoder rank changes, the time from a seek to its first frame, and the
sheet's drag events. `GST_DEBUG` controls GStreamer's own logging.

### Trying layouts and taking screenshots

`SLOPTUBE_WINDOW_SIZE=390x844` starts with a phone-sized window. For scripted
screenshots, GTK's Broadway backend shows the app in a browser:

```sh
gtk4-broadwayd :5 &
GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 sloptube
# open http://localhost:8085
```

Because SlopTube is single-instance, running `sloptube <link>` again with the
same environment navigates the running window. Broadway doesn't show video
colours correctly; check playback on a real Wayland or X11 session.
`.claude/launch.json` holds the same Broadway setup for Claude Code's preview.

### Memory

The `rss=` field of `--diagnose` splits resident memory into anonymous memory
(the app's heap and buffers) and file-backed memory (mostly shared libraries).
`heaptrack sloptube` shows where allocations come from.
