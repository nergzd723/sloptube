//! A single-child container that keeps a fixed aspect ratio (height-for-width).

use std::cell::{Cell, RefCell};

use gtk::glib;
use adw::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use super::*;

    pub struct AspectBin {
        pub child: RefCell<Option<gtk::Widget>>,
        /// width / height
        pub ratio: Cell<f64>,
        /// Upper bound for the requested height (-1 = none).
        pub max_height: Cell<i32>,
        /// Report a minimum height of 0 so parents can squeeze it (letterboxing).
        pub shrinkable: Cell<bool>,
    }

    impl Default for AspectBin {
        fn default() -> Self {
            Self {
                child: RefCell::new(None),
                ratio: Cell::new(16.0 / 9.0),
                max_height: Cell::new(-1),
                shrinkable: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AspectBin {
        const NAME: &'static str = "SlopAspectBin";
        type Type = super::AspectBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for AspectBin {
        fn dispose(&self) {
            if let Some(c) = self.child.take() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for AspectBin {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let child = self.child.borrow();
            match orientation {
                gtk::Orientation::Horizontal => {
                    // Width comes from the container; never let e.g. a loaded
                    // thumbnail's pixel size leak into the layout.
                    let min = child.as_ref().map(|c| c.measure(orientation, -1).0).unwrap_or(0);
                    (min, min, -1, -1)
                }
                _ => {
                    let child_min = child
                        .as_ref()
                        .map(|c| c.measure(orientation, for_size).0)
                        .unwrap_or(0);
                    // A fixed width (size request) means a fixed height: containers may
                    // ask "how tall at 540 px?" while planning, but we'll only ever get
                    // our requested width. Otherwise follow the offered width. Never use
                    // the child's natural height: for a video that's its pixel size.
                    let fixed = self.obj().width_request();
                    let width = if fixed > 0 {
                        fixed
                    } else if for_size > 0 {
                        for_size
                    } else {
                        0
                    };
                    let mut h = if width > 0 {
                        (width as f64 / self.ratio.get()).round() as i32
                    } else {
                        child_min
                    };
                    let max = self.max_height.get();
                    if max > 0 {
                        h = h.min(max);
                    }
                    let h = h.max(child_min);
                    let min = if self.shrinkable.get() { child_min.min(h) } else { h };
                    (min, h, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(c) = self.child.borrow().as_ref() {
                c.allocate(width, height, baseline, None);
            }
        }
    }
}

glib::wrapper! {
    pub struct AspectBin(ObjectSubclass<imp::AspectBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AspectBin {
    pub fn new(child: &impl IsA<gtk::Widget>, ratio: f64) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().ratio.set(ratio);
        obj.set_child(Some(child));
        obj
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk::Widget>>) {
        if let Some(old) = self.imp().child.take() {
            old.unparent();
        }
        if let Some(c) = child {
            c.set_parent(self);
            self.imp().child.replace(Some(c.clone().upcast()));
        }
        self.queue_resize();
    }

    pub fn set_shrinkable(&self, shrinkable: bool) {
        self.imp().shrinkable.set(shrinkable);
        self.queue_resize();
    }

    pub fn set_max_height(&self, h: i32) {
        if self.imp().max_height.get() != h {
            self.imp().max_height.set(h);
            self.queue_resize();
        }
    }
}
