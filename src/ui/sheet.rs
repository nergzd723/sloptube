//! The watch page as a sheet that can be dragged down into the mini player
//! (and back up), following the finger like the YouTube Android app.
//!
//! The sheet keeps the watch view at full size and only translates it, so a
//! drag never triggers a relayout of the details. While moving, a floating copy
//! of the video interpolates between the full player and the mini player's
//! thumbnail, so the video visibly shrinks into the bar.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Instant;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

use super::watch::WatchView;

type Rect = (f32, f32, f32, f32);

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::PlayerSheet)]
    pub struct PlayerSheet {
        pub watch: RefCell<Option<WatchView>>,
        pub floating: gtk::Picture,
        /// 0 = collapsed into the mini player, 1 = fully expanded.
        #[property(get, set = Self::set_progress_value, minimum = 0.0, maximum = 1.0)]
        pub progress: Cell<f64>,
        /// Where the sheet is heading (the settled state).
        pub target: Cell<f64>,
        pub animation: RefCell<Option<adw::SpringAnimation>>,
        pub mini_rect: RefCell<Option<Box<dyn Fn(&super::PlayerSheet) -> Option<graphene::Rect>>>>,
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
            self.floating.set_content_fit(gtk::ContentFit::Contain);
            self.floating.set_can_shrink(true);
            self.floating.add_css_class("floating-video");
            self.floating.set_can_target(false);
            self.floating.set_parent(&*self.obj());
            self.floating.set_child_visible(false);
        }

        fn dispose(&self) {
            if let Some(w) = self.watch.take() {
                w.unparent();
            }
            self.floating.unparent();
        }
    }

    impl WidgetImpl for PlayerSheet {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let min = self
                .watch
                .borrow()
                .as_ref()
                .map(|w| w.measure(orientation, for_size).0)
                .unwrap_or(0);
            (min, min, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let p = self.progress.get() as f32;
            let mini = self.mini_rect_or_default(width, height);
            let travel = mini.1.max(0.0);
            let offset = (1.0 - p) * travel;
            if let Some(watch) = self.watch.borrow().as_ref() {
                if watch.is_child_visible() {
                    let t = gsk::Transform::new().translate(&graphene::Point::new(0.0, offset));
                    watch.allocate(width, height, baseline, Some(t));
                    if let Some(frame) = watch.player_frame() {
                        if let Some(b) = frame.compute_bounds(watch) {
                            self.full_rect.set(Some((b.x(), b.y(), b.width(), b.height())));
                        }
                    }
                }
            }
            if self.floating.is_child_visible() {
                let full = self
                    .full_rect
                    .get()
                    .unwrap_or((0.0, 0.0, width as f32, width as f32 * 9.0 / 16.0));
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
            if let Some(watch) = self.watch.borrow().as_ref() {
                if watch.is_child_visible() {
                    // Details fade out on the way down; fully gone near the bar.
                    let alpha = smoothstep((p - 0.12) / 0.6);
                    if alpha < 1.0 {
                        snapshot.push_opacity(alpha);
                        obj.snapshot_child(watch, snapshot);
                        snapshot.pop();
                    } else {
                        obj.snapshot_child(watch, snapshot);
                    }
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
                _ => (8.0, (height - 64) as f32, 96.0, 54.0_f32.min(width as f32)),
            }
        }

        fn set_progress_value(&self, p: f64) {
            let p = p.clamp(0.0, 1.0);
            let was = self.progress.replace(p);
            let moving = p > 0.0 && p < 1.0;
            let obj = self.obj();
            if let Some(watch) = self.watch.borrow().as_ref() {
                watch.set_child_visible(p > 0.0);
                // The floating copy stands in for the player while moving; hiding the
                // whole frame lets the feed show around the shrinking video.
                if let Some(frame) = watch.player_frame() {
                    frame.set_opacity(if moving { 0.0 } else { 1.0 });
                }
            }
            if moving && !self.floating.is_child_visible() {
                if let Some(watch) = self.watch.borrow().as_ref() {
                    self.floating.set_paintable(watch.player_view().visual_paintable().as_ref());
                }
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
    pub fn new(watch: &WatchView) -> Self {
        let obj: Self = glib::Object::new();
        watch.set_parent(&obj);
        obj.imp().watch.replace(Some(watch.clone()));
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

    pub fn set_locked(&self, locked: bool) {
        self.imp().locked.set(locked);
    }

    fn travel(&self) -> f64 {
        let imp = self.imp();
        let r = imp.mini_rect_or_default(self.width(), self.height());
        (r.1 as f64).max(120.0)
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

    /// Drag the video down to collapse (touch, mouse or pen). `on_drag_start` runs
    /// once a press turns into a drag; `on_pull_up` when the video is flicked up.
    pub fn attach_collapse_drag(&self, widget: &impl IsA<gtk::Widget>, on_drag_start: impl Fn() + 'static, on_pull_up: impl Fn() + 'static) {
        let drag = gtk::GestureDrag::new();
        let started = std::rc::Rc::new(Cell::new(false));
        // The widget moves with the sheet, so GestureDrag's widget-relative offsets
        // would shrink by however far the sheet just moved and the drag would bounce
        // between two positions. Track the pointer in surface coordinates instead.
        let start = std::rc::Rc::new(Cell::new(None::<(f64, f64)>));
        // Offset at which the drag was recognised, so the sheet doesn't jump by the threshold.
        let grab_dy = std::rc::Rc::new(Cell::new(0.0));
        let offset = glib::clone!(
            #[strong]
            start,
            move |g: &gtk::GestureDrag, dx: f64, dy: f64| match (start.get(), g.current_event().and_then(|e| e.position())) {
                (Some((x0, y0)), Some((x, y))) => (x - x0, y - y0),
                _ => (dx, dy),
            }
        );
        let offset = std::rc::Rc::new(offset);
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
                    if dx.abs() > dy.abs() {
                        g.set_state(gtk::EventSequenceState::Denied);
                        return;
                    }
                    started.set(true);
                    grab_dy.set(dy);
                    g.set_state(gtk::EventSequenceState::Claimed);
                    on_drag_start();
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
                if !started.get() {
                    return;
                }
                started.set(false);
                let (_, dy) = offset(g, dx, dy);
                if dy < -80.0 && sheet.progress() >= 0.999 {
                    on_pull_up();
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

    /// Drag the mini player up to expand; tap expands too, a sideways swipe dismisses.
    pub fn attach_expand_drag(&self, widget: &impl IsA<gtk::Widget>, on_dismiss: impl Fn() + 'static) {
        let widget: gtk::Widget = widget.clone().upcast();
        let drag = gtk::GestureDrag::new();
        // 0 = undecided, 1 = vertical (ours), 2 = horizontal (dismiss)
        let mode = std::rc::Rc::new(Cell::new(0u8));
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
            move |_, dx, _| {
                match mode.replace(0) {
                    1 => sheet.release(),
                    2 => {
                        widget.set_opacity(1.0);
                        if dx.abs() > widget.width() as f64 * 0.35 {
                            on_dismiss();
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
            move |_, _, _, _| sheet.animate_to(1.0, 0.0)
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
