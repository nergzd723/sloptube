//! `--selftest`: exercise the network layer against YouTube Music without a window.

use anyhow::{Context, Result};

use crate::api::{self, Item, ShelfKind, WatchTarget};
use crate::extractor::extractor;
use crate::settings::AudioQuality;

fn rss() -> String {
    crate::memory::rss_mb()
        .map(|(t, a, f)| format!("rss {t} MB (anon {a}, file {f})"))
        .unwrap_or_default()
}

pub async fn run() -> Result<()> {
    let c = api::client();
    let _ = c.refresh_client_version().await;

    let home = c.browse_page("FEmusic_home", None).await.context("home")?;
    println!(
        "home: {} shelves, {} chips, continuation {}",
        home.shelves.len(),
        home.chips.len(),
        home.continuation.is_some()
    );
    for s in home.shelves.iter().take(4) {
        println!("  [{:?}] {} ({} items)", s.kind, s.title, s.items.len());
    }
    if let Some(t) = &home.continuation {
        let more = c.browse_more(t).await.context("home continuation")?;
        println!("home continuation: {} shelves", more.shelves.len());
    }
    if let Some(chip) = home.chips.first() {
        let page = c.browse_page("FEmusic_home", chip.params.as_deref()).await.context("home chip")?;
        println!("home chip {:?}: {} shelves", chip.title, page.shelves.len());
    }
    let explore = c.browse_page("FEmusic_explore", None).await.context("explore")?;
    println!("explore: {} shelves", explore.shelves.len());

    let search = c.search("daft punk", None).await.context("search")?;
    println!("search: {} shelves, chips {:?}", search.shelves.len(), search.chips.iter().map(|c| &c.title).collect::<Vec<_>>());
    let songs_chip = search.chips.iter().find(|c| c.title == "Songs").and_then(|c| c.params.clone());
    let songs = c.search("daft punk", songs_chip.as_deref()).await.context("song search")?;
    let list = songs.shelves.iter().find(|s| s.kind == ShelfKind::List);
    println!(
        "songs: {} results, continuation {}",
        list.map(|s| s.items.len()).unwrap_or(0),
        list.and_then(|s| s.continuation.as_ref()).is_some()
    );
    if let Some(t) = list.and_then(|s| s.continuation.clone()) {
        let (items, _) = c.search_more(&t).await.context("search continuation")?;
        println!("songs continuation: {} results", items.len());
    }
    let sugg = c.suggestions("daft p").await.context("suggestions")?;
    println!("suggestions: {:?} + {} items", sugg.queries.iter().take(3).collect::<Vec<_>>(), sugg.items.len());

    let album = c.collection("MPREb_K8qWMWVqXGi").await.context("album")?;
    println!("album: {} — {} tracks, playlist {:?}", album.title, album.tracks.len(), album.playlist_id);
    let artist = c.artist("UC_kRDKYrUlrbtrSiyu5Tflg").await.context("artist")?;
    println!("artist: {} — {} shelves", artist.name, artist.shelves.len());
    let playlist = c
        .collection("VLRDCLAK5uy_n20FRYQXNt1p1wS55Nj2r14IouO5weaYU")
        .await
        .context("playlist")?;
    println!("playlist: {} — {} tracks", playlist.title, playlist.tracks.len());

    let first_song = songs
        .shelves
        .iter()
        .flat_map(|s| &s.items)
        .find_map(|i| match i {
            Item::Track(t) => Some(t.clone()),
            _ => None,
        })
        .context("no song found")?;
    let q = c
        .queue(&WatchTarget {
            video_id: Some(first_song.video_id.clone()),
            ..Default::default()
        })
        .await
        .context("radio")?;
    println!(
        "radio for {:?}: {} tracks, lyrics {:?}, related {:?}",
        first_song.title,
        q.tracks.len(),
        q.lyrics,
        q.related
    );
    if let Some(t) = &q.continuation {
        let more = c.queue_more(t).await.context("radio continuation")?;
        println!("radio continuation: {} tracks", more.tracks.len());
    }
    if let Some(l) = &q.lyrics {
        let lyrics = c.lyrics(l).await.context("lyrics")?;
        println!("lyrics: {} lines", lyrics.map(|l| l.text.lines().count()).unwrap_or(0));
    }
    if let Some(r) = &q.related {
        let related = c.browse_page(r, None).await.context("related")?;
        println!("related: {} shelves", related.shelves.len());
    }
    println!("{}", rss());

    for label in ["cold", "warm"] {
        let started = std::time::Instant::now();
        extractor().invalidate(&first_song.video_id);
        let info = extractor().extract(&first_song.video_id).await.context("extraction")?;
        let f = info.select(AudioQuality::High).context("no audio stream")?;
        println!(
            "extraction ({label}): {:.2}s, {} formats, picked {}",
            started.elapsed().as_secs_f64(),
            info.formats.len(),
            f.describe()
        );
    }
    println!("{}", rss());
    Ok(())
}
