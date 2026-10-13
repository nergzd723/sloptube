//! Now playing as a sheet that follows the finger between the player bar and
//! the full page, like the YouTube Music app.
//!
//! The page keeps its full size and is only translated, so a drag never
//! relayouts it. While moving, a floating copy of the artwork interpolates
//! between the big artwork and the bar's thumbnail, and the rest of the page
//! fades.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

use super::now_playing::NowPlayingView;

type Rect = (f32, f32, f32, f32);

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn smoothstep(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

type RectProvider = Box<dyn Fn(&super::sheet::PlayerSheet) -> Option<graphene::Rect>>;

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::PlayerSheet)]
    pub struct PlayerSheet {
        pub view: RefCell<Option<NowPlayingView>>,
        pub floating: gtk::Picture,
        /// 0 = collapsed into the bar, 1 = fully open.
        #[property(get, set = Self::set_progress_value, minimum = 0.0, maximum = 1.0)]
        pub progress: Cell<f64>,
        pub target: Cell<f64>,
        pub animation: RefCell<Option<adw::SpringAnimation>>,
        pub mini_rect: RefCell<Option<RectProvider>>,
        pub drag_origin: Cell<f64>,
        pub samples: RefCell<VecDeque<(Instant, f64)>>,
        pub full_rect: Cell<Option<Rect>>,
        pub on_settled: RefCell<Vec<Box<dyn Fn(&super::PlayerSheet, bool)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerSheet {
        const NAME: &'static str = "StmPlayerSheet";
        type Type = super::PlayerSheet;
        type ParentType = gtk::Widget;
    }

    #[glib::derived_properties]
    impl ObjectImpl for PlayerSheet {
        fn constructed(&self) {
            self.parent_constructed();
            self.floating.set_content_fit(gtk::ContentFit::Cover);
            self.floating.set_can_shrink(true);
            self.floating.add_css_class("floating-art");
            self.floating.set_overflow(gtk::Overflow::Hidden);
            self.floating.set_can_target(false);
            self.floating.set_parent(&*self.obj());
            self.floating.set_child_visible(false);
        }

        fn dispose(&self) {
            if let Some(v) = self.view.take() {
                v.unparent();
            }
            self.floating.unparent();
        }
    }

    impl WidgetImpl for PlayerSheet {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let min = self
                .view
                .borrow()
                .as_ref()
                .map(|v| v.measure(orientation, for_size).0)
                .unwrap_or(0);
            (min, min, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let p = self.progress.get() as f32;
            let mini = self.mini_rect_or_default(width, height);
            let travel = self.travel_for(height, &mini);
            let offset = (1.0 - p) * travel;
            if let Some(view) = self.view.borrow().as_ref()
                && view.is_child_visible() {
                    let t = gsk::Transform::new().translate(&graphene::Point::new(0.0, offset));
                    view.allocate(width, height, baseline, Some(t));
                    if let Some(r) = view.art_rect_in(view) {
                        self.full_rect.set(Some((r.x(), r.y(), r.width(), r.height())));
                    }
                }
            if self.floating.is_child_visible() {
                let full = self
                    .full_rect
                    .get()
                    .unwrap_or((24.0, 80.0, (width - 48) as f32, (width - 48) as f32));
                // The full rect moves with the page.
                let full = (full.0, full.1 + offset, full.2, full.3);
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
            if let Some(view) = self.view.borrow().as_ref()
                && view.is_child_visible() {
                    let alpha = smoothstep((p - 0.1) / 0.6);
                    if alpha < 1.0 {
                        snapshot.push_opacity(alpha);
                        obj.snapshot_child(view, snapshot);
                        snapshot.pop();
                    } else {
                        obj.snapshot_child(view, snapshot);
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
                _ => (12.0, (height - 60) as f32, 44.0, 44.0_f32.min(width as f32)),
            }
        }

        /// How far the page moves: down to the bar, or off the bottom if the bar is below.
        pub fn travel_for(&self, height: i32, mini: &Rect) -> f32 {
            if mini.1 > 0.0 && mini.1 < height as f32 { mini.1 } else { height as f32 }.max(120.0)
        }

        fn set_progress_value(&self, p: f64) {
            let p = p.clamp(0.0, 1.0);
            let was = self.progress.replace(p);
            let moving = p > 0.0 && p < 1.0;
            let obj = self.obj();
            if let Some(view) = self.view.borrow().as_ref() {
                view.set_child_visible(p > 0.0);
                view.art().set_opacity(if moving { 0.0 } else { 1.0 });
            }
            if moving && !self.floating.is_child_visible() {
                let tex = self.view.borrow().as_ref().and_then(|v| v.art().texture());
                self.floating.set_paintable(tex.as_ref());
            }
            if !moving {
                self.floating.set_paintable(None::<&gtk::gdk::Paintable>);
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
    pub fn new(view: &NowPlayingView) -> Self {
        let obj: Self = glib::Object::new();
        view.set_parent(&obj);
        obj.imp().view.replace(Some(view.clone()));
        obj.set_progress(0.0);
        obj
    }

    /// Bounds of the bar's artwork in this widget's coordinates.
    pub fn set_mini_rect_provider(&self, f: impl Fn(&PlayerSheet) -> Option<graphene::Rect> + 'static) {
        self.imp().mini_rect.replace(Some(Box::new(f)));
    }

    /// Called whenever the sheet comes to rest (`true` = open).
    pub fn connect_settled(&self, f: impl Fn(&PlayerSheet, bool) + 'static) {
        self.imp().on_settled.borrow_mut().push(Box::new(f));
    }

    pub fn target(&self) -> f64 {
        self.imp().target.get()
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

    fn settled(&self, open: bool) {
        log::debug!("sheet: settled open={open}");
        for cb in self.imp().on_settled.borrow().iter() {
            cb(self, open);
        }
    }

    /// Animate to 0 (bar) or 1 (open). `velocity` is in progress per second.
    pub fn animate_to(&self, to: f64, velocity: f64) {
        log::debug!("sheet: animate {:.2} -> {to} (mapped {}, {}x{})", self.progress(), self.is_mapped(), self.width(), self.height());
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
        let open = if v > 1.4 {
            true
        } else if v < -1.4 {
            false
        } else if self.imp().drag_origin.get() >= 0.5 {
            p > 0.66
        } else {
            p > 0.25
        };
        self.animate_to(if open { 1.0 } else { 0.0 }, v);
    }

    /// Drag the open page down to close it. The page moves with the pointer, so
    /// it is tracked in surface coordinates (widget-relative offsets would shrink
    /// as the page moves and make it bounce).
    pub fn attach_collapse_drag(&self, widget: &impl IsA<gtk::Widget>) {
        let drag = gtk::GestureDrag::new();
        let started = Rc::new(Cell::new(false));
        let start = Rc::new(Cell::new(None::<(f64, f64)>));
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
                let (dx, dy) = offset(g, dx, dy);
                if !started.get() {
                    if dx.abs().max(dy.abs()) < 12.0 {
                        return;
                    }
                    if dx.abs() > dy.abs() || dy < 0.0 {
                        g.set_state(gtk::EventSequenceState::Denied);
                        return;
                    }
                    started.set(true);
                    grab_dy.set(dy);
                    g.set_state(gtk::EventSequenceState::Claimed);
                    sheet.begin_drag();
                }
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
                if started.replace(false) {
                    sheet.release();
                    return;
                }
                // A flick down too fast for any motion in between.
                let (dx, dy) = offset(g, dx, dy);
                if dy > 60.0 && dy > dx.abs() {
                    sheet.animate_to(0.0, 0.0);
                }
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

    /// Drag the mini player up to open; tap opens too; a sideways swipe calls
    /// `on_swipe` with -1 (left) or 1 (right).
    pub fn attach_expand_drag(&self, widget: &impl IsA<gtk::Widget>, on_swipe: impl Fn(i32) + 'static) {
        let widget: gtk::Widget = widget.clone().upcast();
        let drag = gtk::GestureDrag::new();
        // 0 = undecided, 1 = vertical, 2 = horizontal
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
                        widget.set_opacity((1.0 - dx.abs() / w * 1.4).clamp(0.25, 1.0));
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
            move |_, dx, dy| match mode.replace(0) {
                1 => sheet.release(),
                2 => {
                    widget.set_opacity(1.0);
                    if dx.abs() > widget.width() as f64 * 0.25 {
                        on_swipe(if dx < 0.0 { -1 } else { 1 });
                    }
                }
                // A flick too fast for any motion in between.
                _ if dy < -40.0 && dy.abs() > dx.abs() => sheet.animate_to(1.0, 0.0),
                _ if dx.abs() > widget.width() as f64 * 0.25 => on_swipe(if dx < 0.0 { -1 } else { 1 }),
                _ => {}
            }
        ));
        widget.add_controller(drag);

        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = sheet)]
            self,
            move |g, _, x, y| {
                // Buttons in the bar handle their own clicks.
                if let Some(w) = g.widget() {
                    if x < 0.0 || y < 0.0 || x > w.width() as f64 || y > w.height() as f64 {
                        return;
                    }
                    if let Some(hit) = w.pick(x, y, gtk::PickFlags::DEFAULT)
                        && hit.ancestor(gtk::Button::static_type()).is_some() {
                            return;
                        }
                }
                sheet.animate_to(1.0, 0.0);
            }
        ));
        widget.add_controller(click);
    }
}

/// Bounds of `widget` relative to `target`.
pub fn bounds_in(widget: &impl IsA<gtk::Widget>, target: &impl IsA<gtk::Widget>) -> Option<graphene::Rect> {
    if !widget.is_mapped() {
        return None;
    }
    widget.compute_bounds(target)
}
