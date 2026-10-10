//! Keeping the resident set small.
//!
//! InnerTube responses are large JSON documents (1–5 MB) that are parsed into
//! the small model types and dropped right away. glibc keeps such freed memory
//! in its arenas, one per thread that ever allocated, so without help the
//! process grows by a few tens of megabytes after browsing a bit. We cap the
//! number of arenas at startup and hand freed memory back to the system a
//! moment after bursts of work.

use std::sync::atomic::{AtomicBool, Ordering};

/// Limit malloc arenas (call before any threads start).
pub fn init() {
    #[cfg(target_env = "gnu")]
    unsafe {
        // M_ARENA_MAX = -8
        libc::mallopt(-8, 2);
    }
}

static SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Return freed heap memory to the system once the current burst of work is
/// over (any thread; calls within the delay coalesce).
pub fn trim_soon() {
    if SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::runtime::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        SCHEDULED.store(false, Ordering::Release);
        trim_now();
    });
}

pub fn trim_now() {
    #[cfg(target_env = "gnu")]
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Resident memory in MB: (total, anonymous, file-backed).
pub fn rss_mb() -> Option<(u64, u64, u64)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| -> Option<u64> {
        let line = status.lines().find(|l| l.starts_with(name))?;
        line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb / 1024)
    };
    Some((field("VmRSS:")?, field("RssAnon:")?, field("RssFile:")?))
}
