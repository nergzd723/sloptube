//! A page header that slides away once its list is scrolled down, without resizing the list.
//!
//! Collapsing the header with a `gtk::Revealer` grows the list's viewport on every
//! animation frame. `GtkListBase` keeps its scroll anchor at a fraction of the
//! viewport, so each resize moves the scroll position, and the scrolled window's
//! drag or kinetic scroll moves it straight back on the next frame. The cards
//! jitter for the whole animation. Here the list keeps its collapsed height the
//! whole time; while the header is shown, the list's bottom edge is pushed past
//! the bottom of the page instead.
//!
//! That hidden strip is only safe if scrolling down far enough to see it also
//! collapses the header. Lists too short for that keep a plain, always visible
//! header.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

/// Scroll offset under which a collapsed header comes back.
const REVEAL_AT: f64 = 4.0;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CollapsingHeader {
        pub header: RefCell<Option<gtk::Widget>>,
        /// Stays put when the header collapses (e.g. a tab row).
        pub bar: RefCell<Option<gtk::Widget>>,
        pub body: RefCell<Option<gtk::Widget>>,
        /// Scroll offset past which the header collapses.
        pub collapse_at: Cell<f64>,
        pub adjustment: RefCell<Option<(gtk::Adjustment, Vec<glib::SignalHandlerId>)>>,
        /// 0 = header fully shown, 1 = fully collapsed.
        pub progress: Cell<f64>,
        pub collapsed: Cell<bool>,
        /// The list is long enough to collapse the header over.
        pub long: Cell<bool>,
        pub animation: RefCell<Option<adw::TimedAnimation>>,
        /// Heights from the last allocation: page, header, bar.
        pub sizes: Cell<(i32, i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollapsingHeader {
        const NAME: &'static str = "SlopCollapsingHeader";
        type Type = super::CollapsingHeader;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CollapsingHeader {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            self.obj().set_adjustment(None);
            for c in [self.header.take(), self.bar.take(), self.body.take()].into_iter().flatten() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for CollapsingHeader {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (mut min, mut nat) = (0, 0);
            for (i, c) in self.children().iter().enumerate() {
                let (cmin, cnat, _, _) = c.measure(orientation, for_size);
                if orientation == gtk::Orientation::Horizontal {
                    min = min.max(cmin);
                    nat = nat.max(cnat);
                } else {
                    // The header may be clipped; the rest has to fit.
                    if i > 0 || self.header.borrow().is_none() {
                        min += cmin;
                    }
                    nat += cnat;
                }
            }
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let natural = |w: &Option<gtk::Widget>| w.as_ref().map(|w| w.measure(gtk::Orientation::Vertical, width).1);
            let hh = natural(&self.header.borrow()).unwrap_or(0);
            let bh = natural(&self.bar.borrow()).unwrap_or(0);
            self.sizes.set((height, hh, bh));
            self.update_long();

            let long = self.long.get();
            let offset = if long { (self.progress.get() * hh as f64).round() as i32 } else { 0 };
            if let Some(h) = self.header.borrow().as_ref() {
                h.set_child_visible(offset < hh);
                h.allocate(width, hh, -1, Some(down(-offset as f32)));
            }
            if let Some(b) = self.bar.borrow().as_ref() {
                b.allocate(width, bh, -1, Some(down((hh - offset) as f32)));
            }
            if let Some(b) = self.body.borrow().as_ref() {
                // Long lists always get the collapsed height: see the module docs.
                let body_h = if long { height - bh } else { height - hh - bh }.max(0);
                let y = (hh + bh - offset) as f32;
                b.allocate(width, body_h, -1, Some(down(y)));
            }
        }
    }

    impl CollapsingHeader {
        pub fn children(&self) -> Vec<gtk::Widget> {
            [&self.header, &self.bar, &self.body]
                .into_iter()
                .filter_map(|c| c.borrow().clone())
                .collect()
        }

        /// Whether the list is long enough that scrolling to its end collapses the header.
        pub fn wants_long(&self) -> bool {
            let (height, hh, bh) = self.sizes.get();
            let Some(upper) = self.adjustment.borrow().as_ref().map(|(a, _)| a.upper()) else {
                return false;
            };
            hh > 0 && height > 0 && upper - (height - bh) as f64 >= hh as f64 + self.collapse_at.get()
        }

        /// Re-evaluates the mode; returns whether it changed.
        pub fn update_long(&self) -> bool {
            let long = self.wants_long();
            if long == self.long.get() {
                return false;
            }
            self.long.set(long);
            if !long {
                if let Some(a) = self.animation.take() {
                    a.pause();
                }
                self.collapsed.set(false);
                self.progress.set(0.0);
            }
            true
        }
    }
}

fn down(y: f32) -> gtk::gsk::Transform {
    gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(0.0, y))
}

glib::wrapper! {
    pub struct CollapsingHeader(ObjectSubclass<imp::CollapsingHeader>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CollapsingHeader {
    /// `body` holds the scrolled list; point [`Self::set_adjustment`] at its vertical adjustment.
    pub fn new(
        header: &impl IsA<gtk::Widget>,
        bar: Option<&gtk::Widget>,
        body: &impl IsA<gtk::Widget>,
        collapse_at: f64,
    ) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.collapse_at.set(collapse_at);
        let header = header.clone().upcast::<gtk::Widget>();
        let body = body.clone().upcast::<gtk::Widget>();
        for c in [Some(&header), bar, Some(&body)].into_iter().flatten() {
            c.set_parent(&obj);
        }
        imp.header.replace(Some(header));
        imp.bar.replace(bar.cloned());
        imp.body.replace(Some(body));
        obj
    }

    /// Follows the scroll position of `adj` (e.g. the visible tab's list).
    pub fn set_adjustment(&self, adj: Option<&gtk::Adjustment>) {
        let imp = self.imp();
        if let Some((old, ids)) = imp.adjustment.take() {
            for id in ids {
                old.disconnect(id);
            }
        }
        let Some(adj) = adj else { return };
        let on_value = adj.connect_value_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |adj| this.follow(adj.value())
        ));
        let on_changed = adj.connect_changed(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                // This fires while the list is being allocated; lay out again afterwards.
                if this.imp().wants_long() != this.imp().long.get() {
                    glib::idle_add_local_once(glib::clone!(
                        #[weak]
                        this,
                        move || this.queue_allocate()
                    ));
                }
            }
        ));
        imp.adjustment.replace(Some((adj.clone(), vec![on_value, on_changed])));
        if imp.update_long() {
            self.queue_allocate();
        }
        self.follow(adj.value());
    }

    fn follow(&self, value: f64) {
        let imp = self.imp();
        if !imp.long.get() {
            return;
        }
        if value > imp.collapse_at.get() && !imp.collapsed.get() {
            self.animate_to(true);
        } else if value < REVEAL_AT && imp.collapsed.get() {
            self.animate_to(false);
        }
    }

    fn animate_to(&self, collapsed: bool) {
        let imp = self.imp();
        imp.collapsed.set(collapsed);
        if let Some(a) = imp.animation.take() {
            a.pause();
        }
        let target = adw::CallbackAnimationTarget::new(glib::clone!(
            #[weak(rename_to = this)]
            self,
            move |v| {
                this.imp().progress.set(v);
                this.queue_allocate();
            }
        ));
        let to = if collapsed { 1.0 } else { 0.0 };
        let anim = adw::TimedAnimation::new(self, imp.progress.get(), to, 250, target);
        anim.set_easing(adw::Easing::EaseOutCubic);
        anim.play();
        imp.animation.replace(Some(anim));
    }
}
