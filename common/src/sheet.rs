//! The player page as a sheet that can be dragged down into the mini player
//! (and back up), following the finger like the YouTube and YouTube Music
//! apps. SlopTube's watch page and Music's Now playing both use it.
//!
//! The sheet keeps the page at full size and only translates it, so a drag
//! never triggers a relayout. While moving, a floating copy of the page's
//! "hero" (the video, the artwork) interpolates between its place on the page
//! and the mini player's thumbnail, so it visibly shrinks into the bar.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// How far (px) the mini player must be dragged up before letting go opens it.
const MIN_EXPAND_DRAG: f64 = 64.0;

type Rect = (f32, f32, f32, f32);

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// What shrinks into the mini player: the video frame, the artwork.
pub struct Hero {
    /// Where the hero is drawn, in the page's coordinates.
    pub rect: Box<dyn Fn() -> Option<graphene::Rect>>,
    /// The widget the floating copy stands in for while the sheet moves.
    pub widget: Box<dyn Fn() -> Option<gtk::Widget>>,
    /// What the floating copy shows.
    pub paintable: Box<dyn Fn() -> Option<gdk::Paintable>>,
    /// How the floating copy fills its box: `Contain` for video, `Cover` for artwork.
    pub fit: gtk::ContentFit,
    /// The floating copy's CSS class.
    pub css_class: &'static str,
    /// Width / height, for the rects used before the real ones are known.
    pub aspect: f32,
}

type RectProvider = Box<dyn Fn(&PlayerSheet) -> Option<graphene::Rect>>;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::PlayerSheet)]
    pub struct PlayerSheet {
        pub page: RefCell<Option<gtk::Widget>>,
        pub hero: RefCell<Option<Hero>>,
        pub floating: gtk::Picture,
        /// 0 = collapsed into the mini player, 1 = fully expanded.
        #[property(get, set = Self::set_progress_value, minimum = 0.0, maximum = 1.0)]
        pub progress: Cell<f64>,
        /// Where the sheet is heading (the settled state).
        pub target: Cell<f64>,
        pub animation: RefCell<Option<adw::SpringAnimation>>,
        pub mini_rect: RefCell<Option<RectProvider>>,
        pub drag_origin: Cell<f64>,
        pub samples: RefCell<VecDeque<(Instant, f64)>>,
        pub locked: Cell<bool>,
        pub full_rect: Cell<Option<Rect>>,
        pub on_settled: RefCell<Vec<Box<dyn Fn(&super::PlayerSheet, bool)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerSheet {
        const NAME: &'static str = "SlopPlayerSheet";
        type Type = super::PlayerSheet;
        type ParentType = gtk::Widget;
    }

    #[glib::derived_properties]
    impl ObjectImpl for PlayerSheet {
        fn constructed(&self) {
            self.parent_constructed();
            self.floating.set_can_shrink(true);
            self.floating.set_overflow(gtk::Overflow::Hidden);
            self.floating.set_can_target(false);
            self.floating.set_parent(&*self.obj());
            self.floating.set_child_visible(false);
        }

        fn dispose(&self) {
            if let Some(p) = self.page.take() {
                p.unparent();
            }
            self.floating.unparent();
        }
    }

    impl WidgetImpl for PlayerSheet {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let min = self
                .page
                .borrow()
                .as_ref()
                .map(|p| p.measure(orientation, for_size).0)
                .unwrap_or(0);
            (min, min, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let p = self.progress.get() as f32;
            let mini = self.mini_rect_or_default(width, height);
            let offset = (1.0 - p) * self.travel_for(height, &mini);
            if let Some(page) = self.page.borrow().as_ref()
                && page.is_child_visible()
            {
                let t = gsk::Transform::new().translate(&graphene::Point::new(0.0, offset));
                page.allocate(width, height, baseline, Some(t));
                if let Some(r) = self.hero.borrow().as_ref().and_then(|h| (h.rect)()) {
                    self.full_rect.set(Some((r.x(), r.y(), r.width(), r.height())));
                }
            }
            if self.floating.is_child_visible() {
                let aspect = self.hero.borrow().as_ref().map_or(16.0 / 9.0, |h| h.aspect);
                let full = self
                    .full_rect
                    .get()
                    .unwrap_or((0.0, 0.0, width as f32, width as f32 / aspect));
                let x = lerp(mini.0, full.0, p);
                let y = lerp(mini.1, full.1, p);
                let w = lerp(mini.2, full.2, p).max(1.0);
                let h = lerp(mini.3, full.3, p).max(1.0);
                let t = gsk::Transform::new().translate(&graphene::Point::new(x, y));
                self.floating.allocate(w as i32, h as i32, -1, Some(t));
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let p = self.progress.get();
            if p <= 0.0 {
                return;
            }
            let obj = self.obj();
            if let Some(page) = self.page.borrow().as_ref()
                && page.is_child_visible()
            {
                // The page fades out on the way down; fully gone near the bar.
                let alpha = smoothstep((p - 0.12) / 0.6);
                if alpha < 1.0 {
                    snapshot.push_opacity(alpha);
                    obj.snapshot_child(page, snapshot);
                    snapshot.pop();
                } else {
                    obj.snapshot_child(page, snapshot);
                }
            }
            if self.floating.is_child_visible() {
                obj.snapshot_child(&self.floating, snapshot);
            }
        }
    }

    impl PlayerSheet {
        pub fn mini_rect_or_default(&self, width: i32, height: i32) -> Rect {
            let obj = self.obj();
            let r = self.mini_rect.borrow().as_ref().and_then(|f| f(&obj));
            match r {
                Some(r) if r.width() > 1.0 => (r.x(), r.y(), r.width(), r.height()),
                _ => {
                    let aspect = self.hero.borrow().as_ref().map_or(16.0 / 9.0, |h| h.aspect);
                    (8.0, (height - 64) as f32, (54.0 * aspect).min(width as f32), 54.0)
                }
            }
        }

        /// How far the page moves: down to the mini player, or off the bottom
        /// when the player bar is below the sheet (Music's desktop layout).
        pub fn travel_for(&self, height: i32, mini: &Rect) -> f32 {
            if mini.1 > 0.0 && mini.1 < height as f32 { mini.1 } else { height as f32 }.max(120.0)
        }

        fn set_progress_value(&self, p: f64) {
            let p = p.clamp(0.0, 1.0);
            let was = self.progress.replace(p);
            let moving = p > 0.0 && p < 1.0;
            let obj = self.obj();
            if let Some(page) = self.page.borrow().as_ref() {
                page.set_child_visible(p > 0.0);
            }
            if let Some(hero) = self.hero.borrow().as_ref() {
                // The floating copy stands in for the hero while moving; hiding the
                // hero lets what is behind the page show around the shrinking copy.
                if let Some(w) = (hero.widget)() {
                    w.set_opacity(if moving { 0.0 } else { 1.0 });
                }
                if moving && !self.floating.is_child_visible() {
                    self.floating.set_paintable((hero.paintable)().as_ref());
                }
            }
            if !moving {
                self.floating.set_paintable(None::<&gdk::Paintable>);
            }
            self.floating.set_child_visible(moving);
            obj.set_can_target(p > 0.0);
            obj.queue_allocate();
            obj.queue_draw();
            if (was - p).abs() > f64::EPSILON {
                obj.notify_progress();
            }
        }
    }
}

glib::wrapper! {
    pub struct PlayerSheet(ObjectSubclass<imp::PlayerSheet>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PlayerSheet {
    pub fn new(page: &impl IsA<gtk::Widget>, hero: Hero) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.floating.set_content_fit(hero.fit);
        imp.floating.add_css_class(hero.css_class);
        imp.hero.replace(Some(hero));
        page.set_parent(&obj);
        imp.page.replace(Some(page.clone().upcast()));
        obj.set_progress(0.0);
        obj
    }

    /// Bounds of the mini player's thumbnail in this widget's coordinates.
    pub fn set_mini_rect_provider(&self, f: impl Fn(&PlayerSheet) -> Option<graphene::Rect> + 'static) {
        self.imp().mini_rect.replace(Some(Box::new(f)));
    }

    /// Called whenever the sheet comes to rest (`true` = expanded).
    pub fn connect_settled(&self, f: impl Fn(&PlayerSheet, bool) + 'static) {
        self.imp().on_settled.borrow_mut().push(Box::new(f));
    }

    pub fn target(&self) -> f64 {
        self.imp().target.get()
    }

    /// Ignore collapse drags (e.g. while fullscreen).
    pub fn set_locked(&self, locked: bool) {
        self.imp().locked.set(locked);
    }

    fn travel(&self) -> f64 {
        let imp = self.imp();
        let r = imp.mini_rect_or_default(self.width(), self.height());
        imp.travel_for(self.height(), &r) as f64
    }

    fn stop_animation(&self) {
        if let Some(a) = self.imp().animation.take() {
            a.pause();
        }
    }

    fn settled(&self, expanded: bool) {
        log::debug!("sheet settled: {}", if expanded { "expanded" } else { "collapsed" });
        for cb in self.imp().on_settled.borrow().iter() {
            cb(self, expanded);
        }
    }

    /// Animate to 0 (mini player) or 1 (expanded). `velocity` is in progress/s.
    pub fn animate_to(&self, to: f64, velocity: f64) {
        let imp = self.imp();
        imp.target.set(to);
        self.stop_animation();
        let from = self.progress();
        if (from - to).abs() < 0.001 {
            self.set_progress(to);
            self.settled(to >= 1.0);
            return;
        }
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            move |v| sheet.set_progress(v)
        ));
        let anim = adw::SpringAnimation::builder()
            .widget(self)
            .value_from(from)
            .value_to(to)
            .spring_params(&adw::SpringParams::new(0.92, 1.0, 520.0))
            .initial_velocity(velocity)
            .clamp(true)
            .target(&target)
            .build();
        anim.connect_done(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            move |_| {
                sheet.imp().animation.take();
                sheet.set_progress(to);
                sheet.settled(to >= 1.0);
            }
        ));
        imp.animation.replace(Some(anim.clone()));
        anim.play();
    }

    fn begin_drag(&self) {
        log::debug!("sheet drag started at {:.2}", self.progress());
        self.stop_animation();
        let imp = self.imp();
        imp.drag_origin.set(self.progress());
        imp.samples.borrow_mut().clear();
    }

    fn drag_to(&self, progress: f64) {
        let p = progress.clamp(0.0, 1.0);
        self.set_progress(p);
        let mut s = self.imp().samples.borrow_mut();
        s.push_back((Instant::now(), p));
        while s.len() > 2 && s.front().is_some_and(|(t, _)| t.elapsed().as_millis() > 120) {
            s.pop_front();
        }
    }

    /// Progress per second over the last ~100 ms of the drag.
    fn velocity(&self) -> f64 {
        let s = self.imp().samples.borrow();
        match (s.front(), s.back()) {
            (Some((t0, p0)), Some((t1, p1))) if t1 > t0 => {
                let dt = t1.duration_since(*t0).as_secs_f64();
                if dt > 0.005 { (p1 - p0) / dt } else { 0.0 }
            }
            _ => 0.0,
        }
    }

    fn release(&self) {
        let p = self.progress();
        let v = self.velocity();
        let expand = if v > 1.4 {
            true
        } else if v < -1.4 {
            false
        } else {
            // From the top you need to pull a third of the way; from the bottom, a quarter.
            if self.imp().drag_origin.get() >= 0.5 { p > 0.66 } else { p > 0.25 }
        };
        log::debug!("sheet released at {p:.2} (velocity {v:.2}/s) -> {}", if expand { "expand" } else { "collapse" });
        self.animate_to(if expand { 1.0 } else { 0.0 }, v);
    }

    /// Drag `widget` down to collapse (touch, mouse or pen). `on_drag_start` runs
    /// once a press turns into a drag. With `on_pull_up`, flicking up from the
    /// expanded sheet calls it; without, drags that start upwards are left to
    /// `widget` (e.g. to scroll it).
    pub fn attach_collapse_drag(
        &self,
        widget: &impl IsA<gtk::Widget>,
        on_drag_start: Option<Box<dyn Fn()>>,
        on_pull_up: Option<Box<dyn Fn()>>,
    ) {
        let drag = gtk::GestureDrag::new();
        let started = Rc::new(Cell::new(false));
        // The widget moves with the sheet, so GestureDrag's widget-relative offsets
        // would shrink by however far the sheet just moved and the drag would bounce
        // between two positions. Track the pointer in surface coordinates instead.
        let start = Rc::new(Cell::new(None::<(f64, f64)>));
        // Offset at which the drag was recognised, so the sheet doesn't jump by the threshold.
        let grab_dy = Rc::new(Cell::new(0.0));
        let offset = {
            let start = start.clone();
            Rc::new(move |g: &gtk::GestureDrag, dx: f64, dy: f64| {
                match (start.get(), g.current_event().and_then(|e| e.position())) {
                    (Some((x0, y0)), Some((x, y))) => (x - x0, y - y0),
                    _ => (dx, dy),
                }
            })
        };
        let pull_up = on_pull_up.is_some();
        drag.connect_drag_begin(glib::clone!(
            #[strong]
            started,
            #[strong]
            start,
            move |g, _, _| {
                started.set(false);
                start.set(g.current_event().and_then(|e| e.position()));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            #[strong]
            started,
            #[strong]
            grab_dy,
            #[strong]
            offset,
            move |g, dx, dy| {
                if sheet.imp().locked.get() {
                    return;
                }
                let (dx, dy) = offset(g, dx, dy);
                if !started.get() {
                    if dx.abs().max(dy.abs()) < 10.0 {
                        return;
                    }
                    if dx.abs() > dy.abs() || (dy < 0.0 && !pull_up) {
                        g.set_state(gtk::EventSequenceState::Denied);
                        return;
                    }
                    started.set(true);
                    grab_dy.set(dy);
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(f) = &on_drag_start {
                        f();
                    }
                    sheet.begin_drag();
                }
                // drag_to clamps, so pulling back above the start just stays expanded.
                sheet.drag_to(sheet.imp().drag_origin.get() - (dy - grab_dy.get()) / sheet.travel());
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            #[strong]
            started,
            #[strong]
            offset,
            move |g, dx, dy| {
                let (dx, dy) = offset(g, dx, dy);
                if !started.replace(false) {
                    // A flick down too fast for any motion in between.
                    if !sheet.imp().locked.get() && dy > 60.0 && dy > dx.abs() {
                        sheet.animate_to(0.0, 0.0);
                    }
                    return;
                }
                if let Some(f) = &on_pull_up
                    && dy < -80.0
                    && sheet.progress() >= 0.999
                {
                    f();
                    return;
                }
                sheet.release();
            }
        ));
        drag.connect_cancel(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            #[strong]
            started,
            move |_, _| {
                if started.replace(false) {
                    sheet.release();
                }
            }
        ));
        widget.add_controller(drag);
    }

    /// Drag the mini player up to expand; tap expands too. A sideways swipe calls
    /// `on_swipe` with -1 (left) or 1 (right).
    pub fn attach_expand_drag(&self, widget: &impl IsA<gtk::Widget>, on_swipe: impl Fn(i32) + 'static) {
        let widget: gtk::Widget = widget.clone().upcast();
        let drag = gtk::GestureDrag::new();
        // 0 = undecided, 1 = vertical (ours), 2 = horizontal (swipe)
        let mode = Rc::new(Cell::new(0u8));
        drag.connect_drag_begin(glib::clone!(
            #[strong]
            mode,
            move |_, _, _| mode.set(0)
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            #[weak]
            widget,
            #[strong]
            mode,
            move |g, dx, dy| {
                if mode.get() == 0 {
                    if dx.abs().max(dy.abs()) < 8.0 {
                        return;
                    }
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if dy.abs() >= dx.abs() {
                        mode.set(1);
                        sheet.begin_drag();
                    } else {
                        mode.set(2);
                    }
                }
                match mode.get() {
                    1 => sheet.drag_to(sheet.imp().drag_origin.get() - dy / sheet.travel()),
                    2 => {
                        let w = widget.width().max(1) as f64;
                        widget.set_opacity((1.0 - dx.abs() / w * 1.5).clamp(0.2, 1.0));
                    }
                    _ => {}
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            #[weak]
            widget,
            #[strong]
            mode,
            move |_, dx, dy| {
                match mode.replace(0) {
                    // A short flick that starts on the bar is usually meant to scroll
                    // the page above it, and even a short one is fast enough to count
                    // as a fling, so only a deliberate drag opens the player.
                    1 if -dy < MIN_EXPAND_DRAG => sheet.animate_to(0.0, 0.0),
                    1 => sheet.release(),
                    2 => {
                        widget.set_opacity(1.0);
                        if dx.abs() > widget.width() as f64 * 0.35 {
                            on_swipe(if dx < 0.0 { -1 } else { 1 });
                        }
                    }
                    _ => {}
                }
            }
        ));
        widget.add_controller(drag);

        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            move |g, _, x, y| {
                // Only a release on the bar counts, and buttons in it handle their own clicks.
                if let Some(w) = g.widget() {
                    if x < 0.0 || y < 0.0 || x > w.width() as f64 || y > w.height() as f64 {
                        return;
                    }
                    if let Some(hit) = w.pick(x, y, gtk::PickFlags::DEFAULT)
                        && hit.ancestor(gtk::Button::static_type()).is_some()
                    {
                        return;
                    }
                }
                sheet.animate_to(1.0, 0.0);
            }
        ));
        widget.add_controller(click);
    }
}

/// Bounds of `widget` relative to `target` (helper for the mini rect provider).
pub fn bounds_in(widget: &impl IsA<gtk::Widget>, target: &impl IsA<gtk::Widget>) -> Option<graphene::Rect> {
    if !widget.is_mapped() {
        return None;
    }
    widget.compute_bounds(target)
}
