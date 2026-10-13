//! Getting playback going again after a failure. Expired URLs, a flaky CDN
//! response or a dropped connection usually go away with fresh URLs, so a
//! couple of failures are retried straight away. Offline (or with YouTube out
//! of reach), it waits for the network instead and reloads once it is back.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gio, glib};

/// Retries in a row before giving up (until playback works again).
const MAX_RETRIES: u32 = 2;

#[derive(Default)]
struct Inner {
    retries: Cell<u32>,
    waiting: Cell<bool>,
    probe: RefCell<Option<glib::SourceId>>,
    reload: RefCell<Option<Rc<dyn Fn()>>>,
    network_changed: RefCell<Option<glib::SignalHandlerId>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(id) = self.network_changed.take() {
            gio::NetworkMonitor::default().disconnect(id);
        }
        if let Some(id) = self.probe.take() {
            id.remove();
        }
    }
}

#[derive(Default)]
pub struct Recovery(Rc<Inner>);

impl Recovery {
    /// What to do once the network is back after [`Recovery::wait_for_network`].
    pub fn connect_back_online(&self, reload: impl Fn() + 'static) {
        self.0.reload.replace(Some(Rc::new(reload)));
        if self.0.network_changed.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let id = gio::NetworkMonitor::default().connect_network_changed(move |_, available| {
            if available {
                back_online(&weak);
            }
        });
        self.0.network_changed.replace(Some(id));
    }

    pub fn online() -> bool {
        gio::NetworkMonitor::default().is_network_available()
    }

    /// After a failure that loading again may fix: whether to retry now. Counts
    /// the retry; offline, or after a couple in a row, it's time to wait instead.
    pub fn retry_now(&self) -> bool {
        let n = self.0.retries.get();
        if n >= MAX_RETRIES || !Self::online() {
            return false;
        }
        self.0.retries.set(n + 1);
        true
    }

    /// Playback works (or starts afresh): failures may be retried again.
    pub fn reset(&self) {
        self.0.retries.set(0);
    }

    /// Reload once the network is back: on NetworkMonitor's `network-changed`,
    /// or when YouTube can be reached again. Not every reconnection comes with
    /// a signal (e.g. the Wi-Fi stays associated while the internet behind it
    /// drops), so that is tried every few seconds.
    pub fn wait_for_network(&self) {
        self.0.waiting.set(true);
        if self.0.probe.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(&self.0);
        let id = glib::timeout_add_seconds_local(10, move || {
            let Some(inner) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !inner.waiting.get() {
                inner.probe.take();
                return glib::ControlFlow::Break;
            }
            let client = gio::SocketClient::new();
            client.set_timeout(5);
            let weak = weak.clone();
            client.connect_to_host_async("www.youtube.com", 443, None::<&gio::Cancellable>, move |res| {
                if res.is_ok() {
                    back_online(&weak);
                }
            });
            glib::ControlFlow::Continue
        });
        self.0.probe.replace(Some(id));
    }

    /// Stop waiting (playback started again, or the failure is gone).
    pub fn cancel(&self) {
        self.0.waiting.set(false);
    }

    pub fn waiting(&self) -> bool {
        self.0.waiting.get()
    }
}

fn back_online(weak: &Weak<Inner>) {
    let Some(inner) = weak.upgrade() else { return };
    if !inner.waiting.replace(false) {
        return;
    }
    inner.retries.set(0);
    if let Some(id) = inner.probe.take() {
        id.remove();
    }
    let reload = inner.reload.borrow().clone();
    if let Some(reload) = reload {
        reload();
    }
}
