//! What SlopTube and SlopTube Music share. Both apps run in one process, so
//! the singletons here (the tokio runtime, ...) are shared by both too.
//!
//! Nothing here reads either app's settings; what differs between the apps
//! is passed in.

pub mod links;
pub mod memory;
pub mod paths;
pub mod prefs;
pub mod runtime;
pub mod touch;
