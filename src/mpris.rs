//! SlopTube's MPRIS player (on `common::mpris`): media keys, desktop media
//! controls, lock screen widgets.

use std::rc::Rc;

use common::mpris::{Mpris, PlaybackStatus, Track};
use gtk::glib;
use gtk::prelude::*;

use crate::config;
use crate::playback::Playback;
use crate::player::PlayState;

fn status_for(state: PlayState, playing: bool) -> PlaybackStatus {
    match state {
        PlayState::Idle | PlayState::Error => PlaybackStatus::Stopped,
        PlayState::Ended => PlaybackStatus::Paused,
        _ if playing => PlaybackStatus::Playing,
        _ => PlaybackStatus::Paused,
    }
}

/// Register the MPRIS service. `raise` brings the window back (e.g. after background play).
pub async fn setup(playback: Playback, raise: impl Fn() + 'static, quit: impl Fn() + 'static) -> anyhow::Result<()> {
    let player = playback.player();
    let mpris = Mpris::new(config::APP_ID, config::APP_NAME, &player, raise, quit).await?;
    let mp = mpris.server();
    mp.connect_play_pause(glib::clone!(
        #[weak]
        playback,
        move |_| playback.toggle()
    ));
    mp.connect_play(glib::clone!(
        #[weak]
        playback,
        move |_| playback.play()
    ));
    mp.connect_stop(glib::clone!(
        #[weak]
        playback,
        move |_| playback.stop()
    ));
    mp.connect_next(glib::clone!(
        #[weak]
        playback,
        move |_| {
            playback.next();
        }
    ));
    mp.connect_previous(glib::clone!(
        #[weak]
        playback,
        move |_| {
            playback.previous();
        }
    ));
    mp.connect_seek(glib::clone!(
        #[weak]
        player,
        move |_, offset| player.seek(player.position() + offset.as_micros() as f64 / 1e6, true)
    ));
    mp.connect_set_position(glib::clone!(
        #[weak]
        player,
        move |_, _, pos| player.seek(pos.as_micros() as f64 / 1e6, true)
    ));
    mp.connect_set_rate(glib::clone!(
        #[weak]
        player,
        move |_, r| {
            if r > 0.0 {
                player.set_rate(r)
            }
        }
    ));
    mpris.run();

    // Metadata
    let update_meta = {
        let mpris = mpris.clone();
        let playback = playback.downgrade();
        move || {
            let Some(pb) = playback.upgrade() else { return };
            let id = pb.video_id();
            let track = pb.active().then(|| Track {
                url: if id.is_empty() { String::new() } else { format!("https://www.youtube.com/watch?v={id}") },
                video_id: id,
                title: pb.title(),
                artists: vec![pb.channel_name()],
                art: pb.thumbnail(),
                length: pb.player().duration(),
                ..Default::default()
            });
            // There is always something to go back to: the start of the video.
            mpris.set_track(track, pb.has_next(), true);
        }
    };
    let update_meta = Rc::new(update_meta);
    for prop in ["title", "channel-name", "thumbnail", "active"] {
        let u = update_meta.clone();
        playback.connect_notify_local(Some(prop), move |_, _| u());
    }
    {
        let u = update_meta.clone();
        player.connect_duration_notify(move |_| u());
    }
    {
        let u = update_meta.clone();
        playback.connect_local("details-loaded", false, move |_| {
            u();
            None
        });
    }

    // Status and rate
    {
        let mpris = mpris.clone();
        player.connect_state_notify(move |p| mpris.set_status(status_for(p.state(), p.is_playing_intent())));
    }
    {
        let mpris = mpris.clone();
        player.connect_rate_notify(move |p| {
            let r = p.rate();
            mpris.update(move |s| async move {
                let _ = s.set_rate(r).await;
            });
        });
    }
    let _ = mp.set_minimum_rate(0.25).await;
    let _ = mp.set_maximum_rate(2.0).await;
    Ok(())
}
