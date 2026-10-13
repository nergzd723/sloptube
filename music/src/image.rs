//! Artwork loading with a small, size-bounded texture cache.
//!
//! Google's image servers scale on request, so artwork is fetched at the size
//! it is shown (see `api::thumb_url`) and decoded at that size; a screen full
//! of album covers costs a few megabytes. Widths are rounded up to a few
//! buckets so cache entries get reused.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, glib};
use tokio::sync::oneshot;

use crate::api::{self, Thumbnail};
use crate::runtime;

/// Decoded textures (what's on screen and around it).
const CACHE_BUDGET_BYTES: usize = 24 * 1024 * 1024;
/// Compressed downloads: ~400 thumbnails, so scrolling back re-decodes (a few
/// milliseconds) instead of downloading again (hundreds).
const BYTES_BUDGET: usize = 8 * 1024 * 1024;

struct Entry {
    texture: gdk::Texture,
    bytes: usize,
    last_use: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<String, Entry>,
    total: usize,
    clock: u64,
    inflight: HashMap<String, Vec<oneshot::Sender<Option<gdk::Texture>>>>,
    colors: HashMap<String, Option<gdk::RGBA>>,
    /// url -> (JPEG bytes, last use)
    bytes: HashMap<String, (Arc<Vec<u8>>, u64)>,
    bytes_total: usize,
}

impl Cache {
    fn get_bytes(&mut self, url: &str) -> Option<Arc<Vec<u8>>> {
        self.clock += 1;
        let clock = self.clock;
        self.bytes.get_mut(url).map(|(b, t)| {
            *t = clock;
            b.clone()
        })
    }

    fn insert_bytes(&mut self, url: String, data: Arc<Vec<u8>>) {
        self.clock += 1;
        self.bytes_total += data.len();
        if let Some((old, _)) = self.bytes.insert(url, (data, self.clock)) {
            self.bytes_total -= old.len();
        }
        while self.bytes_total > BYTES_BUDGET && self.bytes.len() > 1 {
            let Some(oldest) = self.bytes.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some((b, _)) = self.bytes.remove(&oldest) {
                self.bytes_total -= b.len();
            }
        }
    }

    fn get(&mut self, key: &str) -> Option<gdk::Texture> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|e| {
            e.last_use = clock;
            e.texture.clone()
        })
    }

    fn insert(&mut self, key: String, texture: gdk::Texture) {
        let bytes = (texture.width() * texture.height() * 4) as usize;
        self.clock += 1;
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                texture,
                bytes,
                last_use: self.clock,
            },
        ) {
            self.total -= old.bytes;
        }
        self.total += bytes;
        while self.total > CACHE_BUDGET_BYTES && self.entries.len() > 1 {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.last_use).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some(e) = self.entries.remove(&oldest) {
                self.total -= e.bytes;
            }
        }
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Round requested widths up to a few buckets so cache entries get reused.
pub fn bucket(width: u32) -> u32 {
    const BUCKETS: [u32; 9] = [48, 64, 96, 128, 176, 256, 360, 544, 720];
    BUCKETS.into_iter().find(|b| *b >= width).unwrap_or(1080)
}

fn decode(bytes: &[u8], max_width: u32) -> Option<gdk_pixbuf::Pixbuf> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.connect_size_prepared(move |l, w, h| {
        if w > max_width as i32 && w > 0 {
            let nh = ((h as i64 * max_width as i64) / w as i64).max(1) as i32;
            l.set_size(max_width as i32, nh);
        }
    });
    let ok = loader.write(bytes).is_ok();
    let _ = loader.close();
    loader.pixbuf().filter(|_| ok)
}

/// Download (unless `cached` has the bytes) and decode an image off the main thread.
async fn fetch(
    url: String,
    max_width: u32,
    cached: Option<Arc<Vec<u8>>>,
) -> Option<(gdk_pixbuf::Pixbuf, Arc<Vec<u8>>)> {
    let res = runtime::run(async move {
        let started = std::time::Instant::now();
        let from_cache = cached.is_some();
        let data = match cached {
            Some(d) => d,
            None => {
                let resp = api::client::http().get(&url).send().await.ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                Arc::new(resp.bytes().await.ok()?.to_vec())
            }
        };
        let fetched = started.elapsed();
        let input = data.clone();
        let pixbuf = tokio::task::spawn_blocking(move || decode(&input, max_width).map(SendPixbuf))
            .await
            .ok()
            .flatten()?;
        log::debug!(
            "image {}: {} in {fetched:?}, decoded at {:?}, {} B",
            url.split('/').nth(2).unwrap_or(""),
            if from_cache { "cached" } else { "downloaded" },
            started.elapsed(),
            data.len()
        );
        Some((pixbuf, data))
    })
    .await;
    res.map(|(p, d)| (p.0, d))
}

/// Open connections to the artwork servers ahead of the first thumbnails, so
/// they don't wait for TLS handshakes (connections stay pooled for a minute).
pub fn warm_up_connections() {
    runtime::spawn(async {
        let hosts = [
            "https://yt3.googleusercontent.com/",
            "https://lh3.googleusercontent.com/",
            "https://i.ytimg.com/",
            "https://yt3.ggpht.com/",
        ];
        let requests = hosts.map(|h| api::client::http().head(h).send());
        for r in requests {
            let _ = r.await;
        }
    });
}

/// Pixbufs are plain pixel buffers; this lets one cross back from the decoder thread.
struct SendPixbuf(gdk_pixbuf::Pixbuf);
// SAFETY: the pixbuf is created on the worker and only used on the main thread afterwards.
unsafe impl Send for SendPixbuf {}

/// Load an image (main thread). Concurrent requests for the same URL share one download.
pub async fn load(url: &str, width: u32) -> Option<gdk::Texture> {
    if url.is_empty() {
        return None;
    }
    let key = format!("{width}|{url}");
    if let Some(t) = CACHE.with(|c| c.borrow_mut().get(&key)) {
        return Some(t);
    }
    let (tx, rx) = oneshot::channel();
    let first = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let waiters = c.inflight.entry(key.clone()).or_default();
        waiters.push(tx);
        waiters.len() == 1
    });
    if first {
        let url = url.to_owned();
        let cached = CACHE.with(|c| c.borrow_mut().get_bytes(&url));
        glib::spawn_future_local(async move {
            let res = fetch(url.clone(), width, cached).await;
            let tex = res.as_ref().map(|(p, _)| gdk::Texture::for_pixbuf(p));
            CACHE.with(|c| {
                let mut c = c.borrow_mut();
                if let Some((_, data)) = res {
                    c.insert_bytes(url, data);
                }
                if let Some(t) = &tex {
                    c.insert(key.clone(), t.clone());
                }
                for w in c.inflight.remove(&key).unwrap_or_default() {
                    let _ = w.send(tex.clone());
                }
            });
        });
    }
    rx.await.ok().flatten()
}

const URL_KEY: &str = "stmusic-image-key";

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

/// The decoded texture, if it's in memory already (no loading).
pub fn cached_texture(url: &str, width: u32) -> Option<gdk::Texture> {
    CACHE.with(|c| c.borrow_mut().get(&format!("{width}|{url}")))
}

/// Whether `target` already shows (or is loading) this image.
pub fn is_wanted(target: &impl IsA<glib::Object>, url: &str, width: u32) -> bool {
    wanted(target) == format!("{width}|{url}")
}

/// Something that can show a loaded texture.
pub trait ArtTarget: IsA<glib::Object> + IsA<gtk::Widget> {
    fn show_texture(&self, texture: Option<&gdk::Texture>);
    fn has_texture(&self) -> bool;
}

impl ArtTarget for gtk::Picture {
    fn show_texture(&self, texture: Option<&gdk::Texture>) {
        self.set_paintable(texture);
    }

    fn has_texture(&self) -> bool {
        self.paintable().is_some()
    }
}

impl ArtTarget for adw::Avatar {
    fn show_texture(&self, texture: Option<&gdk::Texture>) {
        self.set_custom_image(texture);
    }

    fn has_texture(&self) -> bool {
        self.custom_image().is_some()
    }
}

/// Show the image at `url`, `width` pixels wide. Late results for recycled widgets are ignored.
pub fn set_url<T: ArtTarget>(target: &T, url: Option<&str>, width: u32) {
    let k = url.filter(|u| !u.is_empty()).map(|u| format!("{width}|{u}")).unwrap_or_default();
    if wanted(target) == k && (k.is_empty() || target.has_texture()) {
        return;
    }
    remember(target, &k);
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        target.show_texture(None);
        return;
    };
    if let Some(t) = CACHE.with(|c| c.borrow_mut().get(&k)) {
        target.show_texture(Some(&t));
        return;
    }
    target.show_texture(None);
    let weak = target.downgrade();
    let url = url.to_owned();
    glib::spawn_future_local(async move {
        let tex = load(&url, width).await;
        if let Some(t) = weak.upgrade()
            && wanted(&t) == k {
                t.show_texture(tex.as_ref());
            }
    });
}

/// A dark, saturated tint from the artwork, for backgrounds behind white text.
pub async fn dominant_color(thumbs: &[Thumbnail]) -> Option<gdk::RGBA> {
    let url = api::thumb_url(thumbs, 32)?;
    if let Some(c) = CACHE.with(|c| c.borrow().colors.get(&url).cloned()) {
        return c;
    }
    let pixbuf = fetch(url.clone(), 32, None).await;
    let color = pixbuf.as_ref().and_then(|(p, _)| average_color(p));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.colors.len() > 256 {
            c.colors.clear();
        }
        c.colors.insert(url, color);
    });
    color
}

fn average_color(p: &gdk_pixbuf::Pixbuf) -> Option<gdk::RGBA> {
    let bytes = p.read_pixel_bytes();
    let data: &[u8] = &bytes;
    let (w, h) = (p.width() as usize, p.height() as usize);
    let (stride, n) = (p.rowstride() as usize, p.n_channels() as usize);
    let (mut r, mut g, mut b, mut total) = (0f64, 0f64, 0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * n;
            if i + 2 >= data.len() {
                continue;
            }
            let (pr, pg, pb) = (data[i] as f64 / 255.0, data[i + 1] as f64 / 255.0, data[i + 2] as f64 / 255.0);
            let max = pr.max(pg).max(pb);
            let min = pr.min(pg).min(pb);
            let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
            // Colourful, reasonably bright pixels say more about a cover than grey ones.
            let weight = 0.05 + sat * max;
            r += pr * weight;
            g += pg * weight;
            b += pb * weight;
            total += weight;
        }
    }
    if total <= 0.0 {
        return None;
    }
    let (h, s, _) = rgb_to_hsl(r / total, g / total, b / total);
    let (r, g, b) = hsl_to_rgb(h, (s * 1.1).min(0.65), 0.26);
    Some(gdk::RGBA::new(r as f32, g as f32, b as f32, 1.0))
}

fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    if s <= 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsl_roundtrip() {
        let (h, s, l) = rgb_to_hsl(0.8, 0.2, 0.4);
        let (r, g, b) = hsl_to_rgb(h, s, l);
        assert!((r - 0.8).abs() < 1e-6 && (g - 0.2).abs() < 1e-6 && (b - 0.4).abs() < 1e-6);
    }
}
