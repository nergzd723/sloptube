//! `sloptube --diagnose[=VIDEO_ID]`: play a video muted for a few seconds and report
//! which decoder ran, whether frames stayed in DMABuf memory, and dropped frames.
//! A snapshot of the player (rendered by GTK's real renderer) is written as a PNG.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{glib, graphene};

use crate::api::VideoItem;
use crate::config;
use crate::player::PlayState;
use crate::settings;
use crate::ui::window::Window;
use gtk::prelude::NativeExt;

pub const DEFAULT_VIDEO: &str = "aqz-KE-bpKQ";
fn run_secs() -> u32 {
    std::env::var("SLOPTUBE_DIAGNOSE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(14).max(10)
}

thread_local! {
    static REQUESTED: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// Called from `main` when `--diagnose` was given.
pub fn request(video: String) {
    settings::set_read_only(true);
    REQUESTED.with(|r| r.set(Some(Box::leak(video.into_boxed_str()))));
}

pub fn requested() -> bool {
    REQUESTED.with(|r| r.get().is_some())
}

fn snapshot_png(win: &Window, paintable: &gtk::gdk::Paintable, dir: &std::path::Path) -> anyhow::Result<()> {
    let image = paintable.current_image();
    let (w, h) = (image.intrinsic_width(), image.intrinsic_height());
    anyhow::ensure!(w > 0 && h > 0, "no video frame yet");
    // 1) What GTK's renderer draws (GPU colour conversion on GL/Vulkan).
    let snapshot = gtk::Snapshot::new();
    image.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node().ok_or_else(|| anyhow::anyhow!("nothing was rendered"))?;
    let renderer = win.renderer().ok_or_else(|| anyhow::anyhow!("no renderer"))?;
    let texture = renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, w as f32, h as f32)));
    texture.save_to_png(dir.join("diagnose-rendered.png"))?;
    // 2) GDK's own download of the frame (CPU conversion).
    if let Ok(t) = image.downcast::<gtk::gdk::Texture>() {
        let _ = t.save_to_png(dir.join("diagnose-download.png"));
    }
    Ok(())
}

fn rss_mb() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let get = |key: &str| {
        status
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    };
    format!("{}MB(anon {} file {})", get("VmRSS:"), get("RssAnon:"), get("RssFile:"))
}

pub fn start(win: &Window) {
    let Some(video) = REQUESTED.with(|r| r.get()) else { return };
    let pb = win.playback();
    let player = pb.player();
    player.set_muted(true);
    let idle = std::env::var_os("SLOPTUBE_DIAGNOSE_IDLE").is_some();
    println!("diagnose: renderer {}", win.native().and_then(|n| n.renderer()).map(|r| r.type_().name().to_string()).unwrap_or_default());
    if !idle {
        pb.open(
            VideoItem {
                id: video.to_owned(),
                ..Default::default()
            },
            None,
            Some(30.0),
        );
        win.expand_player();
    }

    let ticks = Rc::new(Cell::new(0u32));
    let win = win.downgrade();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        let Some(win) = win.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let pb = win.playback();
        let p = pb.player();
        let st = p.stats();
        let t = ticks.get() + 1;
        ticks.set(t);
        println!(
            "diagnose t={t:>2}s state={:?} pos={:.1}s decoder={} hw={} dmabuf={} frames={}x{} {} {}fps rendered={} dropped={} rss={} error={:?}",
            p.state(),
            p.position(),
            st.video_decoder.as_deref().unwrap_or("-"),
            st.hardware,
            st.dmabuf,
            st.width,
            st.height,
            st.format,
            st.framerate,
            st.rendered,
            st.dropped,
            rss_mb(),
            pb.error()
        );
        let sheet_test = std::env::var_os("SLOPTUBE_DIAGNOSE_SHEET").is_some();
        if sheet_test {
            if let Some(sheet) = win.sheet() {
                match t {
                    5 => sheet.set_progress(0.75),
                    6 => sheet.set_progress(0.45),
                    7 => sheet.set_progress(0.15),
                    8 => sheet.animate_to(0.0, 0.0),
                    10 => sheet.animate_to(1.0, 0.0),
                    _ => {}
                }
                println!("diagnose: sheet progress {:.2}", sheet.progress());
                if t == 9 {
                    println!("diagnose: layout {}", win.debug_layout());
                }
            }
        } else if t == 7 {
            println!("diagnose: seeking to 60s");
            p.seek(60.0, true);
        }
        if t == run_secs() - 2 {
            let dir = config::runtime_dir();
            if let Some(paintable) = p.paintable() {
                match snapshot_png(&win, &paintable, &dir) {
                    Ok(()) => println!("diagnose: frame snapshots saved to {}/diagnose-*.png", dir.display()),
                    Err(e) => println!("diagnose: snapshot failed: {e:#}"),
                }
            }
        }
        if t >= run_secs() || p.state() == PlayState::Error {
            if let Some(app) = win.application() {
                pb.stop();
                app.quit();
            }
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}
