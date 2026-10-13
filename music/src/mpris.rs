//! Music's MPRIS player (on `common::mpris`): media keys, desktop media
//! controls, lock screen widgets.

use std::rc::Rc;

use common::mpris::{LoopStatus, Mpris, PlaybackStatus, Track};
use gtk::glib;
use gtk::prelude::*;

use crate::config;
use crate::playback::{Playback, RepeatMode};
use crate::player::PlayState;

fn status_for(pb: &Playback) -> PlaybackStatus {
    if !pb.active() {
        return PlaybackStatus::Stopped;
    }
    if pb.is_playing() && pb.player().state() != PlayState::Ended {
        PlaybackStatus::Playing
    } else {
        PlaybackStatus::Paused
    }
}

fn loop_status(r: RepeatMode) -> LoopStatus {
    match r {
        RepeatMode::Off => LoopStatus::None,
        RepeatMode::All => LoopStatus::Playlist,
        RepeatMode::One => LoopStatus::Track,
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
        move |_| {
            if !playback.is_playing() {
                playback.toggle();
            }
        }
    ));
    // Stopping would drop the queue; pause instead.
    mp.connect_stop(glib::clone!(
        #[weak]
        player,
        move |_| player.pause()
    ));
    mp.connect_next(glib::clone!(
        #[weak]
        playback,
        move |_| playback.next()
    ));
    mp.connect_previous(glib::clone!(
        #[weak]
        playback,
        move |_| playback.previous()
    ));
    mp.connect_seek(glib::clone!(
        #[weak]
        playback,
        #[weak]
        player,
        move |_, offset| playback.seek(player.position() + offset.as_micros() as f64 / 1e6)
    ));
    mp.connect_set_position(glib::clone!(
        #[weak]
        playback,
        move |_, _, pos| playback.seek(pos.as_micros() as f64 / 1e6)
    ));
    mp.connect_set_shuffle(glib::clone!(
        #[weak]
        playback,
        move |_, s| playback.set_shuffle(s)
    ));
    mp.connect_set_loop_status(glib::clone!(
        #[weak]
        playback,
        move |_, l| playback.set_repeat(match l {
            LoopStatus::None => RepeatMode::Off,
            LoopStatus::Playlist => RepeatMode::All,
            LoopStatus::Track => RepeatMode::One,
        })
    ));
    mpris.run();

    let update_meta = {
        let mpris = mpris.clone();
        let playback = playback.downgrade();
        move || {
            let Some(pb) = playback.upgrade() else { return };
            let id = pb.video_id();
            let current = pb.current_track();
            let length = match pb.player().duration() {
                d if d > 0.0 => d,
                _ => current.as_ref().and_then(|t| t.duration).unwrap_or(0) as f64,
            };
            let track = pb.active().then(|| Track {
                url: if id.is_empty() { String::new() } else { format!("https://music.youtube.com/watch?v={id}") },
                video_id: id,
                title: pb.title(),
                artists: current.map(|t| t.artists.into_iter().map(|a| a.name).collect()).unwrap_or_default(),
                album: pb.album(),
                art: pb.art(),
                length,
            });
            mpris.set_track(track, pb.has_next(), pb.has_previous());
        }
    };
    let update_meta = Rc::new(update_meta);
    {
        let u = update_meta.clone();
        playback.connect_track_changed(move |_| u());
    }
    {
        let u = update_meta.clone();
        playback.connect_active_notify(move |_| u());
    }
    {
        let u = update_meta.clone();
        player.connect_duration_notify(move |_| u());
    }

    let update_status = {
        let mpris = mpris.clone();
        let playback = playback.downgrade();
        move || {
            if let Some(pb) = playback.upgrade() {
                mpris.set_status(status_for(&pb));
            }
        }
    };
    let update_status = Rc::new(update_status);
    {
        let u = update_status.clone();
        player.connect_state_notify(move |_| u());
    }
    {
        let u = update_status.clone();
        playback.connect_loading_notify(move |_| u());
    }
    {
        let u = update_status.clone();
        playback.connect_active_notify(move |_| u());
    }
    {
        let mpris = mpris.clone();
        playback.connect_shuffle_notify(move |pb| {
            let s = pb.shuffle();
            mpris.update(move |m| async move {
                let _ = m.set_shuffle(s).await;
            });
        });
    }
    {
        let mpris = mpris.clone();
        playback.connect_repeat_notify(move |pb| {
            let l = loop_status(pb.repeat());
            mpris.update(move |m| async move {
                let _ = m.set_loop_status(l).await;
            });
        });
    }
    let _ = mp.set_shuffle(playback.shuffle()).await;
    let _ = mp.set_loop_status(loop_status(playback.repeat())).await;
    update_meta();
    update_status();
    Ok(())
}
