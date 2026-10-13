//! Music's side of the shared Google account (`common::account`): handing the
//! session to yt-dlp, for Premium audio and age-restricted songs.

use common::auth;
pub use common::account::Account;

use crate::extractor::extractor;
use crate::settings;

/// Hand the account's cookies to yt-dlp (or stop doing so).
pub fn update_stream_cookies() {
    let session = common::session::get().filter(auth::Session::is_signed_in);
    let path = match session {
        Some(s) if settings::get().use_account_for_streams => match auth::write_cookie_file(&s, "music") {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn!("cannot write the cookie file for yt-dlp: {e:#}");
                None
            }
        },
        _ => {
            let _ = std::fs::remove_file(auth::cookie_file_path("music"));
            None
        }
    };
    extractor().set_cookie_file(path);
}
