//! MPRIS: media keys, desktop media controls, lock screen widgets. Each app
//! registers its own player; this is what they share: the service, the
//! controls that act on the player itself, following its position and
//! volume, and publishing what's playing.

use std::future::Future;
use std::rc::Rc;

use gtk::glib;
pub use mpris_server::{LoopStatus, PlaybackStatus, Player as Server};
use mpris_server::{Metadata, Time, TrackId};

use crate::player::Player;

/// What's playing, for the desktop's media controls.
#[derive(Default)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    /// Artwork URL.
    pub art: String,
    /// In seconds (0 if unknown).
    pub length: f64,
    pub url: String,
}

#[derive(Clone)]
pub struct Mpris {
    server: Rc<Server>,
    app_id: &'static str,
}

impl Mpris {
    /// Register `app_id`'s player: play/pause, seeking and the queue are the
    /// app's to connect (on [`Mpris::server`]) before [`Mpris::run`]. `raise`
    /// brings the window back (e.g. after background play).
    pub async fn new(
        app_id: &'static str,
        name: &str,
        player: &Player,
        raise: impl Fn() + 'static,
        quit: impl Fn() + 'static,
    ) -> anyhow::Result<Self> {
        let server = Server::builder(app_id)
            .identity(name)
            .desktop_entry(app_id)
            .can_play(true)
            .can_pause(true)
            .can_seek(true)
            .can_control(true)
            .can_go_next(true)
            .can_go_previous(true)
            .can_raise(true)
            .can_quit(true)
            .can_set_fullscreen(false)
            .build()
            .await?;
        let server = Rc::new(server);
        server.connect_pause(glib::clone!(
            #[weak]
            player,
            move |_| player.pause()
        ));
        server.connect_set_volume(glib::clone!(
            #[weak]
            player,
            move |_, v| player.set_volume(v.clamp(0.0, 1.0))
        ));
        server.connect_raise(move |_| raise());
        server.connect_quit(move |_| quit());

        let mpris = Self { server, app_id };
        {
            let s = mpris.server.clone();
            player.connect_position_notify(move |p| s.set_position(time(p.position())));
        }
        {
            let m = mpris.clone();
            player.connect_seeked(move |p| {
                let pos = time(p.position());
                m.update(move |s| async move {
                    let _ = s.seeked(pos).await;
                });
            });
        }
        {
            let m = mpris.clone();
            player.connect_volume_notify(move |p| {
                let v = p.volume();
                m.update(move |s| async move {
                    let _ = s.set_volume(v).await;
                });
            });
        }
        let _ = mpris.server.set_volume(player.volume()).await;
        Ok(mpris)
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Start answering requests.
    pub fn run(&self) {
        glib::spawn_future_local(self.server.run());
    }

    /// Change the server's properties (they are set asynchronously).
    pub fn update<F, Fut>(&self, f: F)
    where
        F: FnOnce(Rc<Server>) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        glib::spawn_future_local(f(self.server.clone()));
    }

    /// Publish what's playing (`None`: nothing).
    pub fn set_track(&self, track: Option<Track>, can_next: bool, can_previous: bool) {
        let meta = match track {
            None => Metadata::new(),
            Some(t) => {
                let mut b = Metadata::builder()
                    .trackid(self.track_id(&t.video_id))
                    .title(t.title)
                    .artist(t.artists);
                if !t.album.is_empty() {
                    b = b.album(t.album);
                }
                if !t.art.is_empty() {
                    b = b.art_url(t.art);
                }
                if t.length > 0.0 {
                    b = b.length(time(t.length));
                }
                if !t.url.is_empty() {
                    b = b.url(t.url);
                }
                b.build()
            }
        };
        self.update(move |s| async move {
            let _ = s.set_metadata(meta).await;
            let _ = s.set_can_go_next(can_next).await;
            let _ = s.set_can_go_previous(can_previous).await;
        });
    }

    pub fn set_status(&self, status: PlaybackStatus) {
        self.update(move |s| async move {
            let _ = s.set_playback_status(status).await;
        });
    }

    /// `/io/github/nergzd/SlopTube/Track/v<id>`, from the app ID.
    fn track_id(&self, video_id: &str) -> TrackId {
        let safe: String = video_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = format!("/{}/Track/v{safe}", self.app_id.replace('.', "/"));
        TrackId::try_from(path).unwrap_or(TrackId::NO_TRACK)
    }
}

pub fn time(secs: f64) -> Time {
    Time::from_micros((secs * 1e6) as i64)
}
