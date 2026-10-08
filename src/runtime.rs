//! A small tokio runtime for network I/O, bridged into the GTK main loop.
//!
//! UI code runs futures with `glib::spawn_future_local` and awaits tokio
//! `JoinHandle`s from there, so network work never blocks the UI thread.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .thread_name("sloptube-io")
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    })
}

pub fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(fut)
}

/// Run a future on the I/O runtime and await its output (from any executor).
pub async fn run<F>(fut: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match spawn(fut).await {
        Ok(v) => v,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("I/O task cancelled: {e}"),
    }
}
