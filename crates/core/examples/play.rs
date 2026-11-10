//! Headless end-to-end check: `cargo run -p cmusic-core --example play -- <video_id> [seconds]`.
//! Plays one track through the real engine, prints every update, quits after `seconds` (20).
//! Set `CMUSIC_STATE_DIR` to a scratch dir to keep your saved session untouched, and
//! `CMUSIC_AO=null` on a box without audio.

use cmusic_core::engine::{start, Options};
use cmusic_core::{Command, Track, Update};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let video_id = args.next().ok_or("usage: play <video_id> [seconds]")?;
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);

    let (handle, mut rx) = start(Options::default())?;
    handle.send(Command::PlayTracks {
        tracks: vec![Track {
            video_id: video_id.clone(),
            title: video_id,
            artists: String::new(),
            album: None,
            duration_secs: None,
            thumbnail_url: None,
        }],
        start: 0,
    });

    let quit_at = tokio::time::sleep(Duration::from_secs(secs));
    tokio::pin!(quit_at);
    let mut quit_sent = false;
    loop {
        tokio::select! {
            u = rx.recv() => match u {
                Some(u) => print(&u),
                None => break,
            },
            _ = &mut quit_at, if !quit_sent => {
                println!("-- quit");
                handle.send(Command::Quit);
                quit_sent = true;
            }
        }
    }
    println!("-- engine stopped");
    Ok(())
}

fn print(u: &Update) {
    match u {
        Update::Player(s) => println!(
            "player {:?} cursor={:?}/{} pos={:.1}/{:.1} title={:?} err={:?}",
            s.status,
            s.cursor,
            s.queue.len(),
            s.position_secs,
            s.duration_secs,
            s.current().map(|t| t.title.as_str()),
            s.last_error,
        ),
        Update::Lyrics { video_id, lyrics } => println!(
            "lyrics {video_id}: {}",
            lyrics
                .as_ref()
                .map_or("none".into(), |l| format!("{} lines", l.lines.len()))
        ),
        Update::Page { request, result } => println!("page {request:?}: ok={}", result.is_ok()),
        Update::AuthRequired(m) => println!("auth required: {m}"),
        Update::Notice(m) => println!("notice: {m}"),
    }
}
