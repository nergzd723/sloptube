//! Touch on scrolled content: a tap that only stops a gliding list shouldn't
//! also open what slid under the finger, and labels shouldn't keep a touch
//! drag from scrolling their list.

use gtk::glib;
use gtk::prelude::*;

/// Let a touch drag that starts on `label` scroll its list.
/// GtkLabel's own gestures claim every touch press on a label with links or a
/// selectable one, and claiming cancels the scrolled window's touch drag, so a list
/// couldn't be touch-scrolled from such a label. Keep touch away from the
/// label's gestures and report taps (in label coordinates) instead; mouse and pen
/// input still reach the label as usual. A tap that only stopped a gliding list
/// isn't reported.
pub fn route_label_touch(label: &gtk::Label, on_tap: impl Fn(&gtk::Label, f64, f64) + 'static) {
    use gtk::gdk::EventType;
    let start = std::rc::Rc::new(std::cell::Cell::new(None::<(f64, f64)>));
    let legacy = gtk::EventControllerLegacy::new();
    legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
    legacy.connect_event(move |c, ev| {
        match ev.event_type() {
            EventType::TouchBegin => start.set(ev.position()),
            EventType::TouchUpdate => {}
            EventType::TouchCancel => start.set(None),
            EventType::TouchEnd => {
                if let (Some((x0, y0)), Some((x, y))) = (start.take(), ev.position()) {
                    let label = c.widget().and_downcast::<gtk::Label>().unwrap();
                    if (x - x0).hypot(y - y0) < 10.0 && !press_stopped_scrolling(&label) {
                        // Event positions are in surface coordinates.
                        let native = label.native().unwrap();
                        let (sx, sy) = native.surface_transform();
                        let p = gtk::graphene::Point::new((x - sx) as f32, (y - sy) as f32);
                        if let Some(p) = native.compute_point(&label, &p) {
                            on_tap(&label, p.x() as f64, p.y() as f64);
                        }
                    }
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    label.add_controller(legacy);
}

const LAST_MOVE_KEY: &str = "sloptube-last-move";
const PRESS_STOPPED_KEY: &str = "sloptube-press-stopped";

/// Note whether each press on `scrolled` lands while it is still moving (gliding
/// after a fling), for [`press_stopped_scrolling`].
pub fn track_scrolling(scrolled: &gtk::ScrolledWindow) {
    for adj in [scrolled.hadjustment(), scrolled.vadjustment()] {
        let scrolled = scrolled.downgrade();
        adj.connect_value_changed(move |_| {
            if let Some(s) = scrolled.upgrade() {
                // SAFETY: the key is only ever used with `Instant`.
                unsafe { s.set_data(LAST_MOVE_KEY, std::time::Instant::now()) };
            }
        });
    }
    let legacy = gtk::EventControllerLegacy::new();
    legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
    legacy.connect_event(|c, ev| {
        use gtk::gdk::EventType;
        if matches!(ev.event_type(), EventType::ButtonPress | EventType::TouchBegin) {
            if let Some(s) = c.widget() {
                // SAFETY: the keys are only ever used with `Instant` and `bool`.
                unsafe {
                    let moving = s
                        .data::<std::time::Instant>(LAST_MOVE_KEY)
                        .is_some_and(|t| t.as_ref().elapsed() < std::time::Duration::from_millis(100));
                    s.set_data(PRESS_STOPPED_KEY, moving);
                }
            }
        }
        glib::Propagation::Proceed
    });
    scrolled.add_controller(legacy);
}

/// Whether the current press landed on a list (`widget` or one around it) that
/// was still moving. Such a tap only stops the list (as on phones); it shouldn't
/// also open whatever slid under the finger.
pub fn press_stopped_scrolling(widget: &impl IsA<gtk::Widget>) -> bool {
    let mut w = Some(widget.as_ref().clone());
    while let Some(x) = w {
        // SAFETY: see `track_scrolling`.
        if unsafe { x.data::<bool>(PRESS_STOPPED_KEY) }.is_some_and(|b| unsafe { *b.as_ref() }) {
            return true;
        }
        w = x.parent();
    }
    false
}
