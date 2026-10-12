//! Remote image loading with small, size-bounded caches.
//!
//! Images are downloaded on the I/O runtime and decoded on a worker thread *at
//! the size they are shown*, so a feed full of thumbnails costs a few hundred KB
//! per card instead of several MB.
//!
//! Most YouTube thumbnails are WebP, whatever their URL ends in. Those are
//! decoded here with `image-webp`: gdk-pixbuf only reads WebP with an extra
//! loader (Homebrew's has none, so thumbnails stayed blank on macOS) or, on
//! newer Linux systems, through glycin's sandboxed loader processes. Everything
//! else (JPEG avatars, PNG) goes through gdk-pixbuf, which scales JPEGs while
//! decoding them.
//!
//! YouTube's image servers answer a burst of requests on one connection slowly
//! (each took ~600 ms instead of ~60 ms when a page's 30 thumbnails were asked
//! for at once), so every card waited for all of them. Requests therefore go a
//! few at a time per host, in the order cards ask for them, which fills the top
//! of a feed first; a request nobody wants any more by its turn is dropped. The
//! downloaded files are kept as well, so an image whose texture was evicted is
//! decoded again rather than downloaded again.


use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, glib};
use tokio::sync::{Semaphore, oneshot};

use crate::runtime;

/// Decoded textures (counted at 4 bytes per pixel).
const TEXTURE_BUDGET_BYTES: usize = 40 * 1024 * 1024;
/// Downloaded files; a thumbnail is 5-70 KB.
const FILE_BUDGET_BYTES: usize = 16 * 1024 * 1024;
/// Requests in flight per host.
const REQUESTS_PER_HOST: usize = 6;
/// Per attempt; images normally take well under a second.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// Larger images are refused rather than decoded.
const MAX_PIXELS: u64 = 4096 * 4096;

/// A least-recently-used map that evicts down to a byte budget.
struct Lru<V> {
    entries: HashMap<String, (V, usize, u64)>,
    total: usize,
    clock: u64,
    budget: usize,
}

impl<V: Clone> Lru<V> {
    fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            total: 0,
            clock: 0,
            budget,
        }
    }

    fn get(&mut self, key: &str) -> Option<V> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|e| {
            e.2 = clock;
            e.0.clone()
        })
    }

    fn insert(&mut self, key: String, value: V, bytes: usize) {
        self.clock += 1;
        if let Some(old) = self.entries.insert(key, (value, bytes, self.clock)) {
            self.total -= old.1;
        }
        self.total += bytes;
        while self.total > self.budget && self.entries.len() > 1 {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.2).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some(e) = self.entries.remove(&oldest) {
                self.total -= e.1;
            }
        }
    }
}

/// A texture being produced, and who is waiting for it.
#[derive(Default)]
struct Inflight {
    waiters: Vec<oneshot::Sender<Option<gdk::Texture>>>,
    /// The widgets that asked for it. If none of them still shows this image by
    /// the time its download would start, the download is skipped.
    owners: Vec<glib::WeakRef<glib::Object>>,
}

struct Cache {
    textures: Lru<gdk::Texture>,
    files: Lru<Arc<[u8]>>,
    inflight: HashMap<String, Inflight>,
    hosts: HashMap<String, Rc<Semaphore>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            textures: Lru::new(TEXTURE_BUDGET_BYTES),
            files: Lru::new(FILE_BUDGET_BYTES),
            inflight: HashMap::new(),
            hosts: HashMap::new(),
        }
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Round requested widths up to a few buckets so cache entries get reused.
fn bucket(width: u32) -> u32 {
    const BUCKETS: [u32; 8] = [48, 96, 176, 256, 360, 480, 720, 1280];
    BUCKETS.into_iter().find(|b| *b >= width).unwrap_or(1920)
}

fn key(url: &str, width: u32) -> String {
    format!("{width}|{url}")
}

// ---------------------------------------------------------------------------
// Decoding (worker thread)
// ---------------------------------------------------------------------------

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn decode(bytes: &[u8], max_width: u32) -> Option<gdk::Texture> {
    if is_webp(bytes) {
        decode_webp(bytes, max_width)
    } else {
        decode_pixbuf(bytes, max_width)
    }
}

fn decode_webp(bytes: &[u8], max_width: u32) -> Option<gdk::Texture> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    let mut pixels = vec![0; decoder.output_buffer_size()?];
    decoder.read_image(&mut pixels).ok()?;
    let has_alpha = decoder.has_alpha();
    let channels = if has_alpha { 4 } else { 3 };
    let pixbuf = gdk_pixbuf::Pixbuf::from_bytes(
        &glib::Bytes::from_owned(pixels),
        gdk_pixbuf::Colorspace::Rgb,
        has_alpha,
        8,
        w as i32,
        h as i32,
        (w * channels) as i32,
    );
    let pixbuf = if w > max_width {
        let nh = ((u64::from(h) * u64::from(max_width)) / u64::from(w)).max(1) as i32;
        pixbuf.scale_simple(max_width as i32, nh, gdk_pixbuf::InterpType::Bilinear)?
    } else {
        pixbuf
    };
    Some(texture_for(&pixbuf))
}

fn decode_pixbuf(bytes: &[u8], max_width: u32) -> Option<gdk::Texture> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.connect_size_prepared(move |l, w, h| {
        if w > max_width as i32 && w > 0 {
            let nh = ((h as i64 * max_width as i64) / w as i64).max(1) as i32;
            l.set_size(max_width as i32, nh);
        }
    });
    let ok = loader.write(bytes).is_ok();
    let _ = loader.close();
    let pixbuf = loader.pixbuf().filter(|_| ok)?;
    Some(texture_for(&pixbuf))
}

fn texture_for(pixbuf: &gdk_pixbuf::Pixbuf) -> gdk::Texture {
    // Straight (not premultiplied) alpha, like gdk-pixbuf's.
    let format = if pixbuf.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    gdk::MemoryTexture::new(
        pixbuf.width(),
        pixbuf.height(),
        format,
        &pixbuf.read_pixel_bytes(),
        pixbuf.rowstride() as usize,
    )
    .upcast()
}

// ---------------------------------------------------------------------------
// Loading (main thread)
// ---------------------------------------------------------------------------

fn host_slots(url: &str) -> Rc<Semaphore> {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    CACHE.with(|c| {
        c.borrow_mut()
            .hosts
            .entry(host)
            .or_insert_with(|| Rc::new(Semaphore::new(REQUESTS_PER_HOST)))
            .clone()
    })
}

/// Whether any widget that asked for `k` still shows it.
fn still_wanted(k: &str) -> bool {
    CACHE.with(|c| {
        c.borrow().inflight.get(k).is_some_and(|i| {
            i.owners
                .iter()
                .any(|o| o.upgrade().is_some_and(|o| wanted(&o) == k))
        })
    })
}

enum Fetch {
    Done(Arc<[u8]>),
    /// The server said no (e.g. 404); asking again won't help.
    Refused,
    /// A network error, timeout or server-side failure.
    Failed,
}

async fn fetch(url: &str) -> Fetch {
    match crate::api::client::http().get(url).timeout(REQUEST_TIMEOUT).send().await {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => Fetch::Done(Arc::from(&b[..])),
            Err(e) => {
                log::debug!("image {url}: {e}");
                Fetch::Failed
            }
        },
        Ok(resp) => {
            let status = resp.status();
            log::debug!("image {url}: {status}");
            if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                Fetch::Failed
            } else {
                Fetch::Refused
            }
        }
        Err(e) => {
            log::debug!("image {url}: {e}");
            Fetch::Failed
        }
    }
}

/// Download on the I/O runtime, trying once more after a network error or a
/// server-side failure.
async fn download(url: String) -> Option<Arc<[u8]>> {
    runtime::run(async move {
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
            match fetch(&url).await {
                Fetch::Done(b) => return Some(b),
                Fetch::Refused => return None,
                Fetch::Failed => {}
            }
        }
        None
    })
    .await
}

async fn decode_off_thread(file: Arc<[u8]>, width: u32) -> Option<gdk::Texture> {
    runtime::run(async move {
        tokio::task::spawn_blocking(move || decode(&file, width))
            .await
            .ok()
            .flatten()
    })
    .await
}

/// Produce the texture for `k`: decode the cached file, or download it when
/// its turn comes (unless nobody wants it by then).
async fn produce(url: &str, width: u32, k: &str) -> Option<gdk::Texture> {
    let file = match CACHE.with(|c| c.borrow_mut().files.get(url)) {
        Some(f) => f,
        None => {
            let slots = host_slots(url);
            let _slot = slots.acquire().await.ok()?;
            if !still_wanted(k) {
                return None;
            }
            let f = download(url.to_owned()).await?;
            CACHE.with(|c| c.borrow_mut().files.insert(url.to_owned(), f.clone(), f.len()));
            f
        }
    };
    let tex = decode_off_thread(file, width).await;
    if tex.is_none() {
        log::debug!("couldn't decode image {url}");
    }
    tex
}

/// Load an image at (up to) `width` pixels wide for `owner`, a widget whose
/// wanted key (see `remember`) is `key(url, width)`. Concurrent requests for
/// the same image share one download.
async fn load(url: &str, width: u32, owner: &glib::Object) -> Option<gdk::Texture> {
    let k = key(url, width);
    if let Some(t) = CACHE.with(|c| c.borrow_mut().textures.get(&k)) {
        return Some(t);
    }
    let (tx, rx) = oneshot::channel();
    let first = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let entry = c.inflight.entry(k.clone()).or_default();
        entry.waiters.push(tx);
        entry.owners.push(owner.downgrade());
        entry.waiters.len() == 1
    });
    if first {
        let url = url.to_owned();
        glib::spawn_future_local(async move {
            let tex = produce(&url, width, &k).await;
            CACHE.with(|c| {
                let mut c = c.borrow_mut();
                if let Some(t) = &tex {
                    let bytes = (t.width() * t.height()) as usize * 4;
                    c.textures.insert(k.clone(), t.clone(), bytes);
                }
                for w in c.inflight.remove(&k).map(|i| i.waiters).unwrap_or_default() {
                    let _ = w.send(tex.clone());
                }
            });
        });
    }
    rx.await.ok().flatten()
}

const URL_KEY: &str = "sloptube-image-url";

fn remember(obj: &impl IsA<glib::Object>, k: &str) {
    // SAFETY: the key is only ever used with `String` values.
    unsafe {
        obj.as_ref().set_data(URL_KEY, k.to_owned());
    }
}

fn wanted(obj: &impl IsA<glib::Object>) -> String {
    // SAFETY: see `remember`.
    unsafe {
        obj.as_ref()
            .data::<String>(URL_KEY)
            .map(|p| p.as_ref().clone())
            .unwrap_or_default()
    }
}

/// Show a remote image in a `GtkPicture`, decoded for `width` device pixels.
/// Late results for recycled widgets are ignored.
pub fn set_picture(picture: &gtk::Picture, url: Option<&str>, width: u32) {
    let width = bucket(width);
    let k = url.filter(|u| !u.is_empty()).map(|u| key(u, width)).unwrap_or_default();
    if wanted(picture) == k && (k.is_empty() || picture.paintable().is_some()) {
        return;
    }
    remember(picture, &k);
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        picture.set_paintable(None::<&gdk::Paintable>);
        return;
    };
    if let Some(t) = CACHE.with(|c| c.borrow_mut().textures.get(&k)) {
        picture.set_paintable(Some(&t));
        return;
    }
    picture.set_paintable(None::<&gdk::Paintable>);
    let picture = picture.downgrade();
    let url = url.to_owned();
    glib::spawn_future_local(async move {
        let Some(owner) = picture.upgrade() else { return };
        let tex = load(&url, width, owner.upcast_ref()).await;
        drop(owner);
        if let Some(p) = picture.upgrade() {
            if wanted(&p) == k {
                p.set_paintable(tex.as_ref());
            }
        }
    });
}

/// Show a remote image in an `AdwAvatar` (falls back to initials).
pub fn set_avatar(avatar: &adw::Avatar, url: Option<&str>) {
    let width = bucket((avatar.size() * avatar.scale_factor().max(1)) as u32);
    let k = url.filter(|u| !u.is_empty()).map(|u| key(u, width)).unwrap_or_default();
    remember(avatar, &k);
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        avatar.set_custom_image(None::<&gdk::Paintable>);
        return;
    };
    if let Some(t) = CACHE.with(|c| c.borrow_mut().textures.get(&k)) {
        avatar.set_custom_image(Some(&t));
        return;
    }
    avatar.set_custom_image(None::<&gdk::Paintable>);
    let avatar = avatar.downgrade();
    let url = url.to_owned();
    glib::spawn_future_local(async move {
        let Some(owner) = avatar.upgrade() else { return };
        let tex = load(&url, width, owner.upcast_ref()).await;
        drop(owner);
        if let Some(a) = avatar.upgrade() {
            if wanted(&a) == k {
                a.set_custom_image(tex.as_ref());
            }
        }
    });
}

/// Request avatar images at a size that suits the widget (YouTube URLs encode `=sNN`).
pub fn sized_avatar_url(url: &str, size: u32) -> String {
    if let Some(pos) = url.rfind("=s") {
        let rest = &url[pos + 2..];
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 {
            return format!("{}=s{}{}", &url[..pos], size, &rest[digits..]);
        }
    }
    url.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_least_recently_used() {
        let mut lru = Lru::new(10);
        lru.insert("a".into(), 1, 4);
        lru.insert("b".into(), 2, 4);
        assert_eq!(lru.get("a"), Some(1));
        lru.insert("c".into(), 3, 4);
        assert_eq!(lru.get("b"), None);
        assert_eq!(lru.get("a"), Some(1));
        assert_eq!(lru.get("c"), Some(3));
        assert_eq!(lru.total, 8);
    }

    #[test]
    fn recognises_webp() {
        assert!(is_webp(b"RIFF\x24\x00\x00\x00WEBPVP8 "));
        assert!(!is_webp(b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01"));
        assert!(!is_webp(b"RIFF"));
    }
}
