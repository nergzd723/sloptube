//! MPRIS integration: media keys, desktop media controls, lock screen widgets.

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player as MprisPlayer, Time, TrackId};

use crate::config;
use crate::playback::{Playback, RepeatMode};
use crate::player::PlayState;

fn track_id(video_id: &str) -> TrackId {
    let safe: String = video_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    TrackId::try_from(format!("/io/github/nergzd/SlopTubeMusic/Track/v{safe}")).unwrap_or(TrackId::NO_TRACK)
}

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
    let mp = MprisPlayer::builder(config::APP_ID)
        .identity(config::APP_NAME)
        .desktop_entry(config::APP_ID)
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
    let mp = Rc::new(mp);
    let player = playback.player();

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
    mp.connect_pause(glib::clone!(
        #[weak]
        player,
        move |_| player.pause()
    ));
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
    mp.connect_set_volume(glib::clone!(
        #[weak]
        player,
        move |_, v| player.set_volume(v.clamp(0.0, 1.0))
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
    mp.connect_raise(move |_| raise());
    mp.connect_quit(move |_| quit());
    glib::spawn_future_local(mp.run());

    let update_meta = {
        let mp = mp.clone();
        let playback = playback.downgrade();
        move || {
            let Some(pb) = playback.upgrade() else { return };
            let mp = mp.clone();
            let mut b = Metadata::builder()
                .trackid(track_id(&pb.video_id()))
                .title(pb.title())
                .artist(pb.current_track().map(|t| t.artists.into_iter().map(|a| a.name).collect::<Vec<_>>()).unwrap_or_default());
            if !pb.album().is_empty() {
                b = b.album(pb.album());
            }
            if !pb.art().is_empty() {
                b = b.art_url(pb.art());
            }
            let d = pb.player().duration();
            if d > 0.0 {
                b = b.length(Time::from_micros((d * 1e6) as i64));
            } else if let Some(d) = pb.current_track().and_then(|t| t.duration) {
                b = b.length(Time::from_secs(d as i64));
            }
            if !pb.video_id().is_empty() {
                b = b.url(format!("https://music.youtube.com/watch?v={}", pb.video_id()));
            }
            let meta = b.build();
            let active = pb.active();
            let (can_next, can_prev) = (pb.has_next(), pb.has_previous());
            glib::spawn_future_local(async move {
                let _ = mp.set_metadata(if active { meta } else { Metadata::new() }).await;
                let _ = mp.set_can_go_next(can_next).await;
                let _ = mp.set_can_go_previous(can_prev).await;
            });
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
        let mp = mp.clone();
        let playback = playback.downgrade();
        move || {
            let Some(pb) = playback.upgrade() else { return };
            let status = status_for(&pb);
            let mp = mp.clone();
            glib::spawn_future_local(async move {
                let _ = mp.set_playback_status(status).await;
            });
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
        let mp = mp.clone();
        player.connect_position_notify(move |p| {
            mp.set_position(Time::from_micros((p.position() * 1e6) as i64));
        });
    }
    {
        let mp = mp.clone();
        player.connect_seeked(move |p| {
            let mp = mp.clone();
            let pos = Time::from_micros((p.position() * 1e6) as i64);
            glib::spawn_future_local(async move {
                let _ = mp.seeked(pos).await;
            });
        });
    }
    {
        let mp = mp.clone();
        player.connect_volume_notify(move |p| {
            let mp = mp.clone();
            let v = p.volume();
            glib::spawn_future_local(async move {
                let _ = mp.set_volume(v).await;
            });
        });
    }
    {
        let mp = mp.clone();
        playback.connect_shuffle_notify(move |pb| {
            let mp = mp.clone();
            let s = pb.shuffle();
            glib::spawn_future_local(async move {
                let _ = mp.set_shuffle(s).await;
            });
        });
    }
    {
        let mp = mp.clone();
        playback.connect_repeat_notify(move |pb| {
            let mp = mp.clone();
            let l = loop_status(pb.repeat());
            glib::spawn_future_local(async move {
                let _ = mp.set_loop_status(l).await;
            });
        });
    }
    let _ = mp.set_volume(player.volume()).await;
    let _ = mp.set_shuffle(playback.shuffle()).await;
    let _ = mp.set_loop_status(loop_status(playback.repeat())).await;
    update_meta();
    update_status();
    Ok(())
}
