//! Artwork: the shared image cache (`common::image`, re-exported here), and
//! the dominant colour Now playing and album headers are tinted with.

use std::cell::RefCell;
use std::collections::HashMap;

pub use common::image::*;
use gtk::gdk;
use gtk::prelude::*;

use crate::api::{self, Thumbnail};

thread_local! {
    /// Artwork URL -> its colour (or none).
    static COLORS: RefCell<HashMap<String, Option<gdk::RGBA>>> = RefCell::new(HashMap::new());
}

/// A dark, saturated tint from the artwork, for backgrounds behind white text.
pub async fn dominant_color(thumbs: &[Thumbnail]) -> Option<gdk::RGBA> {
    let url = api::thumb_url(thumbs, 32)?;
    if let Some(c) = COLORS.with(|c| c.borrow().get(&url).cloned()) {
        return c;
    }
    let color = load(&url, 32).await.and_then(|t| average_color(&t));
    COLORS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 256 {
            c.clear();
        }
        c.insert(url, color);
    });
    color
}

fn average_color(texture: &gdk::Texture) -> Option<gdk::RGBA> {
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let data: &[u8] = &bytes;
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    let (mut r, mut g, mut b, mut total) = (0f64, 0f64, 0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * 4;
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
