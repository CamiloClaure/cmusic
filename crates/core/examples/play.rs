//! Scriptable headless run through the real engine:
//! `cargo run --release -p cmusic-core --example play -- <step> <step> ...`
//!
//! Steps: `play:<id>[,<id>...]` `search:<query>` `wait:<secs>` `seek:<secs>` `pause` `resume`
//! `toggle` `next` `prev` `volume:<0-100>` `remove:<index>` `shuffle` `repeat`
//! `fetch:<liked|playlists|songs|albums|artists>` `quit`.
//! Legacy form `play -- <id> [secs]` still works. Every update is printed with elapsed seconds;
//! `TTFA <ms> <id>` is the time from a play/next/prev to the first Playing snapshot with
//! position > 0. Exit 3 with `HANG` when nothing arrives for 20 s while Playing or Loading.
//! Set `CMUSIC_STATE_DIR` to a scratch dir, `CMUSIC_AO=null` without audio, `CMUSIC_LOG=<file>`
//! for the engine log. Stream URLs are never printed.

use cmusic_core::engine::{start, Handle, Options};
use cmusic_core::{Command, Item, PageRequest, Status, Track, Update};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

#[derive(Default)]
struct Shared {
    last_update: Option<Instant>,
    status: Status,
    cur_id: Option<String>,
    /// TTFA timer: started at, and the track id that must change (None = any).
    pending: Option<(Instant, Option<String>)>,
}

type Sh = Arc<Mutex<Shared>>;

fn stamp(t0: Instant) -> String {
    format!("[{:7.3}]", t0.elapsed().as_secs_f64())
}

fn track(id: &str) -> Track {
    Track {
        video_id: id.into(),
        title: id.into(),
        artists: String::new(),
        album: None,
        duration_secs: None,
        thumbnail_url: None,
    }
}

fn request(kind: &str) -> Option<PageRequest> {
    Some(match kind {
        "liked" => PageRequest::LikedSongs,
        "playlists" => PageRequest::LibraryPlaylists,
        "songs" => PageRequest::LibrarySongs,
        "albums" => PageRequest::LibraryAlbums,
        "artists" => PageRequest::LibraryArtists,
        _ => return None,
    })
}

fn describe(u: &Update, t0: Instant, sh: &Sh) {
    let t = stamp(t0);
    match u {
        Update::Player(s) => {
            let cur = s.current();
            println!(
                "{t} player {:?} id={} title={:?} pos={:.1}/{:.1} queue={} idx={:?} err={:?}",
                s.status,
                cur.map_or("-", |c| c.video_id.as_str()),
                cur.map(|c| c.title.as_str()),
                s.position_secs,
                s.duration_secs,
                s.queue.len(),
                s.cursor,
                s.last_error,
            );
            let mut g = sh.lock().unwrap();
            g.status = s.status;
            let id = cur.map(|c| c.video_id.clone());
            if id != g.cur_id {
                g.cur_id = id.clone();
                if g.pending.is_none() && id.is_some() {
                    g.pending = Some((Instant::now(), None));
                }
            }
            if let Some((at, from)) = g.pending.clone() {
                let moved = from.is_none() || from != id;
                if moved && s.status == Status::Playing && s.position_secs > 0.0 {
                    println!(
                        "TTFA {} {}",
                        at.elapsed().as_millis(),
                        id.unwrap_or_default()
                    );
                    g.pending = None;
                }
            }
        }
        Update::Lyrics { video_id, lyrics } => println!(
            "{t} lyrics {video_id}: {}",
            lyrics
                .as_ref()
                .map_or("none".into(), |l| format!("{} lines", l.lines.len()))
        ),
        Update::Page { request, result } => match result {
            Ok(p) => println!("{t} page {request:?}: ok {} items", p.items.len()),
            Err(e) => println!("{t} page {request:?}: err {e}"),
        },
        Update::AuthRequired(m) => println!("{t} auth required: {m}"),
        Update::Notice(m) => println!("{t} notice: {m}"),
    }
}

struct Script {
    h: Handle,
    sh: Sh,
    pages: UnboundedReceiver<Update>,
    t0: Instant,
}

impl Script {
    fn begin_play(&self) {
        self.sh.lock().unwrap().pending = Some((Instant::now(), None));
    }

    fn begin_change(&self) {
        let mut g = self.sh.lock().unwrap();
        let from = g.cur_id.clone();
        g.pending = Some((Instant::now(), from));
    }

    /// Waits up to 30 s for the page of `req`.
    async fn page(&mut self, req: &PageRequest) -> Option<Result<cmusic_core::Page, String>> {
        let end = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(end);
        loop {
            tokio::select! {
                Some(Update::Page { request, result }) = self.pages.recv() => {
                    if &request == req { return Some(result); }
                }
                _ = &mut end => return None,
            }
        }
    }

    async fn run(&mut self, step: &str) -> Result<bool, String> {
        let (name, arg) = step.split_once(':').unwrap_or((step, ""));
        let status = self.sh.lock().unwrap().status;
        match name {
            "play" => {
                let tracks: Vec<Track> = arg
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(track)
                    .collect();
                self.begin_play();
                self.h.send(Command::PlayTracks { tracks, start: 0 });
            }
            "search" => {
                let req = PageRequest::Search(arg.into());
                self.h.send(Command::Fetch(req.clone()));
                let page = self.page(&req).await.ok_or("search: no page in 30 s")??;
                let tracks: Vec<Track> = page
                    .items
                    .into_iter()
                    .filter_map(|i| {
                        if let Item::Track(t) = i {
                            Some(t)
                        } else {
                            None
                        }
                    })
                    .collect();
                if tracks.is_empty() {
                    return Err("search: no track results".into());
                }
                self.begin_play();
                self.h.send(Command::PlayTracks { tracks, start: 0 });
            }
            "fetch" => {
                let req = request(arg).ok_or(format!("unknown fetch kind {arg:?}"))?;
                self.h.send(Command::Fetch(req.clone()));
                match self.page(&req).await {
                    Some(Ok(p)) => {
                        println!("{} fetch:{arg} {} items", stamp(self.t0), p.items.len())
                    }
                    Some(Err(e)) => println!("{} fetch:{arg} error: {e}", stamp(self.t0)),
                    None => println!("{} fetch:{arg} timeout", stamp(self.t0)),
                }
            }
            "wait" => {
                let s: f64 = arg.parse().map_err(|_| format!("bad wait {arg:?}"))?;
                tokio::time::sleep(Duration::from_secs_f64(s)).await;
            }
            "seek" => {
                let s: f64 = arg.parse().map_err(|_| format!("bad seek {arg:?}"))?;
                self.h.send(Command::SeekTo(s));
            }
            // ponytail: no SetPause command exists, so pause/resume toggle only when the last status says so.
            "pause" if matches!(status, Status::Playing | Status::Buffering) => {
                self.h.send(Command::TogglePause)
            }
            "resume" if status == Status::Paused => self.h.send(Command::TogglePause),
            "pause" | "resume" => println!("{} {name}: skipped, status {status:?}", stamp(self.t0)),
            "toggle" => self.h.send(Command::TogglePause),
            "next" => {
                self.begin_change();
                self.h.send(Command::Next);
            }
            "prev" => {
                self.begin_change();
                self.h.send(Command::Prev);
            }
            "volume" => self
                .h
                .send(Command::SetVolume(arg.parse().map_err(|_| "bad volume")?)),
            "remove" => self.h.send(Command::RemoveFromQueue(
                arg.parse().map_err(|_| "bad index")?,
            )),
            "shuffle" => self.h.send(Command::ToggleShuffle),
            "repeat" => self.h.send(Command::CycleRepeat),
            "quit" => return Ok(false),
            _ => return Err(format!("unknown step {step:?}")),
        }
        Ok(true)
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut steps: Vec<String> = std::env::args().skip(1).collect();
    if steps.is_empty() {
        return Err("usage: play <step> ... (see the file header)".into());
    }
    // Legacy form: `<id> [secs]`.
    if !steps[0].contains(':')
        && !matches!(
            steps[0].as_str(),
            "pause" | "resume" | "toggle" | "next" | "prev" | "shuffle" | "repeat" | "quit"
        )
    {
        let secs = steps.get(1).cloned().unwrap_or_else(|| "20".into());
        steps = vec![format!("play:{}", steps[0]), format!("wait:{secs}")];
    }

    let t0 = Instant::now();
    let sh: Sh = Arc::default();
    let (h, mut rx) = start(Options::default())?;

    // Watchdog on its own thread, so it fires even when the engine or this runtime is stuck.
    let w = sh.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(500));
        let g = w.lock().unwrap();
        let quiet = g
            .last_update
            .is_some_and(|t| t.elapsed() > Duration::from_secs(20));
        if quiet && matches!(g.status, Status::Playing | Status::Loading) {
            println!("{} HANG", stamp(t0));
            std::process::exit(3);
        }
    });

    let (pages_tx, pages) = unbounded_channel();
    let printer = {
        let sh = sh.clone();
        tokio::spawn(async move {
            while let Some(u) = rx.recv().await {
                sh.lock().unwrap().last_update = Some(Instant::now());
                describe(&u, t0, &sh);
                if matches!(u, Update::Page { .. }) {
                    let _ = pages_tx.send(u);
                }
            }
        })
    };

    let mut script = Script {
        h: h.clone(),
        sh,
        pages,
        t0,
    };
    let mut code = 0;
    for step in &steps {
        println!("{} -- {step}", stamp(t0));
        match script.run(step).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                println!("{} step failed: {e}", stamp(t0));
                code = 1;
                break;
            }
        }
    }
    h.send(Command::Quit);
    let _ = tokio::time::timeout(Duration::from_secs(10), printer).await;
    println!("{} -- engine stopped", stamp(t0));
    std::process::exit(code);
}
