//! Helpers for the apps' virtualised lists.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};

#[derive(Default)]
struct Inner {
    pending: RefCell<VecDeque<glib::Object>>,
    draining: Cell<bool>,
    on_drained: RefCell<Option<Box<dyn Fn()>>>,
}

/// Appends a page of items to a list model about one per frame. A list or
/// grid view creates and binds a widget for every new item right away (it
/// keeps a few screens of them), so a whole page at once stalled scrolling
/// for a couple of frames.
#[derive(Clone, Default)]
pub struct Trickle(Rc<Inner>);

impl Trickle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called whenever the last pending item has gone in (e.g. to check whether
    /// to load the next page: until then the list's end isn't real yet).
    pub fn connect_drained(&self, f: impl Fn() + 'static) {
        self.0.on_drained.replace(Some(Box::new(f)));
    }

    /// Items are still waiting to go in.
    pub fn is_pending(&self) -> bool {
        !self.0.pending.borrow().is_empty()
    }

    /// Drop what hasn't gone in yet (e.g. when the list is replaced).
    pub fn clear(&self) {
        self.0.pending.borrow_mut().clear();
    }

    /// Append `items` to `model`, a few per frame of `widget` (all at once when
    /// it isn't on screen: there is nothing to keep smooth).
    pub fn append(
        &self,
        widget: &impl IsA<gtk::Widget>,
        model: &gio::ListStore,
        items: impl IntoIterator<Item = impl IsA<glib::Object>>,
    ) {
        let inner = &self.0;
        inner.pending.borrow_mut().extend(items.into_iter().map(|o| o.upcast()));
        if inner.draining.get() {
            return;
        }
        if !widget.is_mapped() {
            let objs: Vec<glib::Object> = inner.pending.borrow_mut().drain(..).collect();
            model.extend_from_slice(&objs);
            return;
        }
        inner.draining.set(true);
        let this = self.clone();
        let model = model.downgrade();
        widget.add_tick_callback(move |_, _| {
            let inner = &this.0;
            let Some(model) = model.upgrade() else {
                inner.draining.set(false);
                return glib::ControlFlow::Break;
            };
            let start = Instant::now();
            loop {
                let Some(obj) = inner.pending.borrow_mut().pop_front() else { break };
                model.append(&obj);
                if start.elapsed() >= Duration::from_millis(1) {
                    break;
                }
            }
            if inner.pending.borrow().is_empty() {
                inner.draining.set(false);
                if let Some(f) = inner.on_drained.borrow().as_ref() {
                    f();
                }
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }
}
