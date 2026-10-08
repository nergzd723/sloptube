//! Remote image loading with a small, size-bounded texture cache.
//!
//! Images are downloaded on the I/O runtime and decoded *at the size they are
//! shown* (JPEG DCT scaling via gdk-pixbuf), so a feed full of thumbnails costs
//! a few hundred KB per card instead of several MB.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, glib};
use tokio::sync::oneshot;

use crate::runtime;

const CACHE_BUDGET_BYTES: usize = 40 * 1024 * 1024;

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
}

impl Cache {
    fn get(&mut self, key: &str) -> Option<gdk::Texture> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|e| {
            e.last_use = clock;
            e.texture.clone()
        })
    }

    fn insert(&mut self, key: String, texture: gdk::Texture) {
        let bytes = (texture.width() * texture.height() * 3) as usize;
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
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_use)
                .map(|(k, _)| k.clone())
            else {
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
fn bucket(width: u32) -> u32 {
    const BUCKETS: [u32; 8] = [48, 96, 176, 256, 360, 480, 720, 1280];
    BUCKETS.into_iter().find(|b| *b >= width).unwrap_or(1920)
}

fn key(url: &str, width: u32) -> String {
    format!("{width}|{url}")
}

/// Decode (and downscale) on a worker thread.
fn decode(bytes: &[u8], max_width: u32) -> Option<gdk::Texture> {
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
    Some(gdk::Texture::for_pixbuf(&pixbuf))
}

async fn fetch(url: String, max_width: u32) -> Option<gdk::Texture> {
    runtime::run(async move {
        let resp = crate::api::client::http().get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = resp.bytes().await.ok()?;
        tokio::task::spawn_blocking(move || decode(&bytes, max_width))
            .await
            .ok()
            .flatten()
    })
    .await
}

/// Load an image at (up to) `width` pixels wide (main thread).
/// Concurrent requests for the same image share one download.
pub async fn load(url: &str, width: u32) -> Option<gdk::Texture> {
    if url.is_empty() {
        return None;
    }
    let width = bucket(width);
    let k = key(url, width);
    if let Some(t) = CACHE.with(|c| c.borrow_mut().get(&k)) {
        return Some(t);
    }
    let (tx, rx) = oneshot::channel();
    let first = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let waiters = c.inflight.entry(k.clone()).or_default();
        waiters.push(tx);
        waiters.len() == 1
    });
    if first {
        let url = url.to_owned();
        glib::spawn_future_local(async move {
            let tex = fetch(url, width).await;
            CACHE.with(|c| {
                let mut c = c.borrow_mut();
                if let Some(t) = &tex {
                    c.insert(k.clone(), t.clone());
                }
                for w in c.inflight.remove(&k).unwrap_or_default() {
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
    if let Some(t) = CACHE.with(|c| c.borrow_mut().get(&k)) {
        picture.set_paintable(Some(&t));
        return;
    }
    picture.set_paintable(None::<&gdk::Paintable>);
    let picture = picture.downgrade();
    let url = url.to_owned();
    glib::spawn_future_local(async move {
        let tex = load(&url, width).await;
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
    if let Some(t) = CACHE.with(|c| c.borrow_mut().get(&k)) {
        avatar.set_custom_image(Some(&t));
        return;
    }
    avatar.set_custom_image(None::<&gdk::Paintable>);
    let avatar = avatar.downgrade();
    let url = url.to_owned();
    glib::spawn_future_local(async move {
        let tex = load(&url, width).await;
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
