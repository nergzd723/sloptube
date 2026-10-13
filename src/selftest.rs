//! `sloptube --selftest`: exercise the network layer without a UI.

use crate::api::{self, Item, SearchFilter};
use crate::extractor;
use crate::player::{decoders, formats};

fn describe(item: &Item) -> String {
    match item {
        Item::Video(v) => format!(
            "video {} | {} | {} | {} | {} | {}",
            v.id,
            v.title,
            v.channel_name.as_deref().unwrap_or("-"),
            v.duration.as_deref().unwrap_or("-"),
            v.views.as_deref().unwrap_or("-"),
            v.published.as_deref().unwrap_or("-")
        ),
        Item::Channel(c) => format!("channel {} | {} | {:?}", c.id, c.name, c.subscribers),
        Item::Playlist(p) => format!("playlist {} | {} | {:?}", p.id, p.title, p.video_count),
    }
}

pub async fn run() -> anyhow::Result<()> {
    let c = api::client();
    let t = std::time::Instant::now();
    c.refresh_client_version().await?;
    println!("client version refreshed in {:?}", t.elapsed());

    let t = std::time::Instant::now();
    let page = c.search("blender donut tutorial", SearchFilter::All).await?;
    println!("search: {} items, cont={} ({:?})", page.items.len(), page.continuation.is_some(), t.elapsed());
    for i in page.items.iter().take(4) {
        println!("  {}", describe(i));
    }
    if let Some(tok) = &page.continuation {
        let more = c.search_more(tok).await?;
        println!("search page 2: {} items", more.items.len());
    }
    let chans = c.search("blender", SearchFilter::Channels).await?;
    println!("channel search: {}", chans.items.iter().take(2).map(describe).collect::<Vec<_>>().join(" || "));

    let t = std::time::Instant::now();
    let d = c.video("aqz-KE-bpKQ", None).await?;
    println!(
        "video: {} | {} | likes {:?} | {} related | comments {:?} {:?} ({:?})",
        d.title, d.channel.name, d.likes, d.related.len(), d.comment_count, d.comments_token.is_some(), t.elapsed()
    );
    if let Some(tok) = &d.comments_token {
        let cm = c.comments(tok, &d.id).await?;
        println!("comments: {} (count {:?}), first: {} — {:?}", cm.comments.len(), cm.count,
            cm.comments.first().map(|c| c.author.as_str()).unwrap_or(""),
            cm.comments.first().map(|c| c.text.text.chars().take(60).collect::<String>()));
    }
    if let Some(tok) = &d.related_continuation {
        let more = c.related_more(tok).await?;
        println!("related page 2: {} items", more.items.len());
    }

    let ch = c.channel("@BlenderOfficial").await?;
    println!("channel: {} {:?} {:?} tabs={:?} videos={}", ch.name, ch.handle, ch.subscribers,
        ch.tabs.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ch.content.items.len());

    let pl = c.playlist("PLa1F2ddGya_-UvuAqHAksYnB0qL9yWDO6").await?;
    println!("playlist: {} by {:?} stats {:?} items {} cont {}", pl.title, pl.owner, pl.stats, pl.content.items.len(), pl.content.continuation.is_some());

    let ex = c.shelves("UC-9-kyTW8ZkZNDHQJ6FgpwQ", None).await?;
    println!("explore music: {} shelves ({}), {} loose items",
        ex.shelves.len(), ex.shelves.iter().map(|s| format!("{}:{}", s.title, s.items.len())).collect::<Vec<_>>().join(", "), ex.items.len());

    let sugg = c.suggestions("blender").await?;
    println!("suggestions: {:?}", &sugg[..sugg.len().min(4)]);

    let t = std::time::Instant::now();
    let info = extractor::extractor().extract("aqz-KE-bpKQ", &extractor::Request::video(None)).await?;
    println!("extract #1: {} formats ({:?})", info.formats.len(), t.elapsed());
    let t = std::time::Instant::now();
    let info2 = extractor::extractor().extract("jNQXAC9IVRw", &extractor::Request::video(None)).await?;
    println!("extract #2 (warm): {} formats, {} chapters, {} captions ({:?})", info2.formats.len(), info2.chapters.len(), info2.captions.len(), t.elapsed());
    Ok(info).map(|info| {
        let support = decoders::probe();
        let prefs = formats::Prefs {
            codec: crate::settings::CodecPref::Auto,
            max_height: 1080,
            allow_60fps: true,
            hw_decode: true,
            audio_only: false,
        };
        let sel = formats::select(&info, &support, &prefs, None);
        match sel {
            Ok(s) => println!(
                "selected video {:?} audio {:?}",
                s.video_format().map(|f| (&f.format_id, &f.vcodec, f.height)),
                s.audio_format().map(|f| (&f.format_id, &f.acodec))
            ),
            Err(e) => println!("selection failed: {e}"),
        }
        println!(
            "qualities: {:?}",
            formats::quality_options(&info, &support, &prefs)
                .iter()
                .map(|q| format!("{}{}", q.label(), if q.hardware { "(hw)" } else { "" }))
                .collect::<Vec<_>>()
        );
    })
}
