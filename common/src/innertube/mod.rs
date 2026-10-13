//! YouTube's InnerTube API, as both apps use it: SlopTube with the `WEB`
//! client, Music with `WEB_REMIX`. The renderers the two receive differ, so
//! each app keeps its own parsers; what is here is shared underneath them.

pub mod client;
pub mod json;
pub mod model;
