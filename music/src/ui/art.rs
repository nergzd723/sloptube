//! Small drawing widgets: artwork, colour swatches and tinted backgrounds.
//!
//! `ArtView` draws a texture straight into the snapshot, cover-cropped with
//! rounded corners, at a fixed size (or as the largest centred square that
//! fits). That is cheaper than a picture inside a clipping container and
//! never lets the texture's own size leak into the layout.
//!
//! Artwork loads lazily: only once the view is on screen or within half a
//! screen of it. Lists keep many rows around, and carousels have tiles far to
//! the side; loading all of them at once delayed the visible ones by seconds.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::api::{self, Thumbnail};
use crate::image::{self, ArtTarget};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ArtView {
        pub texture: RefCell<Option<gdk::Texture>>,
        /// Fixed size; (0, 0) fills the allocation with a centred square.
        pub size: Cell<(i32, i32)>,
        /// Natural size in fill mode.
        pub natural: Cell<i32>,
        /// Banner mode: as wide as allocated, this tall.
        pub banner: Cell<i32>,
        pub radius: Cell<f32>,
        pub round: Cell<bool>,
        /// Draw a soft drop shadow around the artwork.
        pub shadow: Cell<bool>,
        /// Load at once instead of when scrolled into view.
        pub eager: Cell<bool>,
        /// (url, width) to load once visible.
        pub pending: RefCell<Option<(String, u32)>>,
        /// In the lazy-loading queue.
        pub queued: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArtView {
        const NAME: &'static str = "StmArtView";
        type Type = super::ArtView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("artview");
        }
    }

    impl ObjectImpl for ArtView {}

    impl WidgetImpl for ArtView {
        fn map(&self) {
            self.parent_map();
            if self.pending.borrow().is_some() {
                super::lazy::schedule();
            }
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.size.get();
            let banner = self.banner.get();
            if banner > 0 {
                let v = if orientation == gtk::Orientation::Horizontal { 0 } else { banner };
                (v, v, -1, -1)
            } else if w > 0 && h > 0 {
                let v = if orientation == gtk::Orientation::Horizontal { w } else { h };
                (v, v, -1, -1)
            } else {
                (32, self.natural.get().max(32), -1, -1)
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let (fw, fh) = self.size.get();
            let rect = if (fw > 0 && fh > 0) || self.banner.get() > 0 {
                graphene::Rect::new(0.0, 0.0, w, h)
            } else {
                let side = w.min(h);
                graphene::Rect::new((w - side) / 2.0, (h - side) / 2.0, side, side)
            };
            let radius = if self.round.get() {
                rect.width().min(rect.height()) / 2.0
            } else {
                self.radius.get()
            };
            let rounded = gsk::RoundedRect::from_rect(rect, radius);
            if self.shadow.get() {
                snapshot.append_outset_shadow(&rounded, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.45), 0.0, 10.0, 0.0, 30.0);
            }
            snapshot.push_rounded_clip(&rounded);
            match self.texture.borrow().as_ref() {
                Some(t) => {
                    let (tw, th) = (t.width().max(1) as f32, t.height().max(1) as f32);
                    let scale = (rect.width() / tw).max(rect.height() / th);
                    let (dw, dh) = (tw * scale, th * scale);
                    let dest = graphene::Rect::new(
                        rect.x() + (rect.width() - dw) / 2.0,
                        rect.y() + (rect.height() - dh) / 2.0,
                        dw,
                        dh,
                    );
                    snapshot.append_scaled_texture(t, gsk::ScalingFilter::Linear, &dest);
                }
                None => {
                    let mut c = obj.color();
                    c.set_alpha(0.1);
                    snapshot.append_color(&c, &rect);
                }
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct ArtView(ObjectSubclass<imp::ArtView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ArtView {
    /// Artwork of a fixed size.
    pub fn new(width: i32, height: i32, radius: f32) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().size.set((width, height));
        obj.imp().radius.set(radius);
        obj.set_overflow(gtk::Overflow::Hidden);
        obj
    }

    /// Artwork filling its allocation with a centred square, `natural` pixels wide when there's room.
    pub fn new_fill(natural: i32, radius: f32) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().natural.set(natural);
        obj.imp().radius.set(radius);
        obj
    }

    /// A full-width banner of the given height.
    pub fn new_banner(height: i32) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().banner.set(height);
        obj
    }

    pub fn set_banner_height(&self, height: i32) {
        if self.imp().banner.replace(height) != height {
            self.queue_resize();
        }
    }

    pub fn set_fixed_size(&self, width: i32, height: i32) {
        if self.imp().size.replace((width, height)) != (width, height) {
            self.queue_resize();
        }
    }

    pub fn set_shadow(&self, shadow: bool) {
        self.imp().shadow.set(shadow);
        self.queue_draw();
    }

    pub fn set_round(&self, round: bool) {
        if self.imp().round.replace(round) != round {
            self.queue_draw();
        }
    }

    pub fn texture(&self) -> Option<gdk::Texture> {
        self.imp().texture.borrow().clone()
    }

    /// Always load right away (Now playing, the player bar, page headers).
    pub fn set_eager(&self, eager: bool) {
        self.imp().eager.set(eager);
    }

    /// Show artwork for the widget's size: at once if it's in memory or the
    /// view is eager, otherwise once it scrolls into view.
    pub fn set_thumbnails(&self, thumbs: &[Thumbnail]) {
        let imp = self.imp();
        let (w, h) = imp.size.get();
        let (w, h) = if imp.banner.get() > 0 {
            (1080, 0)
        } else if w > 0 {
            (w, h)
        } else {
            (imp.natural.get(), imp.natural.get())
        };
        // Wide enough to cover the frame's height too (cover-cropping a 16:9
        // video thumbnail into a square must not upscale it).
        let aspect = thumbs
            .iter()
            .max_by_key(|t| t.width)
            .filter(|t| t.height > 0)
            .map(|t| t.width as f32 / t.height as f32)
            .unwrap_or(1.0);
        let needed = (w as f32).max(h as f32 * aspect).ceil().max(1.0) as u32;
        let width = image::bucket(needed * self.scale_factor().max(1) as u32);
        let Some(url) = api::thumb_url(thumbs, width) else {
            imp.pending.replace(None);
            image::set_url(self, None, 0);
            return;
        };
        if imp.eager.get() || image::cached_texture(&url, width).is_some() {
            imp.pending.replace(None);
            image::set_url(self, Some(&url), width);
        } else if image::is_wanted(self, &url, width) {
            // Already showing or loading this one.
            imp.pending.replace(None);
        } else {
            // Forget the previous image (and any load still running for it).
            image::set_url(self, None, 0);
            imp.pending.replace(Some((url, width)));
            lazy::queue(self);
        }
    }

    fn load_pending(&self) {
        if let Some((url, width)) = self.imp().pending.take() {
            image::set_url(self, Some(&url), width);
        }
    }
}

/// The lazy-loading queue: views waiting to be scrolled into view.
mod lazy {
    use super::*;

    thread_local! {
        static QUEUE: RefCell<Vec<glib::WeakRef<ArtView>>> = const { RefCell::new(Vec::new()) };
        static SCHEDULED: Cell<bool> = const { Cell::new(false) };
    }

    const WATCHED: &str = "stmusic-lazy-watched";

    pub fn queue(view: &ArtView) {
        if !view.imp().queued.replace(true) {
            QUEUE.with(|q| q.borrow_mut().push(view.downgrade()));
        }
        schedule();
    }

    /// Check the queue soon (after the current layout); calls coalesce.
    pub fn schedule() {
        if SCHEDULED.with(|s| s.replace(true)) {
            return;
        }
        glib::timeout_add_local_once(std::time::Duration::from_millis(30), || {
            SCHEDULED.with(|s| s.set(false));
            run();
        });
    }

    fn run() {
        let views: Vec<glib::WeakRef<ArtView>> = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
        let mut keep = Vec::with_capacity(views.len());
        for weak in views {
            let Some(view) = weak.upgrade() else { continue };
            if view.imp().pending.borrow().is_none() {
                view.imp().queued.set(false);
                continue;
            }
            if near_viewport(&view) {
                view.imp().queued.set(false);
                view.load_pending();
            } else {
                keep.push(weak);
            }
        }
        QUEUE.with(|q| q.borrow_mut().extend(keep));
    }

    /// On screen, or within half a screen of it, inside every scrolled window
    /// around it. Scrolled windows found on the way get watched, so scrolling
    /// them checks the queue again.
    fn near_viewport(view: &ArtView) -> bool {
        if !view.is_mapped() || view.width() <= 0 {
            return false;
        }
        let Some(root) = view.root() else { return false };
        let root = root.upcast_ref::<gtk::Widget>();
        let Some(b) = view.compute_bounds(root) else { return false };
        let grown = |r: graphene::Rect| {
            let (mx, my) = (r.width() * 0.5, r.height() * 0.5);
            graphene::Rect::new(r.x() - mx, r.y() - my, r.width() + 2.0 * mx, r.height() + 2.0 * my)
        };
        let window = graphene::Rect::new(0.0, 0.0, root.width() as f32, root.height() as f32);
        if b.intersection(&grown(window)).is_none() {
            return false;
        }
        let mut ancestor = view.parent();
        while let Some(a) = ancestor {
            if let Some(sw) = a.downcast_ref::<gtk::ScrolledWindow>() {
                watch(sw);
                if let Some(r) = sw.compute_bounds(root) {
                    if b.intersection(&grown(r)).is_none() {
                        return false;
                    }
                }
            }
            ancestor = a.parent();
        }
        true
    }

    fn watch(sw: &gtk::ScrolledWindow) {
        // SAFETY: the key only ever holds a bool.
        if unsafe { sw.data::<bool>(WATCHED).is_some() } {
            return;
        }
        unsafe { sw.set_data(WATCHED, true) };
        for adj in [sw.hadjustment(), sw.vadjustment()] {
            adj.connect_value_changed(|_| schedule());
            adj.connect_changed(|_| schedule());
        }
    }
}

impl ArtTarget for ArtView {
    fn show_texture(&self, texture: Option<&gdk::Texture>) {
        self.imp().texture.replace(texture.cloned());
        self.queue_draw();
    }

    fn has_texture(&self) -> bool {
        self.imp().texture.borrow().is_some()
    }
}

// ---------------------------------------------------------------------------
// Swatch
// ---------------------------------------------------------------------------

mod swatch_imp {
    use super::*;

    #[derive(Default)]
    pub struct Swatch {
        pub color: Cell<Option<gdk::RGBA>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Swatch {
        const NAME: &'static str = "StmSwatch";
        type Type = super::Swatch;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("swatch");
        }
    }

    impl ObjectImpl for Swatch {}

    impl WidgetImpl for Swatch {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let rect = graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
            if let Some(c) = self.color.get() {
                snapshot.append_color(&c, &rect);
            }
        }
    }
}

glib::wrapper! {
    /// A plain colour block (the stripe on mood tiles).
    pub struct Swatch(ObjectSubclass<swatch_imp::Swatch>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Swatch {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_color(&self, c: Option<gdk::RGBA>) {
        self.imp().color.set(c);
        self.queue_draw();
    }
}

impl Default for Swatch {
    fn default() -> Self {
        Self::new()
    }
}

/// 0xAARRGGBB to RGBA.
pub fn argb(c: u32) -> gdk::RGBA {
    let ch = |shift: u32| ((c >> shift) & 0xff) as f32 / 255.0;
    gdk::RGBA::new(ch(16), ch(8), ch(0), 1.0)
}

// ---------------------------------------------------------------------------
// TintBin
// ---------------------------------------------------------------------------

mod tint_imp {
    use super::*;

    #[derive(Default)]
    pub struct TintBin {
        pub from: Cell<Option<gdk::RGBA>>,
        pub to: Cell<Option<gdk::RGBA>>,
        pub progress: Cell<f64>,
        /// How far down (0..1 of the height) the tint fades out.
        pub reach: Cell<f32>,
        pub animation: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TintBin {
        const NAME: &'static str = "StmTintBin";
        type Type = super::TintBin;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("tintbin");
        }
    }

    impl ObjectImpl for TintBin {
        fn dispose(&self) {
            while let Some(c) = self.obj().first_child() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for TintBin {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (w, h) = (obj.width() as f32, obj.height() as f32);
            let t = self.progress.get() as f32;
            let mix = |a: Option<gdk::RGBA>, b: Option<gdk::RGBA>| -> Option<gdk::RGBA> {
                let clear = |c: gdk::RGBA| gdk::RGBA::new(c.red(), c.green(), c.blue(), 0.0);
                let (a, b) = match (a, b) {
                    (None, None) => return None,
                    (Some(a), None) => (a, clear(a)),
                    (None, Some(b)) => (clear(b), b),
                    (Some(a), Some(b)) => (a, b),
                };
                let l = |x: f32, y: f32| x + (y - x) * t;
                Some(gdk::RGBA::new(
                    l(a.red(), b.red()),
                    l(a.green(), b.green()),
                    l(a.blue(), b.blue()),
                    l(a.alpha(), b.alpha()),
                ))
            };
            if let Some(c) = mix(self.from.get(), self.to.get()).filter(|c| c.alpha() > 0.0) {
                let end = gdk::RGBA::new(c.red(), c.green(), c.blue(), 0.0);
                let reach = self.reach.get().max(0.1);
                snapshot.append_linear_gradient(
                    &graphene::Rect::new(0.0, 0.0, w, h),
                    &graphene::Point::new(0.0, 0.0),
                    &graphene::Point::new(0.0, h * reach),
                    &[gsk::ColorStop::new(0.0, c), gsk::ColorStop::new(1.0, end)],
                );
            }
            self.parent_snapshot(snapshot);
        }
    }
}

glib::wrapper! {
    /// A container that paints a gradient of a colour behind its child.
    pub struct TintBin(ObjectSubclass<tint_imp::TintBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TintBin {
    pub fn new(child: &impl IsA<gtk::Widget>, reach: f32) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().reach.set(reach);
        obj.imp().progress.set(1.0);
        child.set_parent(&obj);
        obj
    }

    /// Fade to a new tint (None = no tint).
    pub fn set_color(&self, color: Option<gdk::RGBA>) {
        let imp = self.imp();
        if imp.to.get() == color {
            return;
        }
        if let Some(a) = imp.animation.take() {
            a.pause();
        }
        // Start from wherever the current fade is.
        let current = {
            let t = imp.progress.get() as f32;
            match (imp.from.get(), imp.to.get()) {
                (_, None) if t >= 1.0 => None,
                (Some(a), Some(b)) => Some(gdk::RGBA::new(
                    a.red() + (b.red() - a.red()) * t,
                    a.green() + (b.green() - a.green()) * t,
                    a.blue() + (b.blue() - a.blue()) * t,
                    a.alpha() + (b.alpha() - a.alpha()) * t,
                )),
                (_, b) => b,
            }
        };
        imp.from.set(current);
        imp.to.set(color);
        imp.progress.set(0.0);
        if !self.is_mapped() {
            imp.progress.set(1.0);
            self.queue_draw();
            return;
        }
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = bin)]
            self,
            move |v| {
                bin.imp().progress.set(v);
                bin.queue_draw();
            }
        ));
        let anim = adw::TimedAnimation::new(self, 0.0, 1.0, 450, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        anim.play();
        imp.animation.replace(Some(anim));
    }
}
