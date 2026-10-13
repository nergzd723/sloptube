# SlopTube Music — plan

A native YouTube Music client for Linux in Rust, GTK 4 and libadwaita. Goals,
in order: low memory use, snappiness, and an interface that feels like the
YouTube Music phone app on a phone and like music.youtube.com on a desktop.

It started from copies of the parts of [SlopTube](../../README.md) that already
worked (the tokio bridge, the resident yt-dlp helper, the image cache, MPRIS,
the draggable sheet), and now runs inside SlopTube's process as the `music/`
crate (`sloptube --music`), with its own audio-only pipeline, queue and
account. Those copies are being moved into the shared `common` crate; the
runtime, settings store, link parsing, touch helpers and the player sheet
already are.

## Data

- **InnerTube `WEB_REMIX`** (`music.youtube.com/youtubei/v1`, client name 67).
  The client version is refreshed from `music.youtube.com/sw.js_data` at
  startup. Continuations only work with the `visitorData` that produced them,
  so the client keeps it and sends it back (`X-Goog-Visitor-Id`).
- Endpoints: `browse` (home `FEmusic_home` + mood chips, explore
  `FEmusic_explore`, moods `FEmusic_moods_and_genres[_category]`, charts,
  new releases, albums `MPREb_…`, playlists `VL…`, artists `UC…`, lyrics
  `MPLYt…`, related `MPTRt…`), `search` (+ filter params from the chips),
  `music/get_search_suggestions`, `next` (the queue: a song's radio
  `RDAMVM<id>`, an album `OLAK…` or a playlist, with radio continuations).
- Parsers turn renderers into a small model: `Track`, `Card` (album, single,
  playlist, artist, mix), `MoodTile`, `Shelf` (carousel, track columns, list,
  grid), and page types. Parsers look for renderers rather than fixed paths,
  so a reshuffled response loses a field, not the page.
- `tools/fetch-fixtures.py DIR` saves real responses; the parser tests run
  against them with `SLOPTUBE_MUSIC_FIXTURES=DIR cargo test`.

## Audio

- yt-dlp resolves stream URLs through one resident Python helper (as in
  SlopTube). It is started on first use and **exits after 5 minutes idle**,
  since a Python process with yt-dlp imported costs ~60–80 MB.
- The next track's stream is resolved as soon as a track starts, so skipping
  and auto-advancing don't wait for yt-dlp.
- One `playbin3` with only audio enabled; Opus/WebM (itag 251/250/249) by
  default, AAC/MP4 as fallback. Small, bounded buffers.
- **Gapless**: on `about-to-finish` the prepared next URI is handed to
  playbin3; the controller advances when the new stream starts.

## Queue (`playback.rs`)

- Playing a song starts its radio (like YTM). Playing from an album or
  playlist queues its tracks. The queue is a `gio::ListStore` that the Up next
  view binds to directly.
- Radio queues extend themselves near the end; when a queue runs out with
  autoplay on, a radio of the last track continues.
- Shuffle (reorders what's after the current track, restorable), repeat
  off/all/one, previous restarts the track after 3 s.
- Local history; queue and position restored at startup (paused).

## Interface

- **Narrow (phone)**: header with the logo, search and menu; bottom tabs Home,
  Explore, Library; mini player above the tabs (tap or drag up to open,
  swipe sideways for next/previous); full-screen Now playing with the art,
  controls and an Up next / Lyrics / Related bottom sheet.
- **Wide (desktop)**: sidebar with the tabs, a full-width player bar at the
  bottom (transport, track info, like, volume, repeat, shuffle), and a Now
  playing page above the bar with the art on the left and Up next / Lyrics /
  Related on the right.
- Near-black in the dark style, like YouTube Music (the style itself is
  SlopTube's setting); Now playing and album headers are tinted with the
  artwork's dominant colour.
- Pages: Home (mood chips + shelves), Explore (new releases, charts, moods &
  genres), Search (suggestions, history, filter chips), Album/Playlist,
  Artist, generic browse pages, Library.

## Memory and speed budget

- Virtualised lists everywhere: a `GtkListView` of shelves (each row a
  carousel), of tracks (with the page header as the first row), of the queue.
- Artwork is requested from Google's image servers at the displayed size
  (`=wN-hN`) and decoded at that size; an LRU texture cache caps memory.
- JSON is parsed on the I/O thread into the model and dropped right away;
  glibc is asked to return freed memory after big responses
  (`malloc_trim`) and its arenas are limited.
- Explore and Library load when first shown; popped pages are freed.

## Milestones

1. **Done in this pass**: browsing (home, chips, explore, moods, charts,
   new releases), search, album/playlist/artist pages, playback with radio,
   gapless, shuffle/repeat, Now playing with Up next/Lyrics/Related, mini
   player and desktop bar, MPRIS, preferences, local library (liked songs,
   saved albums/playlists/artists, history).
2. **Done**: Google account. The session in the keyring, signed requests,
   sign-in through Google's page (now SlopTube's shared sign-in dialog),
   Firefox and Chromium import; personalised Home, the
   YouTube Music library, likes/dislikes, saving and subscribing, Premium
   streams, plays added to history. Next: creating and editing playlists.
3. Downloads for offline listening; time-synced lyrics; Song/Video toggle
   (video through SlopTube's pipeline); InnerTube player fast path to skip
   yt-dlp where possible; packaging (PKGBUILD, Flatpak).
