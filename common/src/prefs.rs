//! Preferences kept as JSON: a store each app wraps around its own settings type.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Settings of type `T`, loaded once and written back on every change. Main thread only:
/// keep it in a `thread_local!`.
pub struct Store<T> {
    path: PathBuf,
    value: RefCell<T>,
    read_only: Cell<bool>,
}

impl<T: Serialize + DeserializeOwned + Default + Clone> Store<T> {
    /// Load from `path`; a missing or unreadable file gives the defaults.
    pub fn load(path: PathBuf) -> Self {
        let value = std::fs::read(&path)
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default();
        Self {
            path,
            value: RefCell::new(value),
            read_only: Cell::new(false),
        }
    }

    /// A snapshot of the current settings.
    pub fn get(&self) -> T {
        self.value.borrow().clone()
    }

    /// Change the settings and save them (unless read-only).
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        let mut value = self.value.borrow_mut();
        f(&mut value);
        if self.read_only.get() {
            return;
        }
        match serde_json::to_vec_pretty(&*value) {
            Ok(data) => {
                if let Err(e) = crate::paths::write_atomic(&self.path, &data) {
                    log::warn!("failed to save {}: {e}", self.path.display());
                }
            }
            Err(e) => log::warn!("failed to serialize {}: {e}", self.path.display()),
        }
    }

    /// Keep changes in memory only (e.g. for a diagnostic run).
    pub fn set_read_only(&self, read_only: bool) {
        self.read_only.set(read_only);
    }
}

/// Put `query` at the front of a search history, without duplicates (ignoring
/// case) and at most 30 long.
pub fn remember_search(history: &mut Vec<String>, query: &str) {
    let q = query.trim();
    if q.is_empty() {
        return;
    }
    history.retain(|h| !h.eq_ignore_ascii_case(q));
    history.insert(0, q.to_owned());
    history.truncate(30);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_history_moves_repeats_to_the_front() {
        let mut h = vec!["a".to_owned(), "Rust GTK".to_owned()];
        remember_search(&mut h, "  rust gtk ");
        remember_search(&mut h, "");
        assert_eq!(h, ["rust gtk", "a"]);
    }
}
