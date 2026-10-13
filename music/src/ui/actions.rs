//! What clicking an item, its play button or its menu does.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::{Nav, navigate, toast};
use crate::api::{self, BrowseTarget, Card, CardKind, Item, PageKind, Track, WatchTarget};
use crate::library::Library;
use crate::playback::Playback;
use crate::runtime;

/// The default action for an item: play songs, open albums/playlists/artists/moods.
pub fn activate(widget: &impl IsA<gtk::Widget>, item: &Item) {
    match item {
        Item::Track(t) => Playback::get().play_song(t.clone()),
        Item::Card(c) if c.kind == CardKind::Mix => play_card(c, false),
        Item::Card(c) => navigate(widget, Nav::Page(c.target.clone(), c.title.clone())),
        Item::Mood(m) => navigate(widget, Nav::Page(m.target.clone(), m.title.clone())),
    }
}

/// Whether the tile's play button makes sense.
pub fn playable(item: &Item) -> bool {
    match item {
        Item::Track(_) => true,
        Item::Card(c) => !matches!(c.kind, CardKind::Artist | CardKind::Profile | CardKind::Podcast),
        Item::Mood(_) => false,
    }
}

/// The tracks behind a card (album, playlist or mix).
pub async fn card_tracks(card: &Card) -> anyhow::Result<Vec<Track>> {
    let card = card.clone();
    runtime::run(async move {
        let c = api::client();
        if card.kind == CardKind::Mix {
            let target = card.play.clone().unwrap_or_default();
            return Ok(c.queue(&target).await?.tracks);
        }
        Ok(c.collection(&card.target.id).await?.tracks)
    })
    .await
}

/// Play an album, playlist or mix (optionally shuffled).
pub fn play_card(card: &Card, shuffle: bool) {
    let pb = Playback::get();
    if !shuffle
        && let Some(target) = card.play.clone().filter(|p| p.playlist_id.is_some()) {
            pb.play_watch(target, &card.title);
            return;
        }
    let card = card.clone();
    glib::spawn_future_local(async move {
        match card_tracks(&card).await {
            Ok(tracks) if shuffle => Playback::get().shuffle_tracks(tracks, &card.title),
            Ok(tracks) => Playback::get().play_tracks(tracks, 0, &card.title),
            Err(e) => toast(&format!("Couldn't play “{}”: {e}", card.title)),
        }
    });
}

fn with_tracks(item: Item, f: impl FnOnce(Vec<Track>) + 'static) {
    match item {
        Item::Track(t) => f(vec![t]),
        Item::Card(c) => {
            glib::spawn_future_local(async move {
                match card_tracks(&c).await {
                    Ok(tracks) => f(tracks),
                    Err(e) => toast(&format!("Couldn't load “{}”: {e}", c.title)),
                }
            });
        }
        Item::Mood(_) => {}
    }
}

pub fn item_url(item: &Item) -> Option<String> {
    match item {
        Item::Track(t) => Some(t.url()),
        Item::Card(c) => Some(super::browse_url(&c.target)),
        Item::Mood(_) => None,
    }
}

fn first_artist(item: &Item) -> Option<(BrowseTarget, String)> {
    let artists = match item {
        Item::Track(t) => &t.artists,
        Item::Card(c) => &c.artists,
        Item::Mood(_) => return None,
    };
    artists.iter().find_map(|a| {
        a.id.as_ref().map(|id| {
            (
                BrowseTarget {
                    id: id.clone(),
                    params: None,
                    kind: PageKind::Artist,
                },
                a.name.clone(),
            )
        })
    })
}

/// Start a radio from an item: a song's radio, or an artist's mix.
fn start_radio(item: &Item) {
    match item {
        Item::Track(t) => Playback::get().play_song(t.clone()),
        Item::Card(c) if matches!(c.kind, CardKind::Artist | CardKind::Profile) => {
            let id = c.target.id.clone();
            let name = c.title.clone();
            glib::spawn_future_local(async move {
                let res = runtime::run(async move { api::client().artist(&id).await }).await;
                match res.map(|a| a.radio) {
                    Ok(Some(radio)) => Playback::get().play_watch(radio, &format!("{name} radio")),
                    Ok(None) => toast("This artist has no radio"),
                    Err(e) => toast(&format!("Couldn't start the radio: {e}")),
                }
            });
        }
        Item::Card(c) => {
            if let Some(pl) = c.play.as_ref().and_then(|p| p.playlist_id.clone()) {
                Playback::get().play_watch(
                    WatchTarget {
                        playlist_id: Some(format!("RDAMPL{pl}")),
                        params: Some("wAEB".into()),
                        ..Default::default()
                    },
                    &format!("{} radio", c.title),
                );
            }
        }
        Item::Mood(_) => {}
    }
}

/// The context menu for an item (action names in the "item" group).
pub fn menu_for(item: &Item) -> gio::Menu {
    let menu = gio::Menu::new();
    let playing = gio::Menu::new();
    match item {
        Item::Track(t) => {
            playing.append(Some("Play next"), Some("item.play-next"));
            playing.append(Some("Add to queue"), Some("item.add-to-queue"));
            playing.append(Some("Start radio"), Some("item.radio"));
            menu.append_section(None, &playing);
            let lib = gio::Menu::new();
            let liked = Library::get().is_track_liked(t);
            lib.append(Some(if liked { "Remove from liked songs" } else { "Like" }), Some("item.like"));
            if api::client().is_signed_in() {
                lib.append(Some("Dislike"), Some("item.dislike"));
            }
            menu.append_section(None, &lib);
            let go = gio::Menu::new();
            if t.album.is_some() {
                go.append(Some("Go to album"), Some("item.album"));
            }
            if first_artist(item).is_some() {
                go.append(Some("Go to artist"), Some("item.artist"));
            }
            go.append(Some("Copy link"), Some("item.copy-link"));
            menu.append_section(None, &go);
        }
        Item::Card(c) => {
            let is_artist = matches!(c.kind, CardKind::Artist | CardKind::Profile);
            if !is_artist && c.kind != CardKind::Podcast {
                playing.append(Some("Play"), Some("item.play"));
                playing.append(Some("Shuffle"), Some("item.shuffle"));
                playing.append(Some("Play next"), Some("item.play-next"));
                playing.append(Some("Add to queue"), Some("item.add-to-queue"));
            }
            if is_artist || c.play.as_ref().is_some_and(|p| p.playlist_id.is_some()) {
                playing.append(Some("Start radio"), Some("item.radio"));
            }
            menu.append_section(None, &playing);
            let lib = gio::Menu::new();
            if c.kind != CardKind::Mix {
                let saved = Library::get().is_saved(&c.target.id);
                lib.append(
                    Some(if saved { "Remove from library" } else { "Save to library" }),
                    Some("item.save"),
                );
            }
            menu.append_section(None, &lib);
            let go = gio::Menu::new();
            if !is_artist && first_artist(item).is_some() {
                go.append(Some("Go to artist"), Some("item.artist"));
            }
            go.append(Some("Copy link"), Some("item.copy-link"));
            menu.append_section(None, &go);
        }
        Item::Mood(_) => {}
    }
    menu
}

/// Give `widget` the "item" action group, acting on whatever `get` returns at the time.
pub fn install(widget: &impl IsA<gtk::Widget>, get: Rc<dyn Fn() -> Option<Item>>) {
    let group = gio::SimpleActionGroup::new();
    let weak = widget.upcast_ref::<gtk::Widget>().downgrade();
    let add = |name: &str, f: Box<dyn Fn(&gtk::Widget, Item)>| {
        let action = gio::SimpleAction::new(name, None);
        let get = get.clone();
        let weak = weak.clone();
        action.connect_activate(move |_, _| {
            if let (Some(w), Some(item)) = (weak.upgrade(), get()) {
                f(&w, item);
            }
        });
        group.add_action(&action);
    };
    add(
        "play",
        Box::new(|_, item| match &item {
            Item::Card(c) => play_card(c, false),
            other => activate_play(other),
        }),
    );
    add(
        "shuffle",
        Box::new(|_, item| {
            if let Item::Card(c) = &item {
                play_card(c, true);
            }
        }),
    );
    add(
        "play-next",
        Box::new(|_, item| {
            let n = matches!(item, Item::Track(_));
            with_tracks(item, move |tracks| {
                let count = tracks.len();
                Playback::get().play_next(tracks);
                toast(if n || count == 1 { "Song will play next" } else { "Songs will play next" });
            })
        }),
    );
    add(
        "add-to-queue",
        Box::new(|_, item| {
            with_tracks(item, |tracks| {
                Playback::get().add_to_queue(tracks);
                toast("Added to queue");
            })
        }),
    );
    add("radio", Box::new(|_, item| start_radio(&item)));
    add(
        "like",
        Box::new(|_, item| {
            if let Item::Track(t) = &item {
                let liked = Library::get().toggle_liked(t);
                toast(if liked { "Added to liked songs" } else { "Removed from liked songs" });
            }
        }),
    );
    add(
        "dislike",
        Box::new(|_, item| {
            if let Item::Track(t) = &item {
                Library::get().dislike(t);
                let pb = Playback::get();
                if pb.video_id() == t.video_id {
                    pb.next();
                }
                toast("Disliked; YouTube Music will play less like it");
            }
        }),
    );
    add(
        "save",
        Box::new(|_, item| {
            if let Item::Card(c) = &item {
                let lib = Library::get();
                let saved = lib.toggle_saved(c);
                toast(if saved { "Saved to library" } else { "Removed from library" });
            }
        }),
    );
    add(
        "album",
        Box::new(|w, item| {
            if let Item::Track(Track { album: Some(a), .. }) = &item {
                navigate(
                    w,
                    Nav::Page(
                        BrowseTarget {
                            id: a.id.clone(),
                            params: None,
                            kind: PageKind::Album,
                        },
                        a.name.clone(),
                    ),
                );
            }
        }),
    );
    add(
        "artist",
        Box::new(|w, item| {
            if let Some((target, name)) = first_artist(&item) {
                navigate(w, Nav::Page(target, name));
            }
        }),
    );
    add(
        "copy-link",
        Box::new(|w, item| {
            if let Some(url) = item_url(&item) {
                super::copy_text(w, &url, "Link copied");
            }
        }),
    );
    widget.insert_action_group("item", Some(&group));
}

fn activate_play(item: &Item) {
    if let Item::Track(t) = item {
        Playback::get().play_song(t.clone());
    }
}

/// Right click and long press open the item's menu at the pointer.
pub fn attach_context_menu(widget: &impl IsA<gtk::Widget>, get: Rc<dyn Fn() -> Option<Item>>) {
    let show = {
        let get = get.clone();
        move |w: &gtk::Widget, x: f64, y: f64| {
            let Some(item) = get() else { return };
            if matches!(item, Item::Mood(_)) {
                return;
            }
            let popover = gtk::PopoverMenu::from_model(Some(&menu_for(&item)));
            popover.set_parent(w);
            popover.set_has_arrow(false);
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.connect_closed(|p| {
                // Unparent once the popover has finished closing.
                let p = p.clone();
                glib::idle_add_local_once(move || p.unparent());
            });
            popover.popup();
        }
    };
    let show = Rc::new(show);
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    click.connect_pressed({
        let show = show.clone();
        move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            if let Some(w) = g.widget() {
                show(&w, x, y);
            }
        }
    });
    widget.add_controller(click);
    let press = gtk::GestureLongPress::new();
    press.set_touch_only(true);
    press.connect_pressed(move |g, x, y| {
        g.set_state(gtk::EventSequenceState::Claimed);
        if let Some(w) = g.widget() {
            show(&w, x, y);
        }
    });
    widget.add_controller(press);
}
